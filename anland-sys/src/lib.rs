use std::ffi::{c_char, c_int, c_void};
use std::os::fd::RawFd;

// ---------------------------------------------------------------------------
// Raw C bindings
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct display_ctx {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct buf_info {
    pub stride: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub modifier: u64,
    pub offset: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    pub type_: u32,
    pub touch: InputTouch,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union InputEventUnion {
    pub touch: InputTouch,
    pub key: InputKey,
    pub pointer_motion: InputPointerMotion,
    pub pointer_button: InputPointerButton,
    pub pointer_axis: InputPointerAxis,
    pub display: InputDisplay,
    pub display_rotation: InputDisplayRotation,
    pub clipboard: InputClipboard,
    pub text_input: InputTextInput,
    pub input_action: InputAction,
    pub resource: InputResource,
    pub input_caps: InputCaps,
    pub padding: [u32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputTouch {
    pub action: i32,
    pub x: f32,
    pub y: f32,
    pub pointer_id: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputKey {
    pub action: i32,
    pub keycode: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputPointerMotion {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputPointerButton {
    pub button: u32,
    pub pressed: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputPointerAxis {
    pub axis: u32,
    pub value: f32,
    pub discrete: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputDisplay {
    pub refresh_mhz: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputDisplayRotation {
    /// Current display rotation in degrees counter-clockwise (0/90/180/270).
    pub angle_deg: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputClipboard {
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputTextInput {
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputAction {
    pub action: u32,
    pub value: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputResource {
    pub type_: u32,
    pub fdnum: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputCaps {
    /// CONSUMER_CAP_* capability bitmask announced by the consumer.
    pub caps: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputEvent {
    pub type_: u32,
    pub clipboard: OutputClipboard,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OutputEventUnion {
    pub clipboard: OutputClipboard,
    pub resources_request: OutputResourcesRequest,
    pub set_consumer_var: OutputSetConsumerVar,
    pub cursor_pos: OutputCursorPos,
    pub padding: [u32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputClipboard {
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputResourcesRequest {
    pub type_: u32,
    pub args: [u32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputSetConsumerVar {
    pub var: u32,
    pub value: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputCursorPos {
    /// Hotspot position in PHYSICAL buffer pixels.
    pub x: f32,
    pub y: f32,
    pub hx: u32,
    pub hy: u32,
}

// Input event type constants
pub const INPUT_TYPE_TOUCH: u32 = 1;
pub const INPUT_TYPE_KEY: u32 = 2;
pub const INPUT_TYPE_POINTER_MOTION: u32 = 3;
pub const INPUT_TYPE_POINTER_BUTTON: u32 = 4;
pub const INPUT_TYPE_POINTER_AXIS: u32 = 5;
pub const INPUT_TYPE_TOUCH_FRAME: u32 = 6;
pub const INPUT_TYPE_DISPLAY_REFRESH: u32 = 7;
pub const INPUT_TYPE_CLIPBOARD: u32 = 8;
pub const INPUT_TYPE_TEXT_INPUT: u32 = 9;
pub const INPUT_TYPE_ACTION: u32 = 10;
pub const INPUT_TYPE_RESOURCE: u32 = 11;
pub const INPUT_TYPE_RESOURCE_INVALID: u32 = 12;
pub const INPUT_TYPE_DISPLAY_ROTATION: u32 = 13;
pub const INPUT_TYPE_CAPS: u32 = 14;

// Output event type constants
pub const OUTPUT_TYPE_CLIPBOARD: u32 = 1;
pub const OUTPUT_TYPE_RESOURCES_REQUEST: u32 = 2;
pub const OUTPUT_TYPE_SET_CONSUMER_VAR: u32 = 3;
pub const OUTPUT_TYPE_CURSOR_POS: u32 = 4;
pub const OUTPUT_TYPE_CURSOR_BITMAP: u32 = 5;

pub const CONSUMER_CAP_CURSOR_PLANE: u32 = 1;

// Consumer variable constants
pub const CONSUMER_VAR_CAPTURE_MOUSE: u32 = 1;

extern "C" {
    pub fn connect_to_deamon(ctx: *mut *mut display_ctx, socket_path: *const c_char) -> c_int;
    pub fn disconnect(ctx: *mut display_ctx);
    pub fn get_screen_info(
        ctx: *mut display_ctx,
        width: *mut u32,
        height: *mut u32,
        format: *mut u32,
        refresh: *mut u32,
    ) -> c_int;
    pub fn set_render_fence(ctx: *mut display_ctx, fence_fd: c_int);
    pub fn trigger_refresh(ctx: *mut display_ctx) -> c_int;
    pub fn poll_input_event(
        ctx: *mut display_ctx,
        event: *mut InputEvent,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn poll_input_event_extend_data(
        ctx: *mut display_ctx,
        payload: *mut c_void,
        size: usize,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn push_output_event(ctx: *mut display_ctx, event: *const OutputEvent) -> c_int;
    pub fn try_push_cursor_bitmap(
        ctx: *mut display_ctx,
        w: u32,
        h: u32,
        hx: u32,
        hy: u32,
        pixels: *const u8,
        pixel_len: u32,
    ) -> c_int;
    pub fn try_push_cursor_pos(
        ctx: *mut display_ctx,
        x: f32,
        y: f32,
        hx: u32,
        hy: u32,
    ) -> c_int;
    pub fn push_output_event_with_length(
        ctx: *mut display_ctx,
        event: *const OutputEvent,
        payload: *const c_void,
        size: usize,
    ) -> c_int;
    pub fn set_fallback_callback(
        ctx: *mut display_ctx,
        on_fallback: Option<extern "C" fn(*mut c_void)>,
        userdata: *mut c_void,
    ) -> c_int;
    pub fn is_fallback(ctx: *mut display_ctx) -> bool;
    pub fn try_exit_fallback(ctx: *mut display_ctx) -> c_int;
    pub fn get_data_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_audio_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_buffer_ready_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_buf_count(ctx: *mut display_ctx) -> c_int;
    pub fn get_selected_idx(ctx: *mut display_ctx) -> c_int;
    pub fn get_dmabuf_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_dmabuf_fd_at(ctx: *mut display_ctx, idx: c_int) -> c_int;
    pub fn get_dmabuf_info(ctx: *mut display_ctx, info: *mut buf_info) -> c_int;
    pub fn get_dmabuf_info_at(ctx: *mut display_ctx, idx: c_int, info: *mut buf_info) -> c_int;
}

// ---------------------------------------------------------------------------
// Safe Rust wrapper
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub refresh: u32,
}

pub struct AnlandContext {
    ctx: *mut display_ctx,
}

// SAFETY: display_ctx is not Send/Sync by itself, but the C library is
// designed to be used from a single thread (the compositor's main thread).
// We mark it Send so it can be owned by the compositor's state, but all
// access must be from the main thread.
unsafe impl Send for AnlandContext {}

impl AnlandContext {
    pub fn connect(socket_path: &std::ffi::CStr) -> Result<Self, String> {
        let mut ctx: *mut display_ctx = std::ptr::null_mut();
        let ret = unsafe { connect_to_deamon(&mut ctx, socket_path.as_ptr()) };
        if ret != 0 || ctx.is_null() {
            return Err("failed to connect to anland daemon".into());
        }
        Ok(Self { ctx })
    }

    pub fn screen_info(&self) -> ScreenInfo {
        let mut w: u32 = 0;
        let mut h: u32 = 0;
        let mut fmt: u32 = 0;
        let mut refresh: u32 = 0;
        unsafe {
            get_screen_info(self.ctx, &mut w, &mut h, &mut fmt, &mut refresh);
        }
        ScreenInfo {
            width: w,
            height: h,
            format: fmt,
            refresh,
        }
    }

    pub fn is_fallback(&self) -> bool {
        // The C side owns the authoritative fallback state (set by enter_fallback()
        // on consumer loss, cleared by try_exit_fallback()). Reading it directly is
        // required: the reconnect timer gates on this and the cached flag was never
        // updated when C entered fallback, which froze the reconnect loop forever.
        unsafe { is_fallback(self.ctx) }
    }

    pub fn try_exit_fallback(&mut self) -> Result<(), ()> {
        let ret = unsafe { try_exit_fallback(self.ctx) };
        if ret == 0 {
            Ok(())
        } else {
            Err(())
        }
    }

    pub fn selected_buffer_index(&self) -> i32 {
        unsafe { get_selected_idx(self.ctx) }
    }

    pub fn dmabuf_fd_at(&self, idx: i32) -> RawFd {
        unsafe { get_dmabuf_fd_at(self.ctx, idx) }
    }

    pub fn dmabuf_info_at(&self, idx: i32) -> Option<buf_info> {
        let mut info = buf_info {
            stride: 0,
            width: 0,
            height: 0,
            format: 0,
            modifier: 0,
            offset: 0,
        };
        let ret = unsafe { get_dmabuf_info_at(self.ctx, idx, &mut info) };
        if ret == 0 {
            Some(info)
        } else {
            None
        }
    }

    pub fn buffer_count(&self) -> i32 {
        unsafe { get_buf_count(self.ctx) }
    }

    pub fn buffer_ready_fd(&self) -> RawFd {
        unsafe { get_buffer_ready_fd(self.ctx) }
    }

    pub fn data_fd(&self) -> RawFd {
        unsafe { get_data_fd(self.ctx) }
    }

    pub fn trigger_refresh(&mut self) {
        unsafe {
            trigger_refresh(self.ctx);
        }
    }

    pub fn set_render_fence(&mut self, fence_fd: RawFd) {
        unsafe {
            set_render_fence(self.ctx, fence_fd);
        }
    }

    pub fn poll_input_event(&self, timeout_ms: i32) -> Option<InputEvent> {
        let mut event = unsafe { std::mem::zeroed::<InputEvent>() };
        let ret = unsafe { poll_input_event(self.ctx, &mut event, timeout_ms) };
        if ret == 1 {
            Some(event)
        } else {
            None
        }
    }

    pub fn poll_input_event_extend_data(&self, buf: &mut [u8], timeout_ms: i32) -> bool {
        let ret = unsafe {
            poll_input_event_extend_data(
                self.ctx,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                timeout_ms,
            )
        };
        ret > 0
    }

    pub fn push_output_event(&mut self, event: &OutputEvent) {
        unsafe {
            push_output_event(self.ctx, event);
        }
    }

    pub fn push_output_event_with_length(&mut self, event: &OutputEvent, payload: &[u8]) {
        unsafe {
            push_output_event_with_length(
                self.ctx,
                event,
                payload.as_ptr() as *const c_void,
                payload.len(),
            );
        }
    }

    pub fn push_set_consumer_var(&mut self, var: u32, value: u32) {
        let mut event: OutputEvent = unsafe { std::mem::zeroed() };
        event.type_ = OUTPUT_TYPE_SET_CONSUMER_VAR;
        unsafe {
            let u: OutputEventUnion = std::mem::zeroed();
            let mut u = u;
            u.set_consumer_var = OutputSetConsumerVar { var, value };
            event.clipboard.size = u.set_consumer_var.var;
            std::ptr::copy_nonoverlapping(
                &u as *const OutputEventUnion as *const u8,
                &mut event.clipboard as *mut OutputClipboard as *mut u8,
                std::mem::size_of::<OutputEventUnion>(),
            );
        }
        self.push_output_event(&event);
    }

    /// Move the consumer-side cursor sprite. (x, y) is the hotspot position in
    /// physical buffer pixels; (hx, hy) is the sprite's hotspot offset so the
    /// consumer can place the image at (x - hx, y - hy).
    pub fn push_cursor_pos(&mut self, x: f32, y: f32, hx: u32, hy: u32) {
        let mut event: OutputEvent = unsafe { std::mem::zeroed() };
        event.type_ = OUTPUT_TYPE_CURSOR_POS;
        unsafe {
            let mut u: OutputEventUnion = std::mem::zeroed();
            u.cursor_pos = OutputCursorPos { x, y, hx, hy };
            std::ptr::copy_nonoverlapping(
                &u as *const OutputEventUnion as *const u8,
                &mut event.clipboard as *mut OutputClipboard as *mut u8,
                std::mem::size_of::<OutputEventUnion>(),
            );
        }
        self.push_output_event(&event);
    }

    /// (Re)define the cursor sprite image: a 16-byte header {w, h, hx, hy}
    /// followed by w*h RGBA8888 bytes. A zero width hides the sprite.
    pub fn push_cursor_bitmap(&mut self, w: u32, h: u32, hx: u32, hy: u32, rgba: &[u8]) {
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize);
        let header: [u32; 4] = [w, h, hx, hy];
        let event = OutputEvent {
            type_: OUTPUT_TYPE_CURSOR_BITMAP,
            clipboard: OutputClipboard {
                size: rgba.len() as u32,
            },
        };
        // The payload is the fixed header (little-endian u32s) followed by the
        // pixel bytes.
        let mut payload = Vec::with_capacity(std::mem::size_of_val(&header) + rgba.len());
        for v in header {
            payload.extend_from_slice(&v.to_ne_bytes());
        }
        payload.extend_from_slice(rgba);
        self.push_output_event_with_length(&event, &payload);
    }

    /// Non-blocking variants: return false when skipped because the socket
    /// backlog was too high (caller keeps its last state and retries).
    pub fn try_push_cursor_bitmap(
        &mut self,
        w: u32,
        h: u32,
        hx: u32,
        hy: u32,
        rgba: &[u8],
    ) -> bool {
        unsafe {
            try_push_cursor_bitmap(self.ctx, w, h, hx, hy, rgba.as_ptr(), rgba.len() as u32) == 1
        }
    }

    pub fn try_push_cursor_pos(&mut self, x: f32, y: f32, hx: u32, hy: u32) -> bool {
        unsafe { try_push_cursor_pos(self.ctx, x, y, hx, hy) == 1 }
    }

    pub fn handle_unhandled_event(&self, event: &InputEvent) {
        if event.type_ == INPUT_TYPE_CLIPBOARD {
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
            let size = unsafe { u.clipboard.size as usize };
            if size > 0 {
                let mut buf = vec![0u8; size];
                self.poll_input_event_extend_data(&mut buf, 1000);
            }
        }
    }

    pub fn raw_ptr(&self) -> *mut display_ctx {
        self.ctx
    }
}

impl Drop for AnlandContext {
    fn drop(&mut self) {
        unsafe {
            disconnect(self.ctx);
        }
    }
}