use compat_common::record;
use core::ffi::c_void;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);
const IID_DEVICE: Guid = Guid(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
const IID_ALLOCATOR: Guid = Guid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
const IID_GFX_LIST: Guid = Guid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
const IID_FENCE: Guid = Guid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
const IID_RESOURCE: Guid = Guid(0x696442be, 0xa72e, 0x4059, [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad]);

pub const ST_COMMON: u32 = 0;
pub const ST_RENDER_TARGET: u32 = 0x4;
pub const ST_DEPTH_WRITE: u32 = 0x10;
pub const ST_DEPTH_READ: u32 = 0x20;
pub const ST_COPY_DEST: u32 = 0x400;
pub const ST_COPY_SOURCE: u32 = 0x800;
pub const ST_RESOLVE_DEST: u32 = 0x1000;
pub const ST_RESOLVE_SOURCE: u32 = 0x2000;

const PC_CLOSE: usize = 9;
const PC_RESET: usize = 10;
const PC_COPY_TEXTURE_REGION: usize = 16;
const PC_RESOLVE: usize = 19;
const PC_BARRIER: usize = 26;

unsafe fn vt(o: *mut c_void) -> *const usize {
    *(o as *const *const usize)
}
macro_rules! call {
    ($obj:expr, $slot:expr, fn($($at:ty),*) -> $rt:ty $(, $arg:expr)*) => {{
        let o = $obj as *mut c_void;
        let f: unsafe extern "system" fn(*mut c_void $(, $at)*) -> $rt = core::mem::transmute(*vt(o).add($slot));
        f(o $(, $arg)*)
    }};
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sel {
    pub secs: Vec<f64>,
    pub presents: Vec<u64>,
    pub file: bool,
    pub frames: u32,
    pub budget_mb: u64,
    pub max_caps: usize,
    pub chain_every: u64,
    pub all: bool,
    pub raw: bool,
    pub unknown: Vec<String>,
}

pub fn parse(v: &str) -> Option<Sel> {
    let v = v.trim();
    if v.is_empty() || v == "0" {
        return None;
    }
    let mut s = Sel { secs: Vec::new(), presents: Vec::new(), file: false, frames: 1, budget_mb: 1200, max_caps: 400, chain_every: 600, all: false, raw: false, unknown: Vec::new() };
    for tok in v.split([',', ';', ' ']).map(str::trim).filter(|t| !t.is_empty()) {
        let (k, val) = match tok.find([':', '=']) {
            Some(i) => (tok[..i].to_ascii_lowercase(), Some(tok[i + 1..].trim())),
            None => (tok.to_ascii_lowercase(), None),
        };
        let num = |x: Option<&str>| x.and_then(|x| x.parse::<f64>().ok()).filter(|f| f.is_finite() && *f >= 0.0);
        let int = |x: Option<&str>| x.and_then(|x| x.parse::<u64>().ok());
        let ok = match (k.as_str(), val) {
            ("t" | "s" | "sec", v) => num(v).map(|f| s.secs.push(f)).is_some(),
            ("f" | "frame" | "present", v) => int(v).map(|n| s.presents.push(n)).is_some(),
            ("n" | "frames", v) => int(v).map(|n| s.frames = n.clamp(1, 8) as u32).is_some(),
            ("mb", v) => int(v).map(|n| s.budget_mb = n.max(16)).is_some(),
            ("caps", v) => int(v).map(|n| s.max_caps = (n as usize).clamp(1, 4096)).is_some(),
            ("chain", v) => int(v).map(|n| s.chain_every = n).is_some(),
            ("file" | "1" | "on", None) => {
                s.file = true;
                true
            }
            ("all", None) => {
                s.all = true;
                true
            }
            ("raw", None) => {
                s.raw = true;
                true
            }
            (_, None) => num(Some(k.as_str())).map(|f| s.secs.push(f)).is_some(),
            _ => false,
        };
        if !ok {
            s.unknown.push(tok.to_string());
        }
    }
    s.secs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s.presents.sort();
    Some(s)
}

pub fn take_due<T: PartialOrd + Copy>(list: &[T], idx: &mut usize, now: T) -> bool {
    let mut hit = false;
    while *idx < list.len() && list[*idx] <= now {
        *idx += 1;
        hit = true;
    }
    hit
}

pub fn sel() -> Option<&'static Sel> {
    static S: OnceLock<Option<Sel>> = OnceLock::new();
    S.get_or_init(|| std::env::var("TOMBOX_PASSCAP").ok().and_then(|v| parse(&v))).as_ref()
}

pub fn on() -> bool {
    sel().is_some()
}

pub fn typed_format(fmt: u32) -> u32 {
    match fmt {
        1 => 2,
        5 => 6,
        9 => 10,
        15 => 16,
        19 => 20,
        23 => 24,
        27 => 28,
        33 => 34,
        39 => 41,
        44 => 45,
        48 => 49,
        53 => 56,
        60 => 61,
        90 => 87,
        92 => 88,
        f => f,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    pub ch: usize,
    pub min: [f32; 4],
    pub max: [f32; 4],
    pub mean: [f64; 4],
    pub nan: u64,
    pub inf: u64,
    pub zero: [f64; 4],
    pub zero_px: f64,
    pub pixels: u64,
}

impl Stats {
    pub fn black(&self) -> bool {
        self.pixels > 0 && self.zero_px >= 1.0
    }
    pub fn uniform(&self) -> bool {
        self.pixels > 0 && self.nan == 0 && (0..self.ch).all(|c| self.min[c] == self.max[c])
    }
    pub fn flags(&self) -> String {
        let mut f = Vec::new();
        if self.black() {
            f.push("BLACK");
        }
        if self.nan > 0 {
            f.push("NAN");
        }
        if self.inf > 0 {
            f.push("INF");
        }
        if self.uniform() && !self.black() {
            f.push("UNIFORM");
        }
        f.join(",")
    }
    pub fn line(&self) -> String {
        let names = ["r", "g", "b", "a"];
        let mut s = String::new();
        for c in 0..self.ch {
            let _ = write!(s, "{}[{:.5}..{:.5} mean {:.5} zero {:.3}] ", names[c], self.min[c], self.max[c], self.mean[c], self.zero[c]);
        }
        let _ = write!(s, "nan={} inf={} zero_px={:.4} px={}", self.nan, self.inf, self.zero_px, self.pixels);
        s
    }
}

pub fn stats(kind: &str, ch: usize, data: &[u8], pitch: usize, w: usize, h: usize, bpp: usize) -> Stats {
    let ch = ch.clamp(1, 4);
    let mut s = Stats { ch, min: [f32::MAX; 4], max: [f32::MIN; 4], ..Default::default() };
    let mut sum = [0f64; 4];
    let mut fin = [0u64; 4];
    let mut zero = [0u64; 4];
    let mut zero_px = 0u64;
    let rgb = ch.min(3);
    for y in 0..h {
        let Some(row) = data.get(y * pitch..y * pitch + w * bpp) else { break };
        for x in 0..w {
            let v = crate::rtcap::decode(kind, &row[x * bpp..x * bpp + bpp]);
            let mut all0 = true;
            for c in 0..ch {
                let f = v[c];
                if f.is_nan() {
                    s.nan += 1;
                    all0 = false;
                    continue;
                }
                if f.is_infinite() {
                    s.inf += 1;
                    all0 = false;
                    continue;
                }
                s.min[c] = s.min[c].min(f);
                s.max[c] = s.max[c].max(f);
                sum[c] += f as f64;
                fin[c] += 1;
                if f == 0.0 {
                    zero[c] += 1;
                } else if c < rgb {
                    all0 = false;
                }
            }
            if all0 {
                zero_px += 1;
            }
            s.pixels += 1;
        }
    }
    let n = s.pixels.max(1) as f64;
    for c in 0..ch {
        if fin[c] == 0 {
            s.min[c] = f32::NAN;
            s.max[c] = f32::NAN;
        }
        s.mean[c] = sum[c] / fin[c].max(1) as f64;
        s.zero[c] = zero[c] as f64 / n;
    }
    s.zero_px = zero_px as f64 / n;
    s
}

pub fn preview(kind: &str, ch: usize, is_float: bool, data: &[u8], pitch: usize, w: usize, h: usize, bpp: usize, st: &Stats) -> (usize, usize, Vec<u8>) {
    let step = w.div_ceil(960).max(1);
    let (pw, ph) = (w.div_ceil(step), h.div_ceil(step));
    let mut rgb = vec![0u8; pw * ph * 3];
    let hi = st.max[0];
    let lo = if st.min[0] > 0.0 { st.min[0] } else { hi * 1e-6 };
    for py in 0..ph {
        let y = (py * step).min(h - 1);
        let Some(row) = data.get(y * pitch..y * pitch + w * bpp) else { break };
        for px in 0..pw {
            let x = (px * step).min(w - 1);
            let v = crate::rtcap::decode(kind, &row[x * bpp..x * bpp + bpp]);
            let c3 = if ch == 1 {
                let t = if v[0] > 0.0 && hi > lo && lo > 0.0 { 0.15 + 0.85 * (v[0] / lo).ln() / (hi / lo).ln() } else if v[0] > 0.0 { 1.0 } else { 0.0 };
                [t, t, t]
            } else if ch == 2 {
                [v[0], v[1], 0.0]
            } else {
                [v[0], v[1], v[2]]
            };
            for c in 0..3 {
                let f = c3[c];
                let t = if f.is_nan() {
                    if c == 0 { 1.0 } else { 0.0 }
                } else if is_float && ch > 1 {
                    let m = f.max(0.0);
                    (m / (1.0 + m)).powf(1.0 / 2.2)
                } else {
                    f.clamp(0.0, 1.0)
                };
                rgb[(py * pw + px) * 3 + c] = (t.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
    }
    (pw, ph, rgb)
}

#[derive(Clone, Debug)]
pub struct Cap {
    pub idx: u32,
    pub slot: String,
    pub res: usize,
    pub label: String,
    pub w: usize,
    pub h: usize,
    pub fmt: u32,
    pub read_fmt: u32,
    pub samples: u32,
    pub state: u32,
    pub state_src: &'static str,
    pub rb: usize,
    pub tmp: usize,
    pub pitch: usize,
    pub total: u64,
}

unsafe impl Send for Cap {}

pub unsafe fn desc_of(res: usize) -> [u8; 0x38] {
    let mut d = [0u8; 0x38];
    call!(res, 10, fn(*mut u8) -> *mut u8, d.as_mut_ptr());
    d
}

fn d32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

pub fn label_of(res: usize, d: &[u8; 0x38]) -> String {
    let w = u64::from_le_bytes(d[0x10..0x18].try_into().unwrap());
    let s = d32(d, 0x24);
    format!("{res:#x} {w}x{} fmt={}{} mips={} arr={} flags={:#x}", d32(d, 0x18), d32(d, 0x20), if s > 1 { format!(" msaa{s}") } else { String::new() },
        u16::from_le_bytes([d[0x1e], d[0x1f]]), u16::from_le_bytes([d[0x1c], d[0x1d]]), d32(d, 0x30))
}

fn barrier_sub0(res: usize, before: u32, after: u32) -> [u64; 4] {
    [0, res as u64, (before as u64) << 32, after as u64]
}

unsafe fn committed(dev: usize, heap: u32, desc: &[u8; 0x38], state: u32) -> Result<usize, i32> {
    let hp: [u32; 5] = [heap, 0, 0, 1, 1];
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = call!(dev, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32,
        hp.as_ptr(), 0, desc.as_ptr(), state, core::ptr::null(), &IID_RESOURCE, &mut out);
    if hr < 0 || out.is_null() { Err(hr) } else { Ok(out as usize) }
}

fn buffer_desc(size: u64) -> [u8; 0x38] {
    let mut bd = [0u8; 0x38];
    bd[0..4].copy_from_slice(&1u32.to_le_bytes());
    bd[0x10..0x18].copy_from_slice(&size.to_le_bytes());
    bd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    bd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    bd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    bd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    bd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    bd
}

unsafe fn list_call_barrier(list: usize, b: &[[u64; 4]]) {
    let f: unsafe extern "system" fn(*mut c_void, u32, *const [u64; 4]) = core::mem::transmute(crate::cmdlist_wrap::pc_method(list as *mut c_void, PC_BARRIER));
    f(list as *mut c_void, b.len() as u32, b.as_ptr());
}

pub unsafe fn list_device(list: usize) -> Option<usize> {
    let mut dev: *mut c_void = core::ptr::null_mut();
    if call!(list, 7, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE, &mut dev) < 0 || dev.is_null() {
        return None;
    }
    call!(dev, 2, fn() -> u32);
    Some(dev as usize)
}

pub unsafe fn inject(dev: usize, list: usize, res: usize, state: u32, slot: &str, budget_left: u64) -> Result<Cap, String> {
    let d = desc_of(res);
    let label = label_of(res, &d);
    let (dim, w, h, fmt, samples, flags) = (d32(&d, 0), u64::from_le_bytes(d[0x10..0x18].try_into().unwrap()) as usize, d32(&d, 0x18) as usize, d32(&d, 0x20), d32(&d, 0x24), d32(&d, 0x30));
    if dim != 3 {
        return Err(format!("{label}: dimension {dim} is not a 2D texture"));
    }
    if w == 0 || h == 0 {
        return Err(format!("{label}: empty"));
    }
    let typed = typed_format(fmt);
    let depth = flags & 2 != 0;
    let mut cd = d;
    if samples > 1 {
        if depth {
            return Err(format!("{label}: multisampled depth cannot go through ResolveSubresource; not captured"));
        }
        cd[0x08..0x10].fill(0);
        cd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
        cd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
        cd[0x20..0x24].copy_from_slice(&typed.to_le_bytes());
        cd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        cd[0x28..0x2c].copy_from_slice(&0u32.to_le_bytes());
        cd[0x2c..0x30].copy_from_slice(&0u32.to_le_bytes());
        cd[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());
    }
    let mut fp = [0u8; 32];
    let (mut rows, mut row_size, mut total) = (0u32, 0u64, 0u64);
    call!(dev, 38, fn(*const u8, u32, u32, u64, *mut [u8; 32], *mut u32, *mut u64, *mut u64) -> (),
        cd.as_ptr(), 0, 1, 0, &mut fp, &mut rows, &mut row_size, &mut total);
    if total == 0 || total == u64::MAX {
        return Err(format!("{label}: no copyable footprint"));
    }
    let tmp_bytes = if samples > 1 { total } else { 0 };
    if total + tmp_bytes > budget_left {
        return Err(format!("{label}: budget exhausted ({} MB left)", budget_left >> 20));
    }
    let pitch = u32::from_le_bytes(fp[0x18..0x1c].try_into().unwrap()) as usize;
    let rb = committed(dev, 3, &buffer_desc(total), ST_COPY_DEST).map_err(|hr| format!("{label}: readback buffer hr={hr:#010x}"))?;
    let (src, tmp) = if samples > 1 {
        let tmp = match committed(dev, 1, &cd, ST_RESOLVE_DEST) {
            Ok(t) => t,
            Err(hr) => {
                call!(rb, 2, fn() -> u32);
                return Err(format!("{label}: resolve texture hr={hr:#010x}"));
            }
        };
        if state != ST_RESOLVE_SOURCE {
            list_call_barrier(list, &[barrier_sub0(res, state, ST_RESOLVE_SOURCE)]);
        }
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u32, u32) = core::mem::transmute(crate::cmdlist_wrap::pc_method(list as *mut c_void, PC_RESOLVE));
        f(list as *mut c_void, tmp as *mut c_void, 0, res as *mut c_void, 0, typed);
        let mut back = vec![barrier_sub0(tmp, ST_RESOLVE_DEST, ST_COPY_SOURCE)];
        if state != ST_RESOLVE_SOURCE {
            back.push(barrier_sub0(res, ST_RESOLVE_SOURCE, state));
        }
        list_call_barrier(list, &back);
        (tmp, tmp)
    } else {
        if state != ST_COPY_SOURCE {
            list_call_barrier(list, &[barrier_sub0(res, state, ST_COPY_SOURCE)]);
        }
        (res, 0)
    };
    let mut dst = [0u8; 48];
    dst[0..8].copy_from_slice(&(rb as u64).to_le_bytes());
    dst[8..12].copy_from_slice(&1u32.to_le_bytes());
    dst[16..48].copy_from_slice(&fp);
    let mut sl = [0u8; 48];
    sl[0..8].copy_from_slice(&(src as u64).to_le_bytes());
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) = core::mem::transmute(crate::cmdlist_wrap::pc_method(list as *mut c_void, PC_COPY_TEXTURE_REGION));
    f(list as *mut c_void, dst.as_ptr(), 0, 0, 0, sl.as_ptr(), core::ptr::null());
    if samples <= 1 && state != ST_COPY_SOURCE {
        list_call_barrier(list, &[barrier_sub0(res, ST_COPY_SOURCE, state)]);
    }
    let read_fmt = u32::from_le_bytes(fp[0x08..0x0c].try_into().unwrap());
    Ok(Cap { idx: 0, slot: slot.to_string(), res, label, w, h, fmt, read_fmt, samples, state, state_src: "caller", rb, tmp, pitch, total: total + tmp_bytes })
}

pub fn decode_kind(fmt: u32, read_fmt: u32) -> Option<(usize, bool, usize, &'static str)> {
    crate::rtcap::layout(typed_format(fmt)).or_else(|| crate::rtcap::layout(typed_format(read_fmt)))
}

pub unsafe fn read_cap(c: &Cap) -> Option<Vec<u8>> {
    let mut p: *mut u8 = core::ptr::null_mut();
    if call!(c.rb, 8, fn(u32, *const c_void, *mut *mut u8) -> i32, 0, core::ptr::null(), &mut p) < 0 || p.is_null() {
        return None;
    }
    let v = core::slice::from_raw_parts(p, c.total.min((c.pitch * c.h) as u64) as usize).to_vec();
    call!(c.rb, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
    Some(v)
}

pub unsafe fn release_cap(c: &Cap) {
    if c.rb != 0 {
        call!(c.rb, 2, fn() -> u32);
    }
    if c.tmp != 0 {
        call!(c.tmp, 2, fn() -> u32);
    }
}

#[derive(Default)]
struct LState {
    rts: Vec<usize>,
    dsv: usize,
    draws: u32,
    dispatches: u32,
    indirect: u32,
    clears: Vec<String>,
    pso: usize,
    ps: Vec<u64>,
    states: HashMap<usize, u32>,
    split: Vec<usize>,
    dirty: bool,
    pso_dirty: bool,
    seq: u32,
}

#[derive(Default)]
struct FrameCount {
    draws: u64,
    dispatches: u64,
    indirect: u64,
    resolves: u64,
    copies: u64,
    into_src: u64,
    unresolved: u64,
    binds: u64,
    per_rt: HashMap<usize, u64>,
    to_src: Vec<String>,
}

struct Pass {
    list: usize,
    key: usize,
    order: u64,
    seq: u32,
    end: String,
    draws: u32,
    dispatches: u32,
    indirect: u32,
    clears: Vec<String>,
    ps: Vec<u64>,
    targets: Vec<String>,
    caps: Vec<usize>,
    notes: Vec<String>,
    writes_src: bool,
}

struct Shot {
    id: u32,
    why: String,
    start_present: u64,
    end_present: u64,
    t: f64,
    frames_left: u32,
    passes: Vec<Pass>,
    caps: Vec<Cap>,
    submit: HashMap<usize, (u64, usize)>,
    bytes: u64,
    waits: u32,
    order: u64,
    src: usize,
    src_label: String,
    chain: String,
    budget_note: Option<String>,
    ps_cache: HashMap<usize, u64>,
}

#[derive(Default)]
struct G {
    lists: HashMap<usize, LState>,
    fc: FrameCount,
    shot: Option<Shot>,
    submit_seq: u64,
    last_src: usize,
    last_src_desc: Vec<u8>,
    last_into_src_zero: Option<bool>,
    desc_cache: HashMap<usize, [u8; 0x38]>,
    presents: u64,
    t0: Option<std::time::Instant>,
    sec_idx: usize,
    pres_idx: usize,
    next_id: u32,
    device: usize,
    own: Option<(usize, usize, usize, u64)>,
}

static STATE: Mutex<Option<G>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PREV: [AtomicUsize; 128] = [const { AtomicUsize::new(0) }; 128];

fn lock() -> MutexGuard<'static, Option<G>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<R>(f: impl FnOnce(&mut G) -> R) -> R {
    let mut g = lock();
    f(g.get_or_insert_with(G::default))
}

fn key(list: *mut c_void) -> usize {
    crate::list_split::origin_of(list)
}

fn prev(slot: usize) -> usize {
    PREV[slot].load(Ordering::Relaxed)
}

pub fn out_root() -> PathBuf {
    if let Some(d) = std::env::var_os("TOMBOX_PASSCAP_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    std::env::var_os("TOMBOX_LOG").map(PathBuf::from).and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(std::env::temp_dir).join("passcap")
}

fn cached_label(g: &mut G, r: usize) -> String {
    match r {
        0 => "-".into(),
        usize::MAX => "?unresolved-view".into(),
        r => {
            if !ACTIVE.load(Ordering::Relaxed) && !g.desc_cache.contains_key(&r) {
                return format!("{r:#x}");
            }
            let d = *g.desc_cache.entry(r).or_insert_with(|| unsafe { desc_of(r) });
            label_of(r, &d)
        }
    }
}

unsafe fn list_type(list: usize) -> u32 {
    call!(list, 8, fn() -> u32)
}

fn ps_hash(shot: &mut Shot, pso: usize) -> u64 {
    if pso == 0 {
        return 0;
    }
    *shot.ps_cache.entry(pso).or_insert_with(|| {
        let real = crate::pso_async::real_of(pso);
        let sum = crate::device_wrap::pso_summary(pso).or_else(|| if real != 0 { crate::device_wrap::pso_summary(real) } else { None });
        let missing = if sum.is_none() { 1 } else { 2 };
        sum.and_then(|(_, p, _)| if p == 0 { None } else { crate::shader_xlat::origin_hash(p) }).unwrap_or(missing)
    })
}

fn bound_state(st: &LState, res: usize, depth: bool) -> (u32, &'static str) {
    if let Some(&s) = st.states.get(&res) {
        return (s, "in-list barrier");
    }
    if depth {
        let g = crate::gpu_va::state(res);
        if g & ST_DEPTH_READ != 0 && g & ST_DEPTH_WRITE == 0 {
            return (ST_DEPTH_READ, "gpu_va global state");
        }
        return (ST_DEPTH_WRITE, "default for a bound DSV");
    }
    (ST_RENDER_TARGET, "default for a bound RT")
}

fn budget_left(shot: &Shot) -> u64 {
    let s = sel().map(|s| s.budget_mb).unwrap_or(1200) << 20;
    s.saturating_sub(shot.bytes)
}

unsafe fn capture_into(g: &mut G, list: usize, res: usize, state: u32, state_src: &'static str, slot: &str) -> Result<usize, String> {
    if g.device == 0 {
        g.device = list_device(list).ok_or("GetDevice failed")?;
    }
    let dev = g.device;
    let shot = g.shot.as_mut().ok_or("no shot")?;
    let max = sel().map(|s| s.max_caps).unwrap_or(400);
    if shot.caps.len() >= max {
        let n = format!("capture count limit {max} reached");
        shot.budget_note.get_or_insert(n.clone());
        return Err(n);
    }
    let mut c = inject(dev, list, res, state, slot, budget_left(shot)).inspect_err(|e| {
        if e.contains("budget") {
            shot.budget_note.get_or_insert(e.clone());
        }
    })?;
    c.idx = shot.caps.len() as u32;
    c.state_src = state_src;
    shot.bytes += c.total;
    shot.caps.push(c);
    Ok(shot.caps.len() - 1)
}

unsafe fn boundary(g: &mut G, list: *mut c_void, end: &str) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let k = key(list);
    let Some(st) = g.lists.get(&k) else { return };
    if st.rts.iter().all(|r| *r == 0) && st.dsv == 0 && st.draws == 0 && st.dispatches == 0 && st.indirect == 0 {
        return;
    }
    let all = sel().map(|s| s.all).unwrap_or(false);
    let want = st.dirty || all;
    let (rts, dsv) = (st.rts.clone(), st.dsv);
    let mut pass = Pass { list: list as usize, key: k, order: 0, seq: st.seq, end: end.to_string(), draws: st.draws, dispatches: st.dispatches, indirect: st.indirect,
        clears: st.clears.clone(), ps: st.ps.clone(), targets: Vec::new(), caps: Vec::new(), notes: Vec::new(), writes_src: false };
    if st.pso_dirty && st.draws + st.indirect == 0 {
        pass.notes.push("SetPipelineState with targets bound but no hooked draw: captured anyway; draw count is a lower bound (the title may draw through an unhooked slot)".into());
    }
    let direct = list_type(list as usize) == 0;
    let mut slots: Vec<(String, usize, bool)> = rts.iter().enumerate().map(|(i, r)| (format!("RT{i}"), *r, false)).collect();
    if dsv != 0 {
        slots.push(("DSV".into(), dsv, true));
    }
    for (name, r, depth) in slots {
        let lab = cached_label(g, r);
        pass.targets.push(format!("{name}={lab}"));
        if r == g.last_src && r != 0 {
            pass.writes_src = true;
        }
        if r == 0 || r == usize::MAX || !want {
            continue;
        }
        if !direct {
            pass.notes.push(format!("{name}: list type is not DIRECT; not captured"));
            continue;
        }
        let st = g.lists.get(&k).unwrap();
        if st.split.contains(&r) {
            pass.notes.push(format!("{name}: split barrier pending; not captured"));
            continue;
        }
        let (s, why) = bound_state(st, r, depth);
        match capture_into(g, list as usize, r, s, why, &name) {
            Ok(i) => pass.caps.push(i),
            Err(e) => pass.notes.push(format!("{name}: {e}")),
        }
    }
    let shot = g.shot.as_mut().unwrap();
    shot.order += 1;
    pass.order = shot.order;
    shot.passes.push(pass);
}

fn reset_pass(st: &mut LState) {
    st.draws = 0;
    st.dispatches = 0;
    st.indirect = 0;
    st.clears.clear();
    st.ps.clear();
    st.dirty = false;
    st.pso_dirty = false;
    st.seq += 1;
}

unsafe fn views(n: u32, handles: *const usize, single: bool) -> Vec<usize> {
    let inc = crate::rtcap::rtv_increment();
    (0..(n as usize).min(8)).map(|i| {
        if handles.is_null() {
            return 0;
        }
        let h = if single { (*handles).wrapping_add(i * inc) } else { *handles.add(i) };
        crate::rtcap::view_of(h).unwrap_or(usize::MAX)
    }).collect()
}

unsafe extern "system" fn om_set_targets(this: *mut c_void, n: u32, handles: *const usize, single: i32, dsv: *const usize) {
    let rts = views(n, handles, single != 0);
    let d = if dsv.is_null() { 0 } else { crate::rtcap::view_of(*dsv).unwrap_or(usize::MAX) };
    with(|g| {
        g.fc.binds += 1;
        g.fc.unresolved += rts.iter().filter(|r| **r == usize::MAX).count() as u64 + (d == usize::MAX) as u64;
        for r in rts.iter().chain(core::iter::once(&d)).filter(|r| **r != 0 && **r != usize::MAX && ACTIVE.load(Ordering::Relaxed)) {
            if !g.desc_cache.contains_key(r) {
                let dd = desc_of(*r);
                g.desc_cache.insert(*r, dd);
            }
        }
        let k = key(this);
        let changed = g.lists.get(&k).map(|s| s.rts != rts || s.dsv != d).unwrap_or(true);
        if changed {
            boundary(g, this, "OMSetRenderTargets");
            let st = g.lists.entry(k).or_default();
            reset_pass(st);
            st.rts = rts;
            st.dsv = d;
        }
    });
    let f: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = core::mem::transmute(prev(50));
    f(this, n, handles, single, dsv)
}

fn note_draw(this: *mut c_void, kind: u8) {
    with(|g| {
        let k = key(this);
        let active = ACTIVE.load(Ordering::Relaxed);
        let st = g.lists.entry(k).or_default();
        match kind {
            2 => st.dispatches += 1,
            3 => st.indirect += 1,
            _ => st.draws += 1,
        }
        if kind != 2 {
            st.dirty = true;
        }
        let rts = if kind == 2 { Vec::new() } else { st.rts.clone() };
        let pso = st.pso;
        match kind {
            2 => g.fc.dispatches += 1,
            3 => g.fc.indirect += 1,
            _ => g.fc.draws += 1,
        }
        if kind != 2 {
            if rts.contains(&g.last_src) && g.last_src != 0 {
                g.fc.into_src += 1;
            }
            for r in rts.iter().filter(|r| **r != 0) {
                *g.fc.per_rt.entry(*r).or_default() += 1;
            }
        }
        if active && kind != 2 {
            if let Some(shot) = g.shot.as_mut() {
                let h = ps_hash(shot, pso);
                let st = g.lists.get_mut(&k).unwrap();
                if !st.ps.contains(&h) && st.ps.len() < 12 {
                    st.ps.push(h);
                }
            }
        }
    })
}

unsafe extern "system" fn draw(this: *mut c_void, a: u32, b: u32, c: u32, d: u32) {
    note_draw(this, 0);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = core::mem::transmute(prev(12));
    f(this, a, b, c, d)
}
unsafe extern "system" fn draw_indexed(this: *mut c_void, a: u32, b: u32, c: u32, d: i32, e: u32) {
    note_draw(this, 1);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32) = core::mem::transmute(prev(13));
    f(this, a, b, c, d, e)
}
unsafe extern "system" fn dispatch(this: *mut c_void, a: u32, b: u32, c: u32) {
    note_draw(this, 2);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = core::mem::transmute(prev(14));
    f(this, a, b, c)
}
unsafe extern "system" fn draw_indexed_x(this: *mut c_void, va: u64, fmt: u32, n: u32, inst: u32) {
    note_draw(this, 1);
    let f: unsafe extern "system" fn(*mut c_void, u64, u32, u32, u32) = core::mem::transmute(prev(81));
    f(this, va, fmt, n, inst)
}
unsafe extern "system" fn dispatch_mesh_x(this: *mut c_void, n: u32, a: u64, b: u64, c: u64) {
    note_draw(this, 0);
    let f: unsafe extern "system" fn(*mut c_void, u32, u64, u64, u64) = core::mem::transmute(prev(98));
    f(this, n, a, b, c)
}
unsafe extern "system" fn dispatch_mesh(this: *mut c_void, x: u32, y: u32, z: u32) {
    note_draw(this, 0);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = core::mem::transmute(prev(127));
    f(this, x, y, z)
}
unsafe extern "system" fn indirect(this: *mut c_void, sig: *mut c_void, n: u32, buf: *mut c_void, off: u64, cbuf: *mut c_void, coff: u64) {
    note_draw(this, 3);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u64, *mut c_void, u64) = core::mem::transmute(prev(59));
    f(this, sig, n, buf, off, cbuf, coff)
}

unsafe extern "system" fn set_pso(this: *mut c_void, pso: *mut c_void) {
    with(|g| {
        let st = g.lists.entry(key(this)).or_default();
        st.pso = pso as usize;
        if st.rts.iter().any(|r| *r != 0) || st.dsv != 0 {
            st.pso_dirty = true;
            st.dirty = true;
        }
    });
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = core::mem::transmute(prev(25));
    f(this, pso)
}

unsafe extern "system" fn barrier(this: *mut c_void, n: u32, p: *const u8) {
    if n != 0 && !p.is_null() {
        let fixed = crate::xone_barrier::sanitize(n, p);
        let bytes: Vec<u8> = match fixed {
            Some(v) => v,
            None => core::slice::from_raw_parts(p, n as usize * 32).to_vec(),
        };
        with(|g| {
            let st = g.lists.entry(key(this)).or_default();
            for b in bytes.chunks_exact(32) {
                if d32(b, 0) != 0 {
                    continue;
                }
                let flags = d32(b, 4);
                let res = u64::from_le_bytes(b[8..16].try_into().unwrap()) as usize;
                let sub = d32(b, 16);
                if flags & 1 != 0 {
                    if !st.split.contains(&res) {
                        st.split.push(res);
                    }
                    continue;
                }
                if flags & 2 != 0 {
                    st.split.retain(|r| *r != res);
                }
                if sub == 0 || sub == u32::MAX {
                    st.states.insert(res, d32(b, 24));
                }
            }
        });
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = core::mem::transmute(prev(26));
    f(this, n, p)
}

unsafe extern "system" fn clear_rtv(this: *mut c_void, h: usize, color: *const f32, n: u32, rects: *const c_void) {
    let res = crate::rtcap::view_of(h).unwrap_or(usize::MAX);
    let c = if color.is_null() { [0.0; 4] } else { [*color, *color.add(1), *color.add(2), *color.add(3)] };
    with(|g| {
        let lab = cached_label(g, res);
        let st = g.lists.entry(key(this)).or_default();
        st.dirty = true;
        if st.clears.len() < 8 {
            st.clears.push(format!("clearRTV {lab} ({:.4},{:.4},{:.4},{:.4}) rects={n}", c[0], c[1], c[2], c[3]));
        }
    });
    let f: unsafe extern "system" fn(*mut c_void, usize, *const f32, u32, *const c_void) = core::mem::transmute(prev(52));
    f(this, h, color, n, rects)
}

unsafe extern "system" fn clear_dsv(this: *mut c_void, h: usize, flags: u32, depth: f32, stencil: u8, n: u32, rects: *const c_void) {
    let res = crate::rtcap::view_of(h).unwrap_or(usize::MAX);
    with(|g| {
        let lab = cached_label(g, res);
        let st = g.lists.entry(key(this)).or_default();
        st.dirty = true;
        if st.clears.len() < 8 {
            st.clears.push(format!("clearDSV {lab} flags={flags} depth={depth} stencil={stencil} rects={n}"));
        }
    });
    let f: unsafe extern "system" fn(*mut c_void, usize, u32, f32, u8, u32, *const c_void) = core::mem::transmute(prev(51));
    f(this, h, flags, depth, stencil, n, rects)
}

unsafe fn after_write(this: *mut c_void, what: &str, dst: usize, src: usize, default_state: u32, resolve: bool) {
    with(|g| {
        if resolve {
            g.fc.resolves += 1;
        } else {
            g.fc.copies += 1;
        }
        let into_src = dst == g.last_src && dst != 0;
        let from_src = src == g.last_src && src != 0;
        if (into_src || from_src) && g.fc.to_src.len() < 8 {
            let t = format!("{what} {}{} <- {}{}", if into_src { "*SRC* " } else { "" }, cached_label(g, dst), if from_src { "*SRC* " } else { "" }, cached_label(g, src));
            g.fc.to_src.push(t);
        }
        if !ACTIVE.load(Ordering::Relaxed) || dst == 0 {
            return;
        }
        let d = *g.desc_cache.entry(dst).or_insert_with(|| desc_of(dst));
        if !(into_src || d32(&d, 0x30) & 3 != 0 || sel().map(|s| s.all).unwrap_or(false)) || d32(&d, 0) != 3 {
            return;
        }
        let k = key(this);
        let st = g.lists.entry(k).or_default();
        let (s, why) = match st.states.get(&dst) {
            Some(&s) => (s, "in-list barrier"),
            None => (default_state, "default for the copy/resolve destination"),
        };
        let seq = st.seq;
        let mut pass = Pass { list: this as usize, key: k, order: 0, seq, end: format!("{what} dst={} src={}", cached_label(g, dst), cached_label(g, src)), draws: 0, dispatches: 0, indirect: 0,
            clears: Vec::new(), ps: Vec::new(), targets: vec![format!("dst={}", cached_label(g, dst))], caps: Vec::new(), notes: Vec::new(), writes_src: into_src };
        if list_type(this as usize) != 0 {
            pass.notes.push("list type is not DIRECT; not captured".into());
        } else {
            match capture_into(g, this as usize, dst, s, why, "dst") {
                Ok(i) => pass.caps.push(i),
                Err(e) => pass.notes.push(e),
            }
        }
        let shot = g.shot.as_mut().unwrap();
        shot.order += 1;
        pass.order = shot.order;
        shot.passes.push(pass);
    })
}

unsafe extern "system" fn resolve(this: *mut c_void, dst: *mut c_void, dsub: u32, src: *mut c_void, ssub: u32, fmt: u32) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u32, u32) = core::mem::transmute(prev(19));
    f(this, dst, dsub, src, ssub, fmt);
    after_write(this, &format!("ResolveSubresource(sub {dsub}<-{ssub} fmt={fmt})"), if dsub == 0 { dst as usize } else { 0 }, src as usize, ST_RESOLVE_DEST, true);
}

