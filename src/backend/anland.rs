use std::collections::HashMap;
use std::ffi::CString;
use std::mem;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

    reconnect_timer_token: Option<RegistrationToken>,
    buf_ready_source_token: Option<RegistrationToken>,
    data_source_token: Option<RegistrationToken>,

    ipc_outputs: Arc<Mutex<IpcOutputMap>>,

    last_buffer_idx: i32,
    debug_tint: bool,
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
            last_buffer_idx: -1,
            debug_tint: false,
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
        niri.add_output(output, None, false);

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

        // The previous session's frame index is meaningless for these freshly
        // imported buffers. Reset it so damage tracking does a full repaint
        // (age 0) instead of claiming age 1 on a brand-new buffer whose content
        // is uninitialized -- that produced blank (black) frames that only
        // cleared on the next exit/enter, alternating on every reconnect.
        self.last_buffer_idx = -1;

        self.dmabufs.clear();

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

        info!(
            "connected to anland consumer: {} buffers, {}x{}",
            self.dmabufs.len(),
            self.ctx.screen_info().width,
            self.ctx.screen_info().height,
        );

        // Drop any sources registered by a previous connection before installing
        // the fresh fds. On reconnect the C context closes the old buffer-ready and
        // data fds; if we kept the old FdEventSources registered, the event loop
        // would hold descriptors that are now closed (their numbers may even be
        // recycled for unrelated fds), producing stale redraws/reads and sporadic
        // black frames. remove_source is idempotent for an unset token.
        self.remove_event_sources(niri);
        self.register_buffer_ready_source(niri);
        self.register_input_source(niri);
    }

    fn remove_event_sources(&mut self, niri: &mut Niri) {
        if let Some(token) = self.buf_ready_source_token.take() {
            niri.event_loop.remove(token);
        }
        if let Some(token) = self.data_source_token.take() {
            niri.event_loop.remove(token);
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
            debug!("buffer_ready event fired (val={})", val);
            if let Some(output) = anland.output.clone() {
                state.niri.queue_redraw(&output);
            }
        }) {
            self.buf_ready_source_token = Some(token);
        }
    }

    fn register_input_source(&mut self, niri: &mut Niri) {
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
        }) {
            self.data_source_token = Some(token);
        }
    }

    // -------------------------------------------------------------------
    // Input dispatch
    // -------------------------------------------------------------------

    /// Poll the daemon for queued input and return the translated smithay
    /// events. Non-input notifications (display refresh, clipboard) are
    /// handled internally and not forwarded.
    fn poll_input(&mut self, timeout: i32) -> Vec<SmithayInputEvent<AnlandInput>> {
        let mut out = Vec::new();
        loop {
            let Some(event) = self.ctx.poll_input_event(timeout) else {
                break;
            };
            if event.type_ == INPUT_TYPE_TEXT_INPUT {
                let u = unsafe {
                    let mut u: InputEventUnion = std::mem::zeroed();
                    std::ptr::copy_nonoverlapping(
                        &event.touch as *const InputTouch as *const u8,
                        &mut u as *mut InputEventUnion as *mut u8,
                        std::mem::size_of::<InputEventUnion>(),
                    );
                    u
                };
                let len = unsafe { u.text_input.size } as usize;
                if len > 0 {
                    // Nonblock drain: the producer sends the text payload together
                    // with the header in one send_all, so it is already in the
                    // socket. Don't fall through to special-event handling if the
                    // drain fails, or the leftover payload desyncs the stream.
                    let mut buf = vec![0u8; len];
                    if self.ctx.poll_input_event_extend_data(&mut buf, 0) {
                        debug!("text input: {} bytes", len);
                        out.extend(self.text_to_key_events(&buf));
                    }
                }
                continue;
            }
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
            INPUT_TYPE_CLIPBOARD => {
                let c = unsafe { u.clipboard };
                if c.size > 0 {
                    let mut buf = vec![0u8; c.size as usize];
                    self.ctx.poll_input_event_extend_data(&mut buf, 1000);
                    debug!("clipboard: {} bytes", c.size);
                }
                true
            }
            _ => false,
        }
    }

    /// Turn a run of UTF-8 text into synthesized key press/release events.
    ///
    /// IME text arrives as an arbitrary unicode payload with no keyboard layout,
    /// but Wayland clients only receive keycodes that they resolve through the
    /// keymap sent by the compositor. For text that lives in the active layout
    /// (Latin/ASCII) we find the evdev keycode + Shift needed to reproduce each
    /// codepoint and emit a press+release pair. Since the compositor and the
    /// focused client resolve against the same keymap, the character is typed in
    /// any client regardless of which one is focused. Codepoints not present in
    /// a keyboard (e.g. emoji) are dropped.
    fn text_to_key_events(&self, text: &[u8]) -> Vec<SmithayInputEvent<AnlandInput>> {
        const KEYCODE_LEFT_SHIFT: u32 = 42;
        const KEYCODE_SPACE: u32 = 57;
        const KEYCODE_ENTER: u32 = 28;
        const KEYCODE_TAB: u32 = 15;
        const KEYCODE_BACKSPACE: u32 = 14;
        const KEYCODE_ESCAPE: u32 = 1;

        // Map an ASCII byte to (keycode, needs_shift). Letters, digits and the
        // typical symbol set follow the standard evdev keymap; anything unknown
        // returns None and is skipped.
        fn char_key(c: u8) -> Option<(u32, bool)> {
            if c == b' ' {
                return Some((KEYCODE_SPACE, false));
            }
            if c.is_ascii_digit() {
                let keycode = match c {
                    b'1' => 2, b'2' => 3, b'3' => 4, b'4' => 5, b'5' => 6,
                    b'6' => 7, b'7' => 8, b'8' => 9, b'9' => 10, _ => 11,
                };
                return Some((keycode, false));
            }
            // Letters are stable evdev keycodes; upper-case just needs Shift.
            if c.is_ascii_alphabetic() {
                let code = match c.to_ascii_lowercase() {
                    b'a' => 30, b'b' => 48, b'c' => 46, b'd' => 32, b'e' => 18,
                    b'f' => 33, b'g' => 34, b'h' => 35, b'i' => 23, b'j' => 36,
                    b'k' => 37, b'l' => 38, b'm' => 50, b'n' => 49, b'o' => 24,
                    b'p' => 25, b'q' => 16, b'r' => 19, b's' => 31, b't' => 20,
                    b'u' => 22, b'v' => 47, b'w' => 17, b'x' => 45, b'y' => 21,
                    b'z' => 44,
                    _ => unreachable!(),
                };
                return Some((code, c.is_ascii_uppercase()));
            }
            // Digit-row shifts: 1->!, 2->@, ... 0->). Keycode is the digit's.
            const DIGIT_SHIFTS: &[(u8, u32)] = &[
                (b'!', 2), (b'@', 3), (b'#', 4), (b'$', 5), (b'%', 6),
                (b'^', 7), (b'&', 8), (b'*', 9), (b'(', 10), (b')', 11),
            ];
            if let Some(&(_, keycode)) = DIGIT_SHIFTS.iter().find(|&&(ch, _)| ch == c) {
                return Some((keycode, true));
            }
            // Punctuation pairs (normal, shifted) on shared keys.
            match c {
                b'-' => Some((12, false)),  b'_' => Some((12, true)),
                b'=' => Some((13, false)),  b'+' => Some((13, true)),
                b'[' => Some((26, false)),  b'{' => Some((26, true)),
                b']' => Some((27, false)),  b'}' => Some((27, true)),
                b'\\' => Some((43, false)), b'|' => Some((43, true)),
                b';' => Some((39, false)),  b':' => Some((39, true)),
                b'\'' => Some((40, false)), b'"' => Some((40, true)),
                b'`' => Some((41, false)),  b'~' => Some((41, true)),
                b',' => Some((51, false)),  b'<' => Some((51, true)),
                b'.' => Some((52, false)),  b'>' => Some((52, true)),
                b'/' => Some((53, false)),  b'?' => Some((53, true)),
                b'\n' => Some((KEYCODE_ENTER, false)),
                b'\t' => Some((KEYCODE_TAB, false)),
                b'\x08' => Some((KEYCODE_BACKSPACE, false)),
                b'\x1b' => Some((KEYCODE_ESCAPE, false)),
                _ => None,
            }
        }

        let mut events = Vec::new();
        let time = get_monotonic_time().as_micros() as u64;
        let mut shift_down = false;

        for &c in text {
            let Some((keycode, needs_shift)) = char_key(c) else {
                continue;
            };
            if needs_shift && !shift_down {
                events.push(SmithayInputEvent::Keyboard {
                    event: AnlandKeyboardEvent {
                        time,
                        key_code: KEYCODE_LEFT_SHIFT,
                        state: KeyState::Pressed,
                    },
                });
                shift_down = true;
            } else if !needs_shift && shift_down {
                events.push(SmithayInputEvent::Keyboard {
                    event: AnlandKeyboardEvent {
                        time,
                        key_code: KEYCODE_LEFT_SHIFT,
                        state: KeyState::Released,
                    },
                });
                shift_down = false;
            }
            events.push(SmithayInputEvent::Keyboard {
                event: AnlandKeyboardEvent {
                    time,
                    key_code: keycode,
                    state: KeyState::Pressed,
                },
            });
            events.push(SmithayInputEvent::Keyboard {
                event: AnlandKeyboardEvent {
                    time,
                    key_code: keycode,
                    state: KeyState::Released,
                },
            });
        }

        if shift_down {
            events.push(SmithayInputEvent::Keyboard {
                event: AnlandKeyboardEvent {
                    time,
                    key_code: KEYCODE_LEFT_SHIFT,
                    state: KeyState::Released,
                },
            });
        }

        events
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
        _target_presentation_time: Duration,
    ) -> RenderResult {
        let _span = tracy_client::span!("Anland::render");

        if self.ctx.is_fallback() {
            debug!("render: skipped (in fallback)");
            return RenderResult::Skipped;
        }

        let idx = self.ctx.selected_buffer_index();
        if idx < 0 || idx as usize >= self.dmabufs.len() {
            debug!("render: skipped selected={} dmabufs={}", idx, self.dmabufs.len());
            return RenderResult::Skipped;
        }

        // Only report a buffer age of 1 to the damage tracker when we're
        // rendering into the same buffer as the previous frame. The consumer
        // hands out buffers through its shared-memory index, so consecutive
        // frames usually land on different dmabufs; claiming age 1 there would
        // make the tracker repaint only the damaged region onto a buffer that
        // still holds stale content, producing flicker.
        let age = if idx == self.last_buffer_idx { 1 } else { 0 };
        self.last_buffer_idx = idx;

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
        let res = match damage_tracker.render_output(
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

        if let Err(err) = res.sync.wait() {
            warn!("error waiting for frame completion: {err:?}");
        }

        niri.update_primary_scanout_output(output, &res.states);

        self.ctx.set_render_fence(-1);
        self.ctx.trigger_refresh();

        let mut presentation_feedbacks =
            niri.take_presentation_feedbacks(output, &res.states);
        presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
            get_monotonic_time(),
            Refresh::Unknown,
            0,
            wp_presentation_feedback::Kind::empty(),
        );

        let output_state = niri.output_state.get_mut(output).unwrap();
        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => unreachable!(),
        }
        output_state.frame_callback_sequence =
            output_state.frame_callback_sequence.wrapping_add(1);

        RenderResult::Submitted
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
        0x34325241 | 0x41425234 | 0x08 | 0x01 => Fourcc::Argb8888,
        0x34325258 | 0x58425234 | 0x0c | 0x02 => Fourcc::Xrgb8888,
        0x32335241 | 0x41425233 | 0x09 | 0x03 => Fourcc::Abgr8888,
        0x32335258 | 0x58425233 | 0x0d | 0x04 => Fourcc::Xbgr8888,
        _ => Fourcc::Argb8888,
    }
}
