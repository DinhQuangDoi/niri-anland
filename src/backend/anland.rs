use std::collections::HashMap;
use std::ffi::CString;
use std::mem;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use niri_config::OutputName;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Fourcc;
use smithay::backend::input::{Axis, ButtonState, InputEvent as SmithayInputEvent, KeyState};
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, DebugFlags, ImportDma, Renderer};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{
    EventSource, Interest, Mode as PollMode, Poll, PostAction, Readiness, RegistrationToken,
    Result as CalloopResult, Token, TokenFactory,
};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::Size;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::presentation::Refresh;
use smithay::wayland::selection::data_device::set_data_device_selection;

use anland_sys::*;

use super::anland_input::{
    AnlandInput, AnlandKeyboardEvent, AnlandPointerAxisEvent, AnlandPointerButtonEvent,
    AnlandPointerMotionEvent, AnlandTouchCancelEvent, AnlandTouchDownEvent, AnlandTouchFrameEvent,
    AnlandTouchMotionEvent, AnlandTouchUpEvent, AnlandVirtualDevice,
};
use super::{IpcOutputMap, OutputId, RenderResult};
use crate::niri::{Niri, RedrawState, State};
use crate::render_helpers::{resources, shaders, RenderCtx, RenderTarget};
use crate::utils::{get_monotonic_time, logical_output};

// ---------------------------------------------------------------------------
// Calloop event source for polling raw file descriptors
// ---------------------------------------------------------------------------

struct FdEventSource {
    fd: RawFd,
}

impl FdEventSource {
    fn new(fd: RawFd) -> Self {
        Self { fd }
    }
}

impl EventSource for FdEventSource {
    type Event = ();
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        _readiness: Readiness,
        _token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        callback((), &mut ());
        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> CalloopResult<()> {
        let token = token_factory.token();
        unsafe {
            let fd = BorrowedFd::borrow_raw(self.fd);
            poll.register(fd, Interest::READ, PollMode::Level, token)
        }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> CalloopResult<()> {
        let token = token_factory.token();
        unsafe {
            let fd = BorrowedFd::borrow_raw(self.fd);
            poll.reregister(fd, Interest::READ, PollMode::Level, token)
        }
    }

    fn unregister(&mut self, poll: &mut Poll) -> CalloopResult<()> {
        unsafe {
            let fd = BorrowedFd::borrow_raw(self.fd);
            poll.unregister(fd)
        }
    }
}

// ---------------------------------------------------------------------------
// Anland Backend
// ---------------------------------------------------------------------------

pub struct Anland {
    ctx: AnlandContext,
    _socket_path: String,

    renderer: GlesRenderer,
    output: Option<Output>,
    damage_tracker: Option<OutputDamageTracker>,
    dmabuf_global: Option<DmabufGlobal>,

    dmabufs: Vec<Dmabuf>,

    // Frame counter and, per dmabuf slot, the frame number it was last
    // rendered into (-1 = never rendered). The consumer cycles its BufferQueue
    // slots (here 4), so a reused slot holds content from
    // `frame_count - last_frame_per_buffer[i]` frames ago; reporting that as
    // the buffer age lets the damage tracker repaint exactly the regions that
    // changed since, instead of doing a full repaint every frame (which drops
    // the refresh rate).
    frame_count: u64,
    last_frame_per_buffer: Vec<i64>,

    // Frame timing for debugging
    frame_times: std::collections::VecDeque<u64>,
    // Present-interval debugging: wall-clock gap between successive render() calls.
    last_render_at: Option<Instant>,
    present_intervals: std::collections::VecDeque<u64>,

    reconnect_timer_token: Option<RegistrationToken>,
    buf_ready_source_token: Option<RegistrationToken>,
    data_source_token: Option<RegistrationToken>,

    ipc_outputs: Arc<Mutex<IpcOutputMap>>,

    debug_tint: bool,

    // Clipboard text received from the Android consumer (anland INPUT_TYPE_CLIPBOARD),
    // staged for the event loop to adopt as the Wayland selection.
    pending_clipboard: Option<Vec<u8>>,