unsafe fn loc_res(l: *const u8) -> (usize, u32, u32) {
    if l.is_null() {
        return (0, 9, 0);
    }
    (core::ptr::read_unaligned(l as *const u64) as usize, core::ptr::read_unaligned(l.add(8) as *const u32), core::ptr::read_unaligned(l.add(16) as *const u32))
}

unsafe extern "system" fn copy_region(this: *mut c_void, dst: *const u8, x: u32, y: u32, z: u32, src: *const u8, bx: *const c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) = core::mem::transmute(prev(16));
    f(this, dst, x, y, z, src, bx);
    let (d, dt, ds) = loc_res(dst);
    let (s, _, _) = loc_res(src);
    after_write(this, "CopyTextureRegion", if dt == 0 && ds == 0 { d } else { 0 }, s, ST_COPY_DEST, false);
}
unsafe extern "system" fn copy_region_x(this: *mut c_void, dst: *const u8, x: u32, y: u32, z: u32, src: *const u8, bx: *const c_void, flags: u32) {
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void, u32) = core::mem::transmute(prev(83));
    f(this, dst, x, y, z, src, bx, flags);
    let (d, dt, ds) = loc_res(dst);
    let (s, _, _) = loc_res(src);
    after_write(this, "CopyTextureRegionX", if dt == 0 && ds == 0 { d } else { 0 }, s, ST_COPY_DEST, false);
}
unsafe extern "system" fn copy_resource(this: *mut c_void, dst: *mut c_void, src: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) = core::mem::transmute(prev(17));
    f(this, dst, src);
    after_write(this, "CopyResource", dst as usize, src as usize, ST_COPY_DEST, false);
}
unsafe extern "system" fn copy_resource_x(this: *mut c_void, dst: *mut c_void, src: *mut c_void, flags: u32) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, u32) = core::mem::transmute(prev(84));
    f(this, dst, src, flags);
    after_write(this, "CopyResourceX", dst as usize, src as usize, ST_COPY_DEST, false);
}

