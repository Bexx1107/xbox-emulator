use core::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(n: *const u16) -> *mut c_void;
    fn ReadProcessMemory(h: *mut c_void, addr: *const c_void, buf: *mut c_void, n: usize, read: *mut usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetThreadId(h: *mut c_void) -> u32;
    fn GetExitCodeThread(h: *mut c_void, code: *mut u32) -> i32;
    fn QueryThreadCycleTime(h: *mut c_void, cycles: *mut u64) -> i32;
    fn GetThreadDescription(h: *mut c_void, desc: *mut *mut u16) -> i32;
    fn LocalFree(p: *mut c_void) -> *mut c_void;
    fn GetLastError() -> u32;
}

pub const VAR: &str = "TOMBOX_WEBM_PROBE";
pub const TITLE_EXE: &str = "GbxGame.exe";

pub const INIT_COUNT: usize = 0x292fec0;
pub const THREAD_HANDLE: usize = 0x292ff50;
pub const QUIT_FLAG: usize = 0x292ff58;
pub const AFFINITY_MASK: usize = 0x28e9538;
pub const DECODERS: usize = 0x292fd90;
pub const DECODER_COUNT: usize = 0x292fd98;
pub const ROUND_ROBIN: usize = 0x292fec4;
pub const WAKE_TIME: usize = 0x292fec8;

pub const FIELDS: &[(&str, usize, usize)] = &[
    ("frames", 0xb28, 4),
    ("req", 0xb30, 4),
    ("target", 0xb34, 4),
    ("late", 0xb38, 4),
    ("block", 0x48, 8),
    ("block_next", 0x78, 8),
    ("now", 0x60, 4),
    ("s64", 0x64, 4),
    ("t90", 0x90, 4),
    ("s94", 0x94, 4),
    ("cur_state", 0x38 + 0x2c, 4),
    ("next_state", 0x68 + 0x2c, 4),
    ("cues", 0x670, 4),
    ("stream", 0x0, 8),
    ("vpx_y", 0x148, 8),
];

pub const MAX_DECODERS: usize = 8;

fn read(addr: usize, len: usize) -> Option<u64> {
    let mut v = 0u64;
    let mut got = 0usize;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const c_void, &mut v as *mut u64 as *mut c_void, len.min(8), &mut got) };
    (ok != 0 && got == len.min(8)).then_some(v)
}

fn thread_line(h: usize) -> (String, u64) {
    if h == 0 {
        return ("thread handle 0 (not created)".into(), 0);
    }
    let hp = h as *mut c_void;
    let tid = unsafe { GetThreadId(hp) };
    let mut code = 0u32;
    let alive = unsafe { GetExitCodeThread(hp, &mut code) } != 0;
    let mut cyc = 0u64;
    let cyc_ok = unsafe { QueryThreadCycleTime(hp, &mut cyc) } != 0;
    let mut p: *mut u16 = core::ptr::null_mut();
    let name = if unsafe { GetThreadDescription(hp, &mut p) } >= 0 && !p.is_null() {
        let mut n = 0usize;
        while n < 128 && unsafe { *p.add(n) } != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(p, n) });
        unsafe { LocalFree(p as *mut c_void) };
        s
    } else {
        String::new()
    };
    let exit = if !alive { format!("exitcode? err={}", unsafe { GetLastError() }) } else if code == 259 { "running".into() } else { format!("exited {code:#x}") };
    (format!("thread h={h:#x} tid={tid} name='{name}' {exit} cycle_query={}", if cyc_ok { "ok" } else { "failed" }), cyc)
}

fn snapshot(base: usize) -> (String, Vec<String>, u64) {
    let g = |rva: usize, len: usize| read(base + rva, len).unwrap_or(u64::MAX);
    let h = g(THREAD_HANDLE, 8) as usize;
    let mask = g(AFFINITY_MASK, 4);
    let topo = super::affinity::topology();
    let host = topo.to_host(mask, super::affinity::process_console_mask());
    let (tl, cycles) = thread_line(if h == usize::MAX { 0 } else { h });
    let head = format!(
        "init={} quit={} mask={mask:#x} host={} count={} rr={} wake={} {}",
        g(INIT_COUNT, 4),
        g(QUIT_FLAG, 4),
        host.map(|v| format!("{v:#x}")).unwrap_or_else(|| "REJECTED".into()),
        g(DECODER_COUNT, 4),
        g(ROUND_ROBIN, 4),
        g(WAKE_TIME, 4),
        tl
    );
    let arr = g(DECODERS, 8) as usize;
    let n = (g(DECODER_COUNT, 4) as usize).min(MAX_DECODERS);
    let mut decs = Vec::new();
    for i in 0..n {
        let d = read(arr + i * 8, 8).unwrap_or(0) as usize;
        if d == 0 {
            decs.push(format!("dec[{i}]=0"));
            continue;
        }
        let parts: Vec<String> = FIELDS.iter().map(|(k, off, len)| match read(d + off, *len) {
            Some(v) => format!("{k}={v:#x}"),
            None => format!("{k}=?"),
        }).collect();
        decs.push(format!("dec[{i}]@{d:#x} {}", parts.join(" ")));
    }
    (head, decs, cycles)
}

pub fn enabled() -> bool {
    std::env::var(VAR).ok().map(|v| v.trim() != "0" && !v.trim().is_empty()).unwrap_or(false)
}

pub fn start() {
    if !enabled() {
        return;
    }
    let is_title = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(TITLE_EXE))).unwrap_or(false);
    if !is_title {
        return;
    }
    let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
    if base == 0 {
        return;
    }
    let period = std::env::var(VAR).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v >= 100).unwrap_or(1000);
    let _ = std::thread::Builder::new().name("tombox-webm-probe".into()).spawn(move || {
        crate::record_raw("webm_probe", "start", &format!("base={base:#x} period={period}ms"));
        let mut last_head = String::new();
        let mut last_decs: Vec<String> = Vec::new();
        let mut last_cycles = 0u64;
        let mut last_running: Option<bool> = None;
        let mut lines = 0u32;
        for tick in 0..(240_000 / period) {
            std::thread::sleep(std::time::Duration::from_millis(period));
            let (head, decs, cycles) = snapshot(base);
            let ran = if last_cycles == 0 { 0 } else { cycles.saturating_sub(last_cycles) };
            let running = ran > 0;
            if head != last_head || (last_cycles != 0 && last_running != Some(running)) {
                if last_cycles != 0 {
                    last_running = Some(running);
                }
                crate::record_raw("webm_probe", "state", &format!("t={}ms {head} cycles_delta={ran}", (tick + 1) * period));
                last_head = head;
                lines += 1;
            }
            for (i, d) in decs.iter().enumerate() {
                if last_decs.get(i) != Some(d) {
                    crate::record_raw("webm_probe", "decoder", &format!("t={}ms cycles_delta={ran} {d}", (tick + 1) * period));
                    lines += 1;
                }
            }
            last_decs = decs;
            last_cycles = cycles;
            if lines > 600 {
                crate::record_raw("webm_probe", "stop", "line cap reached");
                break;
            }
        }
    });
}