    // Rotation angle in degrees CCW (0/90/180/270) received from the Android
    // consumer (anland INPUT_TYPE_DISPLAY_ROTATION), staged for the event loop
    // to apply as the anland output transform.
    pending_rotation: Option<u32>,
}

impl Anland {
    pub fn new(socket_path: String) -> anyhow::Result<Self> {
        let _span = tracy_client::span!("Anland::new");

        let c_path = CString::new(socket_path.as_str())
            .map_err(|_| anyhow::anyhow!("socket path contains null byte"))?;

        let ctx = AnlandContext::connect(&c_path)
            .map_err(|e| anyhow::anyhow!("anland connect failed: {e}"))?;

        let display =
            unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }
                .context("error creating EGL display")?;
        let context =
            EGLContext::new(&display).context("error creating EGL context")?;
        let renderer =
            unsafe { GlesRenderer::new(context) }.context("error creating renderer")?;

        Ok(Self {
            ctx,
            _socket_path: socket_path,
            renderer,
            output: None,
            damage_tracker: None,
            dmabuf_global: None,
            dmabufs: Vec::new(),
            reconnect_timer_token: None,
            buf_ready_source_token: None,
            data_source_token: None,
            ipc_outputs: Arc::new(Mutex::new(HashMap::new())),
            debug_tint: false,
            pending_clipboard: None,
            pending_rotation: None,
            frame_count: 0,
            last_frame_per_buffer: Vec::new(),
            frame_times: std::collections::VecDeque::new(),
            last_render_at: None,
            present_intervals: std::collections::VecDeque::new(),
        })
    }

    pub fn init(&mut self, niri: &mut Niri) {
        let _span = tracy_client::span!("Anland::init");

        let info = self.ctx.screen_info();
        let (w, h) = (info.width as i32, info.height as i32);
        let refresh = info.refresh as i32;
        let output = Output::new(
            "anland".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Anland".into(),
                model: "Android".into(),
                serial_number: "0".into(),
            },
        );

        let mode = Mode {
            size: Size::from((w, h)),
            refresh,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        output.user_data().insert_if_missing(|| OutputName {
            connector: "anland".to_string(),
            make: Some("Anland".to_string()),
            model: Some("Android".to_string()),
            serial: None,
        });

        let physical_properties = output.physical_properties();
        let mut ipc = self.ipc_outputs.lock().unwrap();
        ipc.insert(
            OutputId::next(),
            niri_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None,
                physical_size: None,
                modes: vec![niri_ipc::Mode {
                    width: w as u16,
                    height: h as u16,
                    refresh_rate: info.refresh,
                    is_preferred: true,
                }],
                current_mode: Some(0),
                is_custom_mode: true,
                vrr_supported: false,
                vrr_enabled: false,
                logical: Some(logical_output(&output)),
            },
        );
        drop(ipc);

        resources::init(&mut self.renderer);
        shaders::init(&mut self.renderer);

        self.create_dmabuf_global(niri);

        self.damage_tracker = Some(OutputDamageTracker::from_output(&output));
        self.output = Some(output.clone());
        // Pace niri to the display refresh instead of free-running. info.refresh is
        // in mHz (e.g. 120000 for 120 Hz); the frame clock + estimated-vblank timer
        // then advance animations in even refresh-interval steps.
        let refresh_interval = if info.refresh > 0 {
            Some(Duration::from_nanos(
                1_000_000_000_000u64 / info.refresh as u64,
            ))
        } else {
            None
        };
        niri.add_output(output, refresh_interval, false);