unsafe extern "system" fn close(this: *mut c_void) -> i32 {
    with(|g| {
        boundary(g, this, "Close");
        if let Some(st) = g.lists.get_mut(&key(this)) {
            reset_pass(st);
        }
    });
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = core::mem::transmute(prev(PC_CLOSE));
    f(this)
}

unsafe extern "system" fn reset(this: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    with(|g| {
        let k = key(this);
        g.lists.remove(&k);
        g.lists.remove(&(this as usize));
        if let Some(s) = g.shot.as_mut() {
            if !s.passes.iter().any(|p| p.list == this as usize) {
                s.submit.remove(&(this as usize));
            }
        }
    });
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = core::mem::transmute(prev(PC_RESET));
    let hr = f(this, alloc, pso);
    if !pso.is_null() {
        with(|g| g.lists.entry(key(this)).or_default().pso = pso as usize);
    }
    hr
}

pub fn install(vt: &[AtomicUsize]) {
    if !on() {
        return;
    }
    let hooks: [(usize, usize); 19] = [
        (9, close as *const () as usize), (10, reset as *const () as usize), (12, draw as *const () as usize), (13, draw_indexed as *const () as usize), (14, dispatch as *const () as usize),
        (16, copy_region as *const () as usize), (17, copy_resource as *const () as usize), (19, resolve as *const () as usize), (25, set_pso as *const () as usize), (26, barrier as *const () as usize),
        (50, om_set_targets as *const () as usize), (51, clear_dsv as *const () as usize), (52, clear_rtv as *const () as usize), (59, indirect as *const () as usize), (83, copy_region_x as *const () as usize),
        (84, copy_resource_x as *const () as usize), (81, draw_indexed_x as *const () as usize), (98, dispatch_mesh_x as *const () as usize),
        (127, dispatch_mesh as *const () as usize),
    ];
    let mut missing = Vec::new();
    for (slot, f) in hooks {
        let p = vt[slot].load(Ordering::Relaxed);
        if p == 0 {
            missing.push(slot);
            continue;
        }
        PREV[slot].store(p, Ordering::Relaxed);
        vt[slot].store(f, Ordering::Relaxed);
    }
    let s = sel().unwrap();
    record("d3d12_xs", "passcap", &format!(
        "TOMBOX_PASSCAP armed: shots at t={:?} s after first present, presents={:?}, file trigger={} ({}\\now or %TEMP%\\tombox_passcap_now), frames/shot={}, budget {} MB / {} captures, chain log every {} presents, output {}; hooks chained on Xbox slots 9,10,12,13,14,16,17,19,25,26,50,51,52,59,81,83,84,98,127{}{} (draws are never skipped or changed; captures add barrier+copy commands only on capture frames)",
        s.secs, s.presents, s.file, out_root().display(), s.frames, s.budget_mb, s.max_caps, s.chain_every, out_root().display(),
        if missing.is_empty() { String::new() } else { format!("; empty slots left alone: {missing:?}") },
        if s.unknown.is_empty() { String::new() } else { format!("; ignored tokens {:?}", s.unknown) }));
}

