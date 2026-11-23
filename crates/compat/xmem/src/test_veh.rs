use core::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    fn AddVectoredExceptionHandler(first: u32, handler: unsafe extern "system" fn(*mut [*mut c_void; 2]) -> i32) -> *mut c_void;
    fn GetModuleHandleExW(flags: u32, name: *const u16, out: *mut usize) -> i32;
    fn GetCurrentThreadId() -> u32;
}

unsafe extern "system" fn on_exception(info: *mut [*mut c_void; 2]) -> i32 {
    let rec = unsafe { (*info)[0] as *const u8 };
    let code = unsafe { core::ptr::read_unaligned(rec as *const u32) };
    if code == 0xc000_0005 {
        let ip = unsafe { core::ptr::read_unaligned(rec.add(0x10) as *const usize) };
        let kind = unsafe { core::ptr::read_unaligned(rec.add(0x20) as *const usize) };
        let addr = unsafe { core::ptr::read_unaligned(rec.add(0x28) as *const usize) };
        let mut m = 0usize;
        unsafe { GetModuleHandleExW(6, ip as *const u16, &mut m) };
        eprintln!("VEH access violation tid={} ip={ip:#x} module={m:#x}+{:#x} {} {addr:#x}", unsafe { GetCurrentThreadId() }, ip.wrapping_sub(m), if kind == 1 { "writing" } else if kind == 8 { "executing" } else { "reading" });
        eprintln!("{}", std::backtrace::Backtrace::force_capture());
    }
    0
}

#[used]
#[link_section = ".CRT$XCU"]
static INIT: extern "C" fn() = init;

extern "C" fn init() {
    unsafe { AddVectoredExceptionHandler(1, on_exception) };
}