        self.start_reconnect_timer(niri);
    }

    fn create_dmabuf_global(&mut self, niri: &mut Niri) {
        let default_feedback = || -> anyhow::Result<DmabufFeedback> {
            let display = self.renderer.egl_context().display();
            let device = smithay::backend::egl::EGLDevice::device_for_display(display)
                .context("error getting EGL device")?;
            let node = device
                .try_get_render_node()
                .context("error getting EGL device render node")?
                .context("failed to query EGL device render node")?;

            let primary_formats = self.renderer.dmabuf_formats();
            DmabufFeedbackBuilder::new(node.dev_id(), primary_formats)
                .build()
                .context("error building dmabuf feedback")
        };

        let dmabuf_global = match default_feedback() {
            Ok(feedback) => niri
                .dmabuf_state
                .create_global_with_default_feedback::<State>(
                    &niri.display_handle,
                    &feedback,
                ),
            Err(err) => {
                debug!("failed building dmabuf feedback, falling back to v3: {err:?}");
                let primary_formats = self.renderer.dmabuf_formats();
                niri
                    .dmabuf_state
                    .create_global::<State>(&niri.display_handle, primary_formats)
            }
        };
        self.dmabuf_global = Some(dmabuf_global);
    }

    // -------------------------------------------------------------------
    // Reconnect
    // -------------------------------------------------------------------

    fn start_reconnect_timer(&mut self, niri: &mut Niri) {
        let timer = Timer::from_duration(Duration::from_millis(200));
        if let Ok(token) = niri.event_loop.insert_source(
            timer,
            move |_, _, state| {
                let connected = {
                    let anland = state.backend.anland();
                    let was_fallback = anland.ctx.is_fallback();
                    anland.try_reconnect(&mut state.niri);
                    was_fallback && !anland.ctx.is_fallback()
                };
                if connected {
                    // Make the touch seat exist so subsequent touch events
                    // aren't dropped before the first wl_touch client binds.
                    state.process_input_event::<AnlandInput>(SmithayInputEvent::DeviceAdded {
                        device: AnlandVirtualDevice,
                    });
                }
                TimeoutAction::ToDuration(Duration::from_millis(200))
            },
        ) {
            self.reconnect_timer_token = Some(token);
        }
    }

    fn try_reconnect(&mut self, niri: &mut Niri) {
        if !self.ctx.is_fallback() {
            return;
        }
        if self.ctx.try_exit_fallback().is_ok() {
            self.on_connected(niri);
        }
    }

    fn on_connected(&mut self, niri: &mut Niri) {
        let _span = tracy_client::span!("Anland::on_connected");

        let count = self.ctx.buffer_count() as usize;
        if count == 0 {
            warn!("connected but got 0 dmabufs");
            return;
        }

        self.dmabufs.clear();
        self.last_frame_per_buffer.clear();
        self.frame_count = 0;

        for i in 0..count {
            let raw_fd = self.ctx.dmabuf_fd_at(i as i32);
            if raw_fd < 0 {
                continue;
            }
            let info = match self.ctx.dmabuf_info_at(i as i32) {
                Some(info) => info,
                None => continue,
            };
            match self.import_raw_dmabuf(raw_fd, &info) {
                Ok(dmabuf) => self.dmabufs.push(dmabuf),
                Err(e) => warn!("failed to import dmabuf {}: {e:?}", i),
            }
        }

        self.last_frame_per_buffer.resize(self.dmabufs.len(), -1);

        info!(
            "connected to anland consumer: {} buffers, {}x{}",
            self.dmabufs.len(),
            self.ctx.screen_info().width,
            self.ctx.screen_info().height,
        );

        self.update_output_mode();

        self.register_buffer_ready_source(niri);
        self.register_input_source(niri);
    }

    /*
     * The output may have been created with the fallback screen (no consumer was
     * connected when the compositor booted). Once the consumer is up, refresh the
     * output mode with its real screen size.
     */
    fn update_output_mode(&mut self) {
        let (w, h) = (
            self.ctx.screen_info().width as i32,
            self.ctx.screen_info().height as i32,
        );
        let refresh = self.ctx.screen_info().refresh as i32;

        let Some(output) = self.output.clone() else { return };

        let changed = match output.current_mode() {
            Some(m) => m.size.w != w || m.size.h != h || m.refresh != refresh,
            None => true,
        };
        if !changed {
            return;
        }

        info!("anland consumer screen is now {}x{}", w, h);

        let mode = Mode {
            size: Size::from((w, h)),
            refresh,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        let mut ipc = self.ipc_outputs.lock().unwrap();
        if let Some(ipc_output) = ipc.values_mut().find(|o| o.name == output.name()) {
            ipc_output.modes = vec![niri_ipc::Mode {
                width: w as u16,
                height: h as u16,
                refresh_rate: self.ctx.screen_info().refresh,
                is_preferred: true,
            }];
            ipc_output.current_mode = Some(0);
            ipc_output.logical = Some(logical_output(&output));
        }
    }

    fn import_raw_dmabuf(
        &mut self,
        raw_fd: RawFd,
        info: &anland_sys::buf_info,
    ) -> anyhow::Result<Dmabuf> {
        // Take our own duplicate of the fd. The C producer context retains the
        // original in ctx->dmabuf_fds and closes it on disconnect/reconnect; if we
        // took ownership of the same fd directly, our Dmabuf would keep a now-closed
        // descriptor after a reconnect and every subsequent bind would fail with
        // EBADF (black screen), while releasing it here would double-close C's copy.
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
        let owned_fd = borrowed_fd
            .try_clone_to_owned()
            .context("failed to duplicate dmabuf fd")?;

        let fourcc = protocol_format_to_fourcc(info.format);

        let mut builder = Dmabuf::builder(
            Size::from((info.width as i32, info.height as i32)),
            fourcc,
            smithay::reexports::gbm::Modifier::from(info.modifier),
            smithay::backend::allocator::dmabuf::DmabufFlags::empty(),
        );

        builder.add_plane(owned_fd, 0, info.offset, info.stride);

        builder
            .build()
            .context("failed to build Dmabuf from raw fd")
    }

    // -------------------------------------------------------------------
    // Event sources
    // -------------------------------------------------------------------

    fn register_buffer_ready_source(&mut self, niri: &mut Niri) {
        // A previous consumer connection may have left a source registered on an
        // fd that is now closed (and possibly reused). Re-registering without
        // removing it would leave two sources polling the same fd — the first
        // drains the eventfd and the second blocks on it, stalling the loop.
        if let Some(token) = self.buf_ready_source_token.take() {
            let _ = niri.event_loop.remove(token);
        }
        let fd = self.ctx.buffer_ready_fd();
        if fd < 0 {
            return;
        }
        let source = FdEventSource::new(fd);
        if let Ok(token) = niri.event_loop.insert_source(source, move |_, _, state| {
            let anland = state.backend.anland();
            let fd = anland.ctx.buffer_ready_fd();
            let mut val: u64 = 0;
            unsafe {
                libc::read(
                    fd,
                    &mut val as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
            if let Some(output) = anland.output.clone() {
                state.niri.queue_redraw(&output);
            }
        }) {
            self.buf_ready_source_token = Some(token);
        }
    }

    fn register_input_source(&mut self, niri: &mut Niri) {
        // See register_buffer_ready_source: drop the previous connection's source
        // before installing a fresh one on the new consumer's data fd.
        if let Some(token) = self.data_source_token.take() {
            let _ = niri.event_loop.remove(token);
        }
        let fd = self.ctx.data_fd();
        if fd < 0 {
            return;
        }
        let source = FdEventSource::new(fd);
        if let Ok(token) = niri.event_loop.insert_source(source, move |_, _, state| {
            // Non-blocking drain: with a 16ms poll here the callback would hold the
            // event loop while a motion stream is active (events arrive every ~8ms),
            // starving the buffer_ready source so the consumer gets no render-done
            // fence until the gesture ends -> sparse frames and 5s fallback timeouts.
            let events = {
                let anland = state.backend.anland();
                anland.poll_input(0)
            };
            for event in events {
                state.process_input_event(event);
            }
            // Adopt clipboard text copied on Android as the Wayland selection so
            // compositor-local clients can paste it. Safe to touch Wayland state here:
            // we are on the main event loop.
            if let Some(data) = state.backend.anland().take_pending_clipboard() {
                set_data_device_selection(
                    &state.niri.display_handle,
                    &state.niri.seat,
                    vec![String::from("text/plain")],
                    Arc::from(data),
                );
            }
            // Auto-rotate: the consumer reported the Android display rotation;
            // mirror it onto the anland output through the same transient-config
            // path `niri msg output anland transform <angle>` uses.
            if let Some(angle_deg) = state.backend.anland().take_pending_rotation() {
                let transform = match angle_deg {
                    90 => niri_ipc::Transform::_90,
                    180 => niri_ipc::Transform::_180,
                    270 => niri_ipc::Transform::_270,
                    _ => niri_ipc::Transform::Normal,
                };
                info!("applying anland display rotation {} deg", angle_deg);
                state.apply_transient_output_config(
                    "anland",
                    niri_ipc::OutputAction::Transform { transform },
                );
            }
        }) {
            self.data_source_token = Some(token);
        }
    }

    // -------------------------------------------------------------------
    // Input dispatch
    // -------------------------------------------------------------------

    /// Take the clipboard text staged by an INPUT_TYPE_CLIPBOARD event, if any.
    /// The event loop adopts it as the Wayland selection after polling.
    pub fn take_pending_clipboard(&mut self) -> Option<Vec<u8>> {
        self.pending_clipboard.take()
    }

    /// Take the rotation angle staged by an INPUT_TYPE_DISPLAY_ROTATION event,
    /// if any. The event loop applies it as the output transform after polling.
    pub fn take_pending_rotation(&mut self) -> Option<u32> {
        self.pending_rotation.take()
    }

    /// Push a text clipboard update to the Android consumer.
    pub fn push_clipboard(&mut self, text: &[u8]) {
        let event = OutputEvent {
            type_: OUTPUT_TYPE_CLIPBOARD,
            clipboard: OutputClipboard {
                size: text.len() as u32,
            },
        };
        self.ctx.push_output_event_with_length(&event, text);
    }

    /// Poll the daemon for queued input and return the translated smithay
    /// events. Non-input notifications (display refresh, clipboard) are
    /// handled internally and not forwarded.
    fn poll_input(&mut self, timeout: i32) -> Vec<SmithayInputEvent<AnlandInput>> {
        let mut out = Vec::new();
        loop {
            let Some(event) = self.ctx.poll_input_event(timeout) else {
                break;
            };
            if self.handle_special_event(&event) {
                continue;
            }
            match self.to_smithay_event(&event) {
                Some(smithay_event) => out.push(smithay_event),
                None => self.ctx.handle_unhandled_event(&event),
            }
        }
        out
    }

    fn screen_size(&self) -> (f64, f64) {
        let info = self.ctx.screen_info();
        (info.width as f64, info.height as f64)
    }

    fn handle_special_event(&mut self, event: &InputEvent) -> bool {
        let u = unsafe {
            let u: InputEventUnion = std::mem::zeroed();
            let mut u = u;
            std::ptr::copy_nonoverlapping(
                &event.touch as *const InputTouch as *const u8,
                &mut u as *mut InputEventUnion as *mut u8,
                std::mem::size_of::<InputEventUnion>(),
            );
            u
        };

        match event.type_ {
            INPUT_TYPE_DISPLAY_REFRESH => {
                let d = unsafe { u.display };
                debug!("display refresh: {} mHz", d.refresh_mhz);
                true
            }
            INPUT_TYPE_DISPLAY_ROTATION => {
                let r = unsafe { u.display_rotation };
                info!("display rotation: {} deg", r.angle_deg);
                self.pending_rotation = Some(r.angle_deg);
                true
            }
            INPUT_TYPE_CLIPBOARD => {
                let c = unsafe { u.clipboard };
                if c.size > 0 {
                    let mut buf = vec![0u8; c.size as usize];
                    self.ctx.poll_input_event_extend_data(&mut buf, 1000);
                    // Adopt as the Wayland selection on the main event loop so
                    // compositor-local clients can paste what was copied on Android.
                    self.pending_clipboard = Some(buf);
                }
                true
            }
            _ => false,
        }
    }

    /// Translate a raw Anland input event into a smithay input event niri can
    /// process. Returns `None` for events that carry no input (unknown or
    /// already handled as special).
    fn to_smithay_event(&self, event: &InputEvent) -> Option<SmithayInputEvent<AnlandInput>> {
        let u = unsafe {
            let u: InputEventUnion = std::mem::zeroed();
            let mut u = u;
            std::ptr::copy_nonoverlapping(
                &event.touch as *const InputTouch as *const u8,
                &mut u as *mut InputEventUnion as *mut u8,
                std::mem::size_of::<InputEventUnion>(),
            );
            u
        };

        let time = get_monotonic_time().as_micros() as u64;

        match event.type_ {
            INPUT_TYPE_TOUCH => {
                let t = unsafe { u.touch };
                debug!(
                    "touch: action={} x={} y={} id={}",
                    t.action, t.x, t.y, t.pointer_id
                );
                let slot = t.pointer_id.max(0) as u32;
                let (screen_w, screen_h) = self.screen_size();
                match t.action {
                    0 => Some(SmithayInputEvent::TouchDown {
                        event: AnlandTouchDownEvent {
                            time,
                            slot,
                            x: t.x as f64,
                            y: t.y as f64,
                            screen_w,
                            screen_h,
                        },
                    }),
                    1 => Some(SmithayInputEvent::TouchUp {
                        event: AnlandTouchUpEvent { time, slot },
                    }),
                    2 => Some(SmithayInputEvent::TouchMotion {
                        event: AnlandTouchMotionEvent {
                            time,
                            slot,
                            x: t.x as f64,
                            y: t.y as f64,
                            screen_w,
                            screen_h,
                        },
                    }),
                    _ => None,
                }
            }
            INPUT_TYPE_KEY => {
                let k = unsafe { u.key };
                debug!("key: action={} keycode={}", k.action, k.keycode);
                Some(SmithayInputEvent::Keyboard {
                    event: AnlandKeyboardEvent {
                        time,
                        key_code: k.keycode.max(0) as u32,
                        state: match k.action {
                            0 => KeyState::Pressed,
                            _ => KeyState::Released,
                        },
                    },
                })
            }
            INPUT_TYPE_POINTER_MOTION => {
                let m = unsafe { u.pointer_motion };
                debug!(
                    "pointer motion: x={} y={} dx={} dy={}",
                    m.x, m.y, m.dx, m.dy
                );
                let (screen_w, screen_h) = self.screen_size();
                Some(SmithayInputEvent::PointerMotionAbsolute {
                    event: AnlandPointerMotionEvent {
                        time,
                        x: m.x as f64,
                        y: m.y as f64,
                        screen_w,
                        screen_h,
                    },
                })
            }
            INPUT_TYPE_POINTER_BUTTON => {
                let b = unsafe { u.pointer_button };
                debug!("pointer button: button={} pressed={}", b.button, b.pressed);
                Some(SmithayInputEvent::PointerButton {
                    event: AnlandPointerButtonEvent {
                        time,
                        button_code: b.button,
                        state: if b.pressed != 0 {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                    },
                })
            }
            INPUT_TYPE_POINTER_AXIS => {
                let a = unsafe { u.pointer_axis };
                debug!(
                    "pointer axis: axis={} value={} discrete={}",
                    a.axis, a.value, a.discrete
                );
                Some(SmithayInputEvent::PointerAxis {
                    event: AnlandPointerAxisEvent {
                        time,
                        axis: if a.axis == 0 {
                            Axis::Vertical
                        } else {
                            Axis::Horizontal
                        },
                        value: a.value as f64,
                        discrete: a.discrete,
                    },
                })
            }
            INPUT_TYPE_TOUCH_FRAME => Some(SmithayInputEvent::TouchFrame {
                event: AnlandTouchFrameEvent { time },
            }),
            _ => None,
        }
    }

    // -------------------------------------------------------------------
    // Render
    // -------------------------------------------------------------------

    pub fn render(
        &mut self,
        niri: &mut Niri,
        output: &Output,
        target_presentation_time: Duration,
    ) -> RenderResult {
        let _span = tracy_client::span!("Anland::render");
        let frame_start = Instant::now();

        if self.ctx.is_fallback() {
            return RenderResult::Skipped;
        }

        let idx = self.ctx.selected_buffer_index();
        if idx < 0 || idx as usize >= self.dmabufs.len() {
            return RenderResult::Skipped;
        }

        // The consumer cycles its BufferQueue slots (here 4), so a reused slot
        // holds the frame rendered `frame_count - last_frame_per_buffer[idx]`
        // frames ago. Report that as the buffer age so the damage tracker
        // repaints exactly the regions that changed in between. Age 0 (a
        // fresh/reused buffer we cannot reconstruct) makes the tracker repaint
        // the whole frame, which keeps correctness at the cost of a full draw.
        let last = self.last_frame_per_buffer[idx as usize];
        let age = if last >= 0 {
            (self.frame_count - last as u64) as usize
        } else {
            0
        };
        self.last_frame_per_buffer[idx as usize] = self.frame_count as i64;
        self.frame_count = self.frame_count.wrapping_add(1);

        let ctx = RenderCtx {
            renderer: &mut self.renderer,
            target: RenderTarget::Output,
            xray: None,
        };
        let elements = niri.render_to_vec(ctx, output, true);

        let mut target = match self.renderer.bind(&mut self.dmabufs[idx as usize]) {
            Ok(t) => t,
            Err(e) => {
                warn!("error binding dmabuf: {e:?}");
                return RenderResult::Skipped;
            }
        };

        let damage_tracker = self.damage_tracker.as_mut().unwrap();
        let mut res = match damage_tracker.render_output(
            &mut self.renderer,
            &mut target,
            age,
            &elements,
            [0.0, 0.0, 0.0, 1.0],
        ) {
            Ok(r) => r,
            Err(e) => {
                warn!("render error: {e:?}");
                return RenderResult::Skipped;
            }
        };

        // The consumer hands out a rotating pool of buffers, so the slot we
        // just got may hold pixels from an older frame. When the damage
        // tracker reports nothing to redraw it skips rendering entirely and
        // `res.damage` is None, which would leave us presenting stale content
        // -> blink. Re-render the whole frame (age 0) so the buffer is never
        // stale.
        if res.damage.is_none() {
            res = match damage_tracker.render_output(
                &mut self.renderer,
                &mut target,
                0,
                &elements,
                [0.0, 0.0, 0.0, 1.0],
            ) {
                Ok(r) => r,
                Err(e) => {
                    warn!("render error: {e:?}");
                    return RenderResult::Skipped;
                }
            };
        }

        if let Err(err) = res.sync.wait() {
            warn!("error waiting for frame completion: {err:?}");
        }

        // Release the renderer bind (GlesTarget borrows &mut self.renderer) before
        // we take &mut self again for queue_estimated_vblank_timer below.
        drop(target);

        niri.update_primary_scanout_output(output, &res.states);

        self.ctx.set_render_fence(-1);
        self.ctx.trigger_refresh();

        let mut presentation_feedbacks =
            niri.take_presentation_feedbacks(output, &res.states);
        let now = get_monotonic_time();
        presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
            now,
            Refresh::Unknown,
            0,
            wp_presentation_feedback::Kind::empty(),
        );

        // Feed the frame clock so next_presentation_time() advances in refresh-rate
        // steps. Without this, niri free-runs (renders the instant the consumer
        // frees a buffer) at 150-300 fps into a 120 Hz panel -> uneven pacing that
        // looks like judder/low-Hz. With a real presentation time, animations step
        // at the display cadence.
        //
        // NB: do NOT reset redraw_state here; queue_estimated_vblank_timer() below
        // consumes it (it expects Queued / WaitingForEstimatedVBlank*), so leaving
        // it as-is is required.
        let output_state = niri.output_state.get_mut(output).unwrap();
        output_state.frame_clock.presented(now);
        output_state.frame_callback_sequence =
            output_state.frame_callback_sequence.wrapping_add(1);

        // Pace to the display refresh: queue a timer at the predicted next vblank.
        // redraw_queued_outputs only redraws outputs in Queued state, so parking in
        // WaitingForEstimatedVBlank throttles the free-running render loop to the
        // panel's refresh interval. buffer_ready / input still promote to
        // ...AndQueued so we never miss a needed frame.
        self.queue_estimated_vblank_timer(niri, output.clone(), target_presentation_time);

        let frame_time_ms = frame_start.elapsed().as_millis() as u64;
        self.frame_times.push_back(frame_time_ms);
        if self.frame_times.len() > 60 {
            self.frame_times.pop_front();
        }
        // Present interval = wall-clock gap between successive render() calls. This
        // is the perceived frame rate (unlike frame_time, which is only how long
        // our render() body took). A large gap here with a tiny frame_time means
        // niri is not being *asked* to redraw often enough (scheduling), not that
        // rendering is slow.
        let now = Instant::now();
        if let Some(prev) = self.last_render_at {
            let gap_ms = now.duration_since(prev).as_millis() as u64;
            self.present_intervals.push_back(gap_ms);
            if self.present_intervals.len() > 60 {
                self.present_intervals.pop_front();
            }
        }
        self.last_render_at = Some(now);
        if self.frame_count % 60 == 0 && !self.frame_times.is_empty() {
            let avg: u64 = self.frame_times.iter().sum::<u64>() / self.frame_times.len() as u64;
            let max = *self.frame_times.iter().max().unwrap_or(&0);
            let min = *self.frame_times.iter().min().unwrap_or(&0);
            let (iavg, imax, imin) = if !self.present_intervals.is_empty() {
                let s: u64 = self.present_intervals.iter().sum();
                (
                    s / self.present_intervals.len() as u64,
                    *self.present_intervals.iter().max().unwrap_or(&0),
                    *self.present_intervals.iter().min().unwrap_or(&0),
                )
            } else {
                (0, 0, 0)
            };
            info!(
                "frame timing: render avg={}ms max={}ms min={}ms | present-interval avg={}ms max={}ms min={}ms (~{}fps presented)",
                avg,
                max,
                min,
                iavg,
                imax,
                imin,
                1000 / iavg.max(1)
            );
        }

        RenderResult::Submitted
    }

    // Pace the free-running render loop to the display refresh. After a render we
    // park redraw_state in WaitingForEstimatedVBlank so redraw_queued_outputs()
    // won't immediately re-render; a timer fires at the predicted next vblank and
    // either re-queues (if an animation is ongoing) or just sends frame callbacks.
    fn queue_estimated_vblank_timer(
        &mut self,
        niri: &mut Niri,
        output: Output,
        target_presentation_time: Duration,
    ) {
        let output_state = niri.output_state.get_mut(&output).unwrap();
        match mem::take(&mut output_state.redraw_state) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(token)
            | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
                output_state.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
                return;
            }
        }

        let now = get_monotonic_time();
        let mut duration = target_presentation_time.saturating_sub(now);
        if duration.is_zero() {
            duration += output_state
                .frame_clock
                .refresh_interval()
                .unwrap_or(Duration::from_micros(8_333));
        }

        let timer = Timer::from_duration(duration);
        let token = niri
            .event_loop
            .insert_source(timer, move |_, _, state| {
                state
                    .backend
                    .anland()
                    .on_estimated_vblank_timer(&mut state.niri, output.clone());
                TimeoutAction::Drop
            })
            .unwrap();
        output_state.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
    }

    fn on_estimated_vblank_timer(&self, niri: &mut Niri, output: Output) {
        let Some(output_state) = niri.output_state.get_mut(&output) else {
            return;
        };

        output_state.frame_callback_sequence =
            output_state.frame_callback_sequence.wrapping_add(1);

        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => unreachable!(),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(_) => (),
            // The timer fired just in front of a redraw.
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
                output_state.redraw_state = RedrawState::Queued;
                return;
            }
        }

        if output_state.unfinished_animations_remain {
            niri.queue_redraw(&output);
        } else {
            niri.send_frame_callbacks(&output);
        }
    }

    // -------------------------------------------------------------------
    // Backend trait methods
    // -------------------------------------------------------------------

    pub fn seat_name(&self) -> String {
        "anland".to_owned()
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        Some(f(&mut self.renderer))
    }

    pub fn toggle_debug_tint(&mut self) {
        self.renderer
            .set_debug_flags(self.renderer.debug_flags() ^ DebugFlags::TINT);
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.renderer.import_dmabuf(dmabuf, None).is_ok()
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn protocol_format_to_fourcc(format: u32) -> Fourcc {
    match format {
        0x34325241 | 0x41425234 | 0x08 => Fourcc::Argb8888,
        // Consumer-side format 1 == Android RGBA_8888 (AHARDWAREBUFFER
        // R8G8B8A8_UNORM): byte order R,G,B,A in memory == DRM ABGR8888.
        // Importing it as ARGB8888 (B,G,R,A) rendered every pixel R<->B swapped.
        0x01 => Fourcc::Abgr8888,
        0x34325258 | 0x58425234 | 0x0c | 0x02 => Fourcc::Xrgb8888,
        0x32335241 | 0x41425233 | 0x09 | 0x03 => Fourcc::Abgr8888,
        0x32335258 | 0x58425233 | 0x0d | 0x04 => Fourcc::Xbgr8888,
        _ => Fourcc::Argb8888,
    }
}