pub fn note_submit(queue: usize, real: &[*mut c_void]) {
    if !on() {
        return;
    }
    with(|g| {
        let Some(shot) = g.shot.as_mut() else { return };
        for l in real {
            if !shot.submit.contains_key(&(*l as usize)) {
                g.submit_seq += 1;
                shot.submit.insert(*l as usize, (g.submit_seq, queue));
            }
        }
    })
}

unsafe fn plane_text(plane_count: u32, planes: *const u8) -> String {
    if planes.is_null() {
        return format!("planes={plane_count} (null)");
    }
    let words: Vec<String> = (0..(plane_count as usize * 4).clamp(4, 16)).map(|i| format!("{:x}", core::ptr::read_unaligned((planes as *const u64).add(i)))).collect();
    let n = core::ptr::read_unaligned(planes.add(8) as *const u32);
    let pp = core::ptr::read_unaligned(planes.add(0x10) as *const *const usize);
    let res: Vec<String> = if pp.is_null() { Vec::new() } else { (0..(n as usize).min(4)).map(|i| format!("{:#x}", *pp.add(i))).collect() };
    format!("planes={plane_count} plane0 res_count={n} res=[{}] raw=[{}]", res.join(" "), words.join(" "))
}

fn chain_line(g: &mut G, src: usize, planes: &str) -> String {
    let fc = core::mem::take(&mut g.fc);
    let mut per: Vec<(usize, u64)> = fc.per_rt.iter().map(|(a, b)| (*a, *b)).collect();
    per.sort_by(|a, b| b.1.cmp(&a.1));
    let known = |r: usize| g.desc_cache.get(&r).map(|d| label_of(r, d)).unwrap_or_else(|| format!("{r:#x}"));
    let top: Vec<String> = per.iter().take(6).map(|(r, c)| format!("{}{} x{c}", if *r == src { "*SRC* " } else { "" }, known(*r))).collect();
    let src_label = label_of(src, &unsafe { desc_of(src) });
    format!("src={} | {planes} | frame: draws={} dispatches={} indirect={} resolves={} copies={} binds={} unresolved_views={} draws_into_src={} | to/from src: [{}] | top rts: [{}]",
        src_label, fc.draws, fc.dispatches, fc.indirect, fc.resolves, fc.copies, fc.binds, fc.unresolved, fc.into_src, fc.to_src.join("; "), top.join("; "))
}

unsafe fn own_objects(g: &mut G, queue: *mut c_void) -> Option<(usize, usize, usize)> {
    if let Some((a, l, f, _)) = g.own {
        return Some((a, l, f));
    }
    let mut dev: *mut c_void = core::ptr::null_mut();
    if call!(queue, 7, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE, &mut dev) < 0 || dev.is_null() {
        return None;
    }
    let mut a: *mut c_void = core::ptr::null_mut();
    let mut l: *mut c_void = core::ptr::null_mut();
    let mut f: *mut c_void = core::ptr::null_mut();
    let ok = call!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut a) >= 0
        && call!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, a, core::ptr::null_mut(), &IID_GFX_LIST, &mut l) >= 0
        && call!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut f) >= 0;
    if g.device == 0 {
        g.device = dev as usize;
    }
    call!(dev, 2, fn() -> u32);
    if !ok {
        return None;
    }
    call!(l, PC_CLOSE, fn() -> i32);
    g.own = Some((a as usize, l as usize, f as usize, 0));
    Some((a as usize, l as usize, f as usize))
}

unsafe fn wait_fence(f: usize, v: u64, ms: u32) -> bool {
    extern "system" {
        fn Sleep(ms: u32);
    }
    let mut spins = 0;
    while call!(f, 8, fn() -> u64) < v {
        if spins >= ms {
            return false;
        }
        Sleep(1);
        spins += 1;
    }
    true
}

pub unsafe fn on_present(queue: *mut c_void, src: *mut c_void, plane_count: u32, planes: *const u8) {
    let Some(s) = sel() else { return };
    let ptext = plane_text(plane_count, planes);
    let mut run_list: Option<usize> = None;
    let mut finalize: Option<Shot> = None;
    {
        let mut guard = lock();
        let g = guard.get_or_insert_with(G::default);
        g.presents += 1;
        let t0 = *g.t0.get_or_insert_with(std::time::Instant::now);
        let t = t0.elapsed().as_secs_f64();
        let src_u = src as usize;
        let d = desc_of(src_u);
        let changed = src_u != g.last_src || g.last_src_desc != d[..];
        let into_zero = g.fc.into_src == 0;
        let flip = g.last_into_src_zero.map(|z| z != into_zero).unwrap_or(true);
        let periodic = s.chain_every != 0 && g.presents % s.chain_every == 0;
        let recording_end = ACTIVE.load(Ordering::Relaxed) && g.shot.as_ref().map(|sh| sh.frames_left <= 1).unwrap_or(false);
        static LINES: AtomicUsize = AtomicUsize::new(0);
        let quota = changed || periodic || recording_end || LINES.load(Ordering::Relaxed) < 400;
        if (changed || flip || periodic || recording_end) && quota && LINES.fetch_add(1, Ordering::Relaxed) < 4000 {
            let line = chain_line(g, src_u, &ptext);
            if periodic {
                g.desc_cache.clear();
            }
            record("d3d12_xs", "passcap.chain", &format!("present #{} t={t:.2}s{}{} {line}", g.presents,
                if changed { " SRC-CHANGED" } else { "" }, if flip { if into_zero { " DRAWS-INTO-SRC-STOPPED" } else { " DRAWS-INTO-SRC-STARTED" } } else { "" }));
            if recording_end {
                if let Some(sh) = g.shot.as_mut() {
                    sh.chain = line;
                }
            }
        } else {
            g.fc = FrameCount::default();
        }
        g.last_into_src_zero = Some(into_zero);
        g.last_src = src_u;
        g.last_src_desc = d.to_vec();
        if ACTIVE.load(Ordering::Relaxed) {
            let done = g.shot.as_mut().map(|sh| {
                sh.frames_left = sh.frames_left.saturating_sub(1);
                sh.frames_left == 0
            }).unwrap_or(true);
            if done {
                ACTIVE.store(false, Ordering::Relaxed);
                if let Some((a, l, _)) = own_objects(g, queue) {
                    let lab = label_of(src_u, &d);
                    call!(a, 8, fn() -> i32);
                    call!(l, PC_RESET, fn(*mut c_void, *mut c_void) -> i32, a as *mut c_void, core::ptr::null_mut());
                    let dev = g.device;
                    let sh = g.shot.as_mut().unwrap();
                    sh.end_present = g.presents;
                    sh.src = src_u;
                    sh.src_label = lab.clone();
                    let mut pass = Pass { list: l, key: l, order: 0, seq: 0, end: "PresentX (title src, state COMMON) -> present.rs CopyResource into the swapchain buffer".into(), draws: 0, dispatches: 0, indirect: 0,
                        clears: Vec::new(), ps: Vec::new(), targets: vec![format!("src={lab}")], caps: Vec::new(), notes: Vec::new(), writes_src: true };
                    let left = budget_left(sh) + (256 << 20);
                    match inject(dev, l, src_u, ST_COMMON, "PRESENT", left) {
                        Ok(mut c) => {
                            c.idx = sh.caps.len() as u32;
                            sh.bytes += c.total;
                            sh.caps.push(c);
                            pass.caps.push(sh.caps.len() - 1);
                        }
                        Err(e) => pass.notes.push(e),
                    }
                    sh.order += 1;
                    pass.order = sh.order;
                    sh.passes.push(pass);
                    call!(l, PC_CLOSE, fn() -> i32);
                    run_list = Some(l);
                }
            }
        }
        if !ACTIVE.load(Ordering::Relaxed) {
            if let Some(sh) = g.shot.as_mut() {
                if sh.end_present != 0 && run_list.is_none() {
                    let lists: Vec<usize> = sh.passes.iter().filter(|p| !p.caps.is_empty()).map(|p| p.list).collect();
                    let pending = lists.iter().filter(|l| !sh.submit.contains_key(l)).count();
                    sh.waits += 1;
                    if pending == 0 || sh.waits > 8 {
                        finalize = g.shot.take();
                    }
                }
            }
        }
        if g.shot.is_none() && !ACTIVE.load(Ordering::Relaxed) {
            let mut why = None;
            if take_due(&s.secs, &mut g.sec_idx, t) {
                why = Some(format!("t>={:.1}s after first present", s.secs[g.sec_idx - 1]));
            }
            if take_due(&s.presents, &mut g.pres_idx, g.presents) {
                why = Some(format!("present #{}", s.presents[g.pres_idx - 1]));
            }
            if s.file {
                for p in [out_root().join("now"), std::env::temp_dir().join("tombox_passcap_now")] {
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                        why = Some(format!("trigger file {}", p.display()));
                    }
                }
            }
            if let Some(w) = why {
                g.next_id += 1;
                g.shot = Some(Shot { id: g.next_id, why: w.clone(), start_present: g.presents, end_present: 0, t, frames_left: s.frames, passes: Vec::new(), caps: Vec::new(), submit: HashMap::new(),
                    bytes: 0, waits: 0, order: 0, src: 0, src_label: String::new(), chain: String::new(), budget_note: None, ps_cache: HashMap::new() });
                ACTIVE.store(true, Ordering::Relaxed);
                record("d3d12_xs", "passcap", &format!("shot {} armed ({w}) at present #{} t={t:.2}s: recording the next {} frame(s)", g.next_id, g.presents, s.frames));
            }
        }
    }
    if let Some(l) = run_list {
        let lists = [l as *mut c_void];
        call!(queue, 10, fn(u32, *const *mut c_void) -> (), 1, lists.as_ptr());
    }
    if let Some(sh) = finalize {
        finish(queue as usize, sh);
    }
}

unsafe fn finish(present_queue: usize, mut sh: Shot) {
    let fence = with(|g| g.own.map(|o| o.2));
    let mut queues: Vec<usize> = sh.submit.values().map(|v| v.1).collect();
    queues.push(present_queue);
    queues.sort();
    queues.dedup();
    let mut waited = Vec::new();
    if let Some(f) = fence {
        for q in &queues {
            let v = with(|g| {
                let o = g.own.as_mut().unwrap();
                o.3 += 1;
                o.3
            });
            call!(*q, 14, fn(*mut c_void, u64) -> i32, f as *mut c_void, v);
            waited.push((*q, wait_fence(f, v, 10_000)));
        }
    }
    let ok = !waited.is_empty() && waited.iter().all(|w| w.1);
    let raw = sel().map(|s| s.raw).unwrap_or(false);
    for p in sh.passes.iter_mut() {
        if !sh.submit.contains_key(&p.list) && !p.caps.is_empty() && p.list != p.key {
            if let Some(v) = sh.submit.get(&p.key) {
                sh.submit.insert(p.list, *v);
            }
        }
    }
    let _ = std::thread::Builder::new().name("tombox-passcap".into()).spawn(move || write_shot(sh, ok, waited, raw));
}

fn write_shot(sh: Shot, gpu_done: bool, waited: Vec<(usize, bool)>, raw: bool) {
    let dir = out_root().join(format!("shot{:02}_p{}", sh.id, sh.start_present));
    let _ = std::fs::create_dir_all(&dir);
    let mut order: Vec<usize> = (0..sh.passes.len()).collect();
    let present_list = sh.passes.last().map(|p| p.list).unwrap_or(0);
    let exec = |p: &Pass| -> u64 {
        if p.list == present_list && p.end.starts_with("PresentX") {
            return u64::MAX - 1;
        }
        sh.submit.get(&p.list).map(|v| v.0).unwrap_or(u64::MAX)
    };
    order.sort_by_key(|&i| (exec(&sh.passes[i]), sh.passes[i].order));
    let mut results: Vec<Option<(Stats, String)>> = vec![None; sh.caps.len()];
    for c in &sh.caps {
        let slotname: String = c.slot.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect();
        let stem = format!("{:04}_{}_{}x{}_fmt{}", c.idx, slotname, c.w, c.h, c.fmt);
        let res = if !gpu_done {
            None
        } else {
            unsafe { read_cap(c) }
        };
        let text = match (res, decode_kind(c.fmt, c.read_fmt)) {
            (None, _) => (Stats::default(), if gpu_done { "readback Map failed".to_string() } else { "GPU fence wait timed out; not read".to_string() }),
            (Some(data), None) => {
                if raw {
                    let _ = std::fs::write(dir.join(format!("{stem}.raw")), &data);
                }
                (Stats::default(), format!("format {} (copy fmt {}) not decoded; pitch {}", c.fmt, c.read_fmt, c.pitch))
            }
            (Some(data), Some((ch, is_float, bpp, kind))) => {
                let st = stats(kind, ch, &data, c.pitch, c.w, c.h, bpp);
                let (pw, ph, rgb) = preview(kind, ch, is_float, &data, c.pitch, c.w, c.h, bpp, &st);
                let png = compat_common::gdk::user::png_rgb(pw as u32, ph as u32, &rgb);
                let _ = std::fs::write(dir.join(format!("{stem}.png")), png);
                if raw {
                    let mut packed = Vec::with_capacity(c.w * c.h * bpp);
                    for y in 0..c.h {
                        if let Some(r) = data.get(y * c.pitch..y * c.pitch + c.w * bpp) {
                            packed.extend_from_slice(r);
                        }
                    }
                    let _ = std::fs::write(dir.join(format!("{stem}.raw")), packed);
                }
                (st, format!("{kind} {} -> {stem}.png", st.line()))
            }
        };
        results[c.idx as usize] = Some(text);
        unsafe { release_cap(c) };
    }
    let mut out = String::new();
    let _ = writeln!(out, "passcap shot {} ({}) t={:.2}s: recorded presents #{}..#{} | GPU complete={gpu_done} queues waited={:x?}", sh.id, sh.why, sh.t, sh.start_present, sh.end_present, waited);
    let _ = writeln!(out, "presented src: {} -> present.rs present_frame: CopyResource(swapchain buffer, src), then overlay; the swapchain only ever shows src", sh.src_label);
    let _ = writeln!(out, "last frame chain: {}", sh.chain);
    if let Some(b) = &sh.budget_note {
        let _ = writeln!(out, "BUDGET: {b} (later passes logged without captures)");
    }
    let _ = writeln!(out, "{} passes, {} captures, {} MB; order = GPU execution order (submission index, then recording order); exec ? = list never submitted", sh.passes.len(), sh.caps.len(), sh.bytes >> 20);
    let mut first_black = None;
    let mut first_nan = None;
    let mut lit: HashMap<usize, bool> = HashMap::new();
    let mut first_dark_after_lit = None;
    for &i in &order {
        let p = &sh.passes[i];
        for &ci in &p.caps {
            let c = &sh.caps[ci];
            if let Some((st, _)) = &results[ci] {
                if st.pixels == 0 {
                    continue;
                }
                let tag = format!("cap {:04} {} {} (pass order {})", ci, c.slot, c.label, p.order);
                if st.black() && first_black.is_none() && p.draws + p.indirect > 0 {
                    first_black = Some(tag.clone());
                }
                if st.nan > 0 && first_nan.is_none() {
                    first_nan = Some(tag.clone());
                }
                let was = lit.get(&c.res).copied().unwrap_or(false);
                if was && st.black() && first_dark_after_lit.is_none() {
                    first_dark_after_lit = Some(tag.clone());
                }
                if !st.black() {
                    lit.insert(c.res, true);
                }
            }
        }
    }
    let _ = writeln!(out, "first BLACK target after a pass with draws: {}", first_black.unwrap_or_else(|| "none".into()));
    let _ = writeln!(out, "first NaN target: {}", first_nan.unwrap_or_else(|| "none".into()));
    let _ = writeln!(out, "first target that had content earlier in the frame and is now BLACK: {}", first_dark_after_lit.unwrap_or_else(|| "none".into()));
    let _ = writeln!(out);
    for &i in &order {
        let p = &sh.passes[i];
        let e = exec(p);
        let es = if e == u64::MAX { "?".to_string() } else if e == u64::MAX - 1 { "present".to_string() } else { e.to_string() };
        let ps: Vec<String> = p.ps.iter().map(|h| format!("{h:016x}")).collect();
        let _ = writeln!(out, "[exec {es} order {:04}] list={:#x}{} pass#{} end={} draws={} indirect={} dispatches={}{} ps=[{}]", p.order, p.list,
            if p.key != p.list { format!(" (origin {:#x})", p.key) } else { String::new() }, p.seq, p.end, p.draws, p.indirect, p.dispatches,
            if p.writes_src { " WRITES-PRESENTED-SRC" } else { "" }, ps.join(","));
        for t in &p.targets {
            let _ = writeln!(out, "    target {t}");
        }
        for c in &p.clears {
            let _ = writeln!(out, "    {c}");
        }
        for &ci in &p.caps {
            let c = &sh.caps[ci];
            let (st, txt) = results[ci].clone().unwrap_or_default();
            let _ = writeln!(out, "    cap {:04} {} state={:#x} ({}){} {} {}", ci, c.slot, c.state, c.state_src, if c.samples > 1 { format!(" msaa{}x->resolved(sample avg)", c.samples) } else { String::new() }, st.flags(), txt);
        }
        for n in &p.notes {
            let _ = writeln!(out, "    note {n}");
        }
    }
    let _ = std::fs::write(dir.join("passes.txt"), &out);
    record("d3d12_xs", "passcap", &format!("shot {} written: {} passes, {} captures -> {}", sh.id, sh.passes.len(), sh.caps.len(), dir.join("passes.txt").display()));
}
