use std::ffi::CString;
use std::time::Duration;

use anland_sys::*;

fn main() {
    let socket_path = std::env::var("ANLAND_SOCKET")
        .unwrap_or_else(|_| "/run/display.sock".to_string());

    println!("=== Anland Producer Test ===");
    println!("Socket: {}", socket_path);

    let c_path = CString::new(socket_path.as_str()).expect("invalid socket path");
    let mut ctx = match AnlandContext::connect(&c_path) {
        Ok(ctx) => {
            println!("[OK] Connected to daemon");
            ctx
        }
        Err(e) => {
            eprintln!("[FAIL] connect: {}", e);
            std::process::exit(1);
        }
    };

    let info = ctx.screen_info();
    println!(
        "[OK] Screen: {}x{} format=0x{:x} refresh={}mHz",
        info.width, info.height, info.format, info.refresh
    );

    println!("[..] Waiting for consumer...");
    let mut attempts = 0;
    loop {
        if !ctx.is_fallback() {
            break;
        }
        match ctx.try_exit_fallback() {
            Ok(()) => {
                println!("[OK] Connected after {} attempts", attempts);
                break;
            }
            Err(_) => {
                attempts += 1;
                if attempts % 5 == 0 {
                    print!(".");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    println!();

    let buf_count = ctx.buffer_count();
    println!("[OK] Buffers: {}", buf_count);

    for i in 0..buf_count {
        let fd = ctx.dmabuf_fd_at(i);
        if let Some(info) = ctx.dmabuf_info_at(i) {
            let w = info.width;
            let h = info.height;
            let s = info.stride;
            let f = info.format;
            let m = info.modifier;
            let o = info.offset;
            println!("  buf[{i}]: fd={fd} {w}x{h} stride={s} fmt=0x{f:x} mod=0x{m:x} off={o}");
        }
    }

    let data_fd = ctx.data_fd();
    let buf_ready_fd = ctx.buffer_ready_fd();
    println!("[OK] data_fd={data_fd} buf_ready_fd={buf_ready_fd}");

    println!("[OK] EGL test: {}", if test_egl() { "PASS" } else { "SKIP" });

    for i in 0..3 {
        ctx.set_render_fence(-1);
        ctx.trigger_refresh();
        println!("  refresh #{i} sent");
        std::thread::sleep(Duration::from_millis(500));
    }

    println!("[..] Polling input (10s)...");
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(event) = ctx.poll_input_event(100) {
            print_event(&event);
            ctx.handle_unhandled_event(&event);
        }
    }

    println!("=== Test Complete ===");
}

fn test_egl() -> bool {
    let egl = match unsafe { libloading::Library::new("libEGL.so.1") } {
        Ok(lib) => lib,
        Err(_) => return false,
    };

    type EglGetDisplay = unsafe extern "C" fn(i32) -> *mut std::ffi::c_void;
    type EglInitialize = unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32, *mut i32) -> i32;

    let get_display: libloading::Symbol<EglGetDisplay> = unsafe { egl.get(b"eglGetDisplay").unwrap() };
    let initialize: libloading::Symbol<EglInitialize> = unsafe { egl.get(b"eglInitialize").unwrap() };

    let display = unsafe { get_display(0) };
    if display.is_null() {
        return false;
    }

    let mut major = 0i32;
    let mut minor = 0i32;
    if unsafe { initialize(display, &mut major, &mut minor) } == 0 {
        return false;
    }

    println!("  EGL {major}.{minor}");
    true
}

fn print_event(event: &InputEvent) {
    let ty = event.type_;
    let u = read_union(event);

    match ty {
        INPUT_TYPE_TOUCH => {
            let t = read_touch(&u);
            println!("[EVENT] TOUCH action={} x={:.1} y={:.1} id={}", t.0, t.1, t.2, t.3);
        }
        INPUT_TYPE_KEY => {
            let k = read_key(&u);
            println!("[EVENT] KEY action={} code={}", k.0, k.1);
        }
        INPUT_TYPE_POINTER_MOTION => {
            let m = read_motion(&u);
            println!("[EVENT] MOTION x={:.1} y={:.1} dx={:.1} dy={:.1}", m.0, m.1, m.2, m.3);
        }
        INPUT_TYPE_POINTER_BUTTON => {
            let b = read_button(&u);
            println!("[EVENT] BUTTON btn={} pressed={}", b.0, b.1);
        }
        INPUT_TYPE_POINTER_AXIS => {
            let a = read_axis(&u);
            println!("[EVENT] AXIS axis={} val={:.1} discrete={}", a.0, a.1, a.2);
        }
        INPUT_TYPE_DISPLAY_REFRESH => {
            let d = read_display(&u);
            println!("[EVENT] REFRESH {}mHz", d);
        }
        INPUT_TYPE_CLIPBOARD => {
            let c = read_clipboard(&u);
            println!("[EVENT] CLIPBOARD {}B", c);
        }
        INPUT_TYPE_TEXT_INPUT => {
            let t = read_text_input(&u);
            println!("[EVENT] TEXT_INPUT {}B", t);
        }
        _ => println!("[EVENT] type={ty}"),
    }
}

fn read_union(event: &InputEvent) -> InputEventUnion {
    unsafe {
        let u: InputEventUnion = std::mem::zeroed();
        let mut u = u;
        std::ptr::copy_nonoverlapping(
            &event.touch as *const InputTouch as *const u8,
            &mut u as *mut InputEventUnion as *mut u8,
            std::mem::size_of::<InputEventUnion>(),
        );
        u
    }
}

fn read_touch(u: &InputEventUnion) -> (i32, f32, f32, i32) {
    let t = unsafe { u.touch };
    (t.action, t.x, t.y, t.pointer_id)
}

fn read_key(u: &InputEventUnion) -> (i32, i32) {
    let k = unsafe { u.key };
    (k.action, k.keycode)
}

fn read_motion(u: &InputEventUnion) -> (f32, f32, f32, f32) {
    let m = unsafe { u.pointer_motion };
    (m.x, m.y, m.dx, m.dy)
}

fn read_button(u: &InputEventUnion) -> (u32, i32) {
    let b = unsafe { u.pointer_button };
    (b.button, b.pressed)
}

fn read_axis(u: &InputEventUnion) -> (u32, f32, i32) {
    let a = unsafe { u.pointer_axis };
    (a.axis, a.value, a.discrete)
}

fn read_display(u: &InputEventUnion) -> u32 {
    unsafe { u.display.refresh_mhz }
}

fn read_clipboard(u: &InputEventUnion) -> u32 {
    unsafe { u.clipboard.size }
}

fn read_text_input(u: &InputEventUnion) -> u32 {
    unsafe { u.text_input.size }
}