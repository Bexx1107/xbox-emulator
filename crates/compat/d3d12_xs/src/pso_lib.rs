use compat_common::record;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, RwLock};
use crate::device_wrap::Guid;

const MAGIC: &[u8; 4] = b"TBPL";
const FILE_VERSION: u32 = 1;
const KEY_VERSION: &[u8] = b"pso-lib-key v1";
const GFX_DESC: usize = 0x290;
const CS_DESC: usize = 0x38;
const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
const ADAPTER_NOT_FOUND: i32 = 0x887e_0001_u32 as i32;
const DRIVER_MISMATCH: i32 = 0x887e_0002_u32 as i32;
const SAVE_EVERY: u64 = 64;
const SAVE_SECS: u64 = 20;
const CACHED_SALT: u128 = 0x7462_706c_6361_6368_6564_7073_6f00_0001;

const IID_DEVICE1: Guid = Guid { d1: 0x77ac_ce80, d2: 0x638e, d3: 0x4e65, d4: [0x88, 0x95, 0xc1, 0xf2, 0x33, 0x86, 0x86, 0x3e] };
const IID_LIBRARY1: Guid = Guid { d1: 0x80ea_bf42, d2: 0x2568, d3: 0x4e5e, d4: [0xbd, 0x82, 0xc3, 0x7f, 0x86, 0x96, 0x1d, 0xc3] };

const DEV1_CREATE_LIBRARY: usize = 44;
const LIB_STORE: usize = 8;
const LIB_LOAD_GFX: usize = 9;
const LIB_LOAD_CS: usize = 10;
const LIB_SIZE: usize = 11;
const LIB_SERIALIZE: usize = 12;
const LIB_LOAD_STREAM: usize = 13;
const PSO_CACHED_BLOB: usize = 8;

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORED: AtomicU64 = AtomicU64::new(0);
static REJECTED: AtomicU64 = AtomicU64::new(0);
static UNKEYED: AtomicU64 = AtomicU64::new(0);
static CACHED_HITS: AtomicU64 = AtomicU64::new(0);
static CACHED_MISSES: AtomicU64 = AtomicU64::new(0);
static SAVED_AT: AtomicU64 = AtomicU64::new(0);
static SAVING: AtomicBool = AtomicBool::new(false);
static PREV_RS: AtomicUsize = AtomicUsize::new(0);

pub fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        let on = std::env::var("TOMBOX_PSO_LIBRARY").map(|v| v.trim() != "0").unwrap_or(true);
        if !on {
            record("d3d12_xs", "pso_lib", "TOMBOX_PSO_LIBRARY=0: no persistent driver PSO cache, every PSO is compiled by the driver");
        }
        on
    })
}

unsafe fn vslot(obj: *mut c_void, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

unsafe fn release(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(obj, 2)) };
    unsafe { f(obj) };
}

pub fn fnv64(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

struct Hasher(u64, u64);

impl Hasher {
    fn new() -> Hasher {
        let mut h = Hasher(0xcbf2_9ce4_8422_2325, 0x84222325_cbf29ce4 ^ 0x9e37_79b9_7f4a_7c15);
        h.feed(KEY_VERSION);
        h
    }

    fn feed(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
            self.1 = (self.1 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let n = b.len() as u64;
        self.0 = (self.0 ^ n).wrapping_mul(0x0000_0100_0000_01b3);
        self.1 = (self.1 ^ n).wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn done(self) -> u128 {
        ((self.0 as u128) << 64) | self.1 as u128
    }
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

unsafe fn bytes<'a>(p: u64, n: usize) -> &'a [u8] {
    if p == 0 || n == 0 { &[] } else { unsafe { core::slice::from_raw_parts(p as *const u8, n) } }
}

unsafe fn cstr<'a>(p: u64) -> Option<&'a [u8]> {
    if p == 0 {
        return None;
    }
    let s = p as *const u8;
    let mut n = 0;
    while n < 256 {
        if unsafe { *s.add(n) } == 0 {
            return Some(unsafe { core::slice::from_raw_parts(s, n) });
        }
        n += 1;
    }
    None
}

pub unsafe fn gfx_key(desc: &[u8], root: u64, mode: &str) -> Option<u128> {
    if desc.len() < GFX_DESC {
        return None;
    }
    let mut h = Hasher::new();
    h.feed(b"gfx");
    h.feed(mode.as_bytes());
    let mut d = desc[..GFX_DESC].to_vec();
    for o in [0x00, 0x08, 0x18, 0x28, 0x38, 0x48, 0x58, 0x68, 0x228, 0x278, 0x280] {
        d[o..o + 8].fill(0);
    }
    h.feed(&d);
    h.feed(&root.to_le_bytes());
    for o in [0x08, 0x18, 0x28, 0x38, 0x48] {
        let (p, n) = (rd64(desc, o), rd64(desc, o + 8) as usize);
        if n > 64 << 20 {
            return None;
        }
        h.feed(&fnv64(unsafe { bytes(p, n) }).to_le_bytes());
    }
    let (so, so_n) = (rd64(desc, 0x58), rd32(desc, 0x60) as usize);
    if so_n > 64 || (so == 0 && so_n != 0) {
        return None;
    }
    for i in 0..so_n {
        let e = unsafe { bytes(so + (i * 0x18) as u64, 0x18) };
        h.feed(&e[0..4]);
        h.feed(unsafe { cstr(rd64(e, 8)) }?);
        h.feed(&e[16..24]);
    }
    let (st, st_n) = (rd64(desc, 0x68), rd32(desc, 0x70) as usize);
    if st_n > 8 || (st == 0 && st_n != 0) {
        return None;
    }
    h.feed(unsafe { bytes(st, st_n * 4) });
    let (il, il_n) = (rd64(desc, 0x228), rd32(desc, 0x230) as usize);
    if il_n > 64 || (il == 0 && il_n != 0) {
        return None;
    }
    for i in 0..il_n {
        let e = unsafe { bytes(il + (i * 0x20) as u64, 0x20) };
        h.feed(unsafe { cstr(rd64(e, 0)) }?);
        h.feed(&e[8..0x20]);
    }
    Some(h.done())
}

pub unsafe fn compute_key(desc: &[u8], root: u64, mode: &str) -> Option<u128> {
    if desc.len() < CS_DESC {
        return None;
    }
    let mut h = Hasher::new();
    h.feed(b"cs");
    h.feed(mode.as_bytes());
    let mut d = desc[..CS_DESC].to_vec();
    for o in [0x00, 0x08, 0x20, 0x28] {
        d[o..o + 8].fill(0);
    }
    h.feed(&d);
    h.feed(&root.to_le_bytes());
    let (p, n) = (rd64(desc, 0x08), rd64(desc, 0x10) as usize);
    if n > 64 << 20 {
        return None;
    }
    h.feed(&fnv64(unsafe { bytes(p, n) }).to_le_bytes());
    Some(h.done())
}

pub fn stream_key(gfx: u128, extra: &str) -> u128 {
    let mut h = Hasher::new();
    h.feed(b"stream");
    h.feed(&gfx.to_le_bytes());
    h.feed(extra.as_bytes());
    h.done()
}

fn roots() -> &'static Mutex<HashMap<usize, u64>> {
    static R: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn note_root(rs: usize, blob_hash: u64) {
    if rs != 0 {
        roots().lock().unwrap_or_else(|e| e.into_inner()).insert(rs, blob_hash);
    }
}

fn root_hash(rs: u64) -> Option<u64> {
    if rs == 0 {
        return Some(0);
    }
    roots().lock().unwrap_or_else(|e| e.into_inner()).get(&(rs as usize)).copied()
}

unsafe extern "system" fn create_root_signature(this: *mut c_void, mask: u32, blob: *const u8, len: usize, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8, usize, *const c_void, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(PREV_RS.load(Relaxed)) };
    let hr = unsafe { f(this, mask, blob, len, riid, ppv) };
    if hr >= 0 && !ppv.is_null() && !blob.is_null() && len != 0 {
        let mut h = fnv64(unsafe { core::slice::from_raw_parts(blob, len) });
        h ^= mask as u64;
        note_root(unsafe { *ppv } as usize, h);
    }
    hr
}

pub fn hook_device(v: &mut [usize]) {
    if v.len() <= 16 || v[16] == 0 || v[16] == create_root_signature as usize {
        return;
    }
    PREV_RS.store(v[16], Relaxed);
    v[16] = create_root_signature as usize;
}

fn mode_tag() -> &'static str {
    crate::shader_xlat::runtime_mode().cbv_raw.key_tag()
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileCheck<'a> {
    Empty,
    Blob(&'a [u8]),
    Rebuild(String),
}

pub fn check_file<'a>(file: Option<&'a [u8]>, ident: &str) -> FileCheck<'a> {
    let Some(b) = file else { return FileCheck::Empty };
    if b.len() < 12 || &b[0..4] != MAGIC {
        return FileCheck::Rebuild("not a TomBox pipeline library file".into());
    }
    let ver = u32::from_le_bytes(b[4..8].try_into().unwrap());
    if ver != FILE_VERSION {
        return FileCheck::Rebuild(format!("file version {ver}, this build writes {FILE_VERSION}"));
    }
    let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let Some(id) = b.get(12..12 + n) else { return FileCheck::Rebuild("truncated header".into()) };
    if id != ident.as_bytes() {
        return FileCheck::Rebuild(format!("built for [{}], this run is [{ident}]", String::from_utf8_lossy(id)));
    }
    let blob = &b[12 + n..];
    if blob.is_empty() { FileCheck::Empty } else { FileCheck::Blob(blob) }
}

pub fn file_bytes(ident: &str, blob: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + ident.len() + blob.len());
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&FILE_VERSION.to_le_bytes());
    v.extend_from_slice(&(ident.len() as u32).to_le_bytes());
    v.extend_from_slice(ident.as_bytes());
    v.extend_from_slice(blob);
    v
}

#[derive(Debug, PartialEq, Eq)]
pub enum OnCreate {
    Use,
    RebuildEmpty,
    Unavailable,
}

pub fn on_create_hr(hr: i32, had_blob: bool) -> OnCreate {
    match (hr >= 0, had_blob) {
        (true, _) => OnCreate::Use,
        (false, true) => OnCreate::RebuildEmpty,
        (false, false) => OnCreate::Unavailable,
    }
}

struct Lib {
    lib: usize,
    rw: RwLock<()>,
    path: std::path::PathBuf,
    ident: String,
    rejected: Mutex<HashSet<u128>>,
}

unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

static LIB: OnceLock<Option<Lib>> = OnceLock::new();

fn ident() -> String {
    let p = tombox_xshader::fma_probe::probe();
    format!("{} | {}", p.adapter, mode_tag())
}

unsafe fn luid(real: *mut c_void) -> u64 {
    let f: unsafe extern "system" fn(*mut c_void, *mut u64) -> *mut u64 = unsafe { core::mem::transmute(vslot(real, 43)) };
    let mut l = 0u64;
    unsafe { f(real, &mut l) };
    l
}

unsafe fn create_library(dev1: *mut c_void, blob: &[u8]) -> (i32, usize) {
    let f: unsafe extern "system" fn(*mut c_void, *const u8, usize, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev1, DEV1_CREATE_LIBRARY)) };
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { f(dev1, if blob.is_empty() { core::ptr::null() } else { blob.as_ptr() }, blob.len(), &IID_LIBRARY1, &mut out) };
    (hr, out as usize)
}

unsafe fn open(real: *mut c_void) -> Option<Lib> {
    let t0 = std::time::Instant::now();
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(real, 0)) };
    let mut dev1: *mut c_void = core::ptr::null_mut();
    if unsafe { qi(real, &IID_DEVICE1, &mut dev1) } < 0 || dev1.is_null() {
        record("d3d12_xs", "pso_lib", "device has no ID3D12Device1; PSOs fall back to CachedPSO blobs");
        return None;
    }
    let ident = ident();
    let path = tombox_xshader::shader_cache::title_dir(&crate::shader_xlat::title_name_pub()).join("pso_library.bin");
    let file = std::fs::read(&path).ok();
    let check = check_file(file.as_deref(), &ident);
    let (blob, why): (&'static [u8], Option<String>) = match check {
        FileCheck::Empty => (&[], None),
        FileCheck::Rebuild(w) => (&[], Some(w)),
        FileCheck::Blob(b) => (Box::leak(b.to_vec().into_boxed_slice()), None),
    };
    drop(file);
    let (mut hr, mut lib) = unsafe { create_library(dev1, blob) };
    let mut note = why.map(|w| format!("dropped the stored library ({w}); rebuilding"));
    if on_create_hr(hr, !blob.is_empty()) == OnCreate::RebuildEmpty {
        let what = match hr {
            DRIVER_MISMATCH => "driver version changed",
            ADAPTER_NOT_FOUND => "adapter changed",
            E_INVALIDARG => "blob rejected",
            _ => "load failed",
        };
        note = Some(format!("driver rejected the stored library hr={hr:#010x} ({what}); rebuilding"));
        (hr, lib) = unsafe { create_library(dev1, &[]) };
    }
    unsafe { release(dev1) };
    if hr < 0 || lib == 0 {
        record("d3d12_xs", "pso_lib", &format!("CreatePipelineLibrary failed hr={hr:#010x}; PSOs fall back to CachedPSO blobs"));
        return None;
    }
    record("d3d12_xs", "pso_lib", &format!(
        "pipeline library {} ({} bytes loaded in {:.0} ms, adapter luid {:016x}, {}){}; TOMBOX_PSO_LIBRARY=0 disables",
        path.display(), blob.len(), t0.elapsed().as_secs_f64() * 1000.0, unsafe { luid(real) }, ident, note.map(|n| format!("; {n}")).unwrap_or_default()));
    SAVED_AT.store(now_s(), Relaxed);
    register_exit_save();
    Some(Lib { lib, rw: RwLock::new(()), path, ident, rejected: Mutex::new(HashSet::new()) })
}

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn lib(real: *mut c_void) -> Option<&'static Lib> {
    LIB.get_or_init(|| unsafe { open(real) }).as_ref()
}

pub fn warm(real: *mut c_void) {
    if !enabled() || real.is_null() || LIB.get().is_some() {
        return;
    }
    let r = real as usize;
    let _ = std::thread::Builder::new().name("tombox-pso-lib".into()).spawn(move || {
        let _ = lib(r as *mut c_void);
    });
}

fn wide(k: u128) -> [u16; 33] {
    let mut w = [0u16; 33];
    for (i, c) in format!("{k:032x}").bytes().enumerate() {
        w[i] = c as u16;
    }
    w
}

fn note_counts() {
    let n = HITS.load(Relaxed) + MISSES.load(Relaxed);
    if n == 1 || n % 200 == 0 {
        record("d3d12_xs", "pso_lib", &summary());
    }
}

pub fn summary() -> String {
    format!("pipeline library: {} hits, {} misses (compiled by the driver), {} stored, {} rejected loads, {} unkeyed; CachedPSO fallback {} hits {} misses",
        HITS.load(Relaxed), MISSES.load(Relaxed), STORED.load(Relaxed), REJECTED.load(Relaxed), UNKEYED.load(Relaxed), CACHED_HITS.load(Relaxed), CACHED_MISSES.load(Relaxed))
}

unsafe fn try_load(l: &Lib, k: u128, load: &dyn Fn(usize, *const u16) -> i32) -> bool {
    if l.rejected.lock().unwrap_or_else(|e| e.into_inner()).contains(&k) {
        return false;
    }
    let w = wide(k);
    let _r = l.rw.read().unwrap_or_else(|e| e.into_inner());
    let mut ev = crate::frame_ev::begin(crate::frame_ev::PSO_LIB_HIT);
    let hr = load(l.lib, w.as_ptr());
    if hr < 0 {
        if let Some(e) = ev.as_mut() {
            e.set_kind(crate::frame_ev::PSO_LIB_MISS);
        }
    }
    hr >= 0
}

unsafe fn store(l: &Lib, k: u128, pso: *mut c_void) {
    let w = wide(k);
    let hr = {
        let _w = l.rw.write().unwrap_or_else(|e| e.into_inner());
        let f: unsafe extern "system" fn(*mut c_void, *const u16, *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(l.lib as *mut c_void, LIB_STORE)) };
        unsafe { f(l.lib as *mut c_void, w.as_ptr(), pso) }
    };
    if hr >= 0 {
        let n = STORED.fetch_add(1, Relaxed) + 1;
        if n % SAVE_EVERY == 0 {
            save_async();
        }
    } else {
        l.rejected.lock().unwrap_or_else(|e| e.into_inner()).insert(k);
        let n = REJECTED.fetch_add(1, Relaxed);
        if n < 8 {
            record("d3d12_xs", "pso_lib", &format!("StorePipeline {k:032x} hr={hr:#010x}: the stored entry under this key does not match this PSO; it uses CachedPSO blobs from now on"));
        }
    }
}

unsafe fn cached_blob(pso: *mut c_void) -> Option<Vec<u8>> {
    let f: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(pso, PSO_CACHED_BLOB)) };
    let mut b: *mut c_void = core::ptr::null_mut();
    if unsafe { f(pso, &mut b) } < 0 || b.is_null() {
        return None;
    }
    let ptr: unsafe extern "system" fn(*mut c_void) -> *const u8 = unsafe { core::mem::transmute(vslot(b, 3)) };
    let len: unsafe extern "system" fn(*mut c_void) -> usize = unsafe { core::mem::transmute(vslot(b, 4)) };
    let (p, n) = unsafe { (ptr(b), len(b)) };
    let v = if p.is_null() || n == 0 { None } else { Some(unsafe { core::slice::from_raw_parts(p, n) }.to_vec()) };
    unsafe { release(b) };
    v
}

unsafe fn with_cached(k: u128, desc: &[u8], at: usize, ppv: *mut *mut c_void, create: &dyn Fn(*const u8) -> i32) -> i32 {
    let ck = k ^ CACHED_SALT;
    if let Some(b) = tombox_xshader::shader_cache::get(ck).filter(|b| !b.is_empty()) {
        let mut d = desc.to_vec();
        d[at..at + 8].copy_from_slice(&(b.as_ptr() as u64).to_le_bytes());
        d[at + 8..at + 16].copy_from_slice(&(b.len() as u64).to_le_bytes());
        let hr = create(d.as_ptr());
        if hr >= 0 {
            CACHED_HITS.fetch_add(1, Relaxed);
            return hr;
        }
    }
    CACHED_MISSES.fetch_add(1, Relaxed);
    let hr = create(desc.as_ptr());
    if hr >= 0 && !ppv.is_null() && !unsafe { *ppv }.is_null() && rd64(desc, at) == 0 {
        if let Some(b) = unsafe { cached_blob(*ppv) } {
            let _ = tombox_xshader::shader_cache::put(ck, &b);
        }
    }
    hr
}

unsafe fn run(key: Option<u128>, real: *mut c_void, desc: &[u8], cached_at: Option<usize>, ppv: *mut *mut c_void, load: &dyn Fn(usize, *const u16) -> i32, create: &dyn Fn(*const u8) -> i32) -> i32 {
    if !enabled() {
        return create(desc.as_ptr());
    }
    let Some(k) = key else {
        UNKEYED.fetch_add(1, Relaxed);
        return create(desc.as_ptr());
    };
    let l = lib(real);
    if let Some(l) = l {
        if unsafe { try_load(l, k, load) } {
            HITS.fetch_add(1, Relaxed);
            note_counts();
            return 0;
        }
    }
    MISSES.fetch_add(1, Relaxed);
    note_counts();
    let rejected = l.is_some_and(|l| l.rejected.lock().unwrap_or_else(|e| e.into_inner()).contains(&k));
    let hr = match cached_at {
        Some(at) if l.is_none() || rejected => unsafe { with_cached(k, desc, at, ppv, create) },
        _ => create(desc.as_ptr()),
    };
    if let (Some(l), false) = (l, rejected) {
        if hr >= 0 && !ppv.is_null() && !unsafe { *ppv }.is_null() {
            unsafe { store(l, k, *ppv) };
        }
    }
    hr
}

pub unsafe fn gfx(real: *mut c_void, desc: &[u8], riid: *const Guid, ppv: *mut *mut c_void, create: &dyn Fn(*const u8) -> i32) -> i32 {
    if !enabled() {
        return create(desc.as_ptr());
    }
    let key = root_hash(rd64(desc, 0)).and_then(|r| unsafe { gfx_key(desc, r, mode_tag()) });
    let load = |lib: usize, name: *const u16| -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, *const u16, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(lib as *mut c_void, LIB_LOAD_GFX)) };
        unsafe { f(lib as *mut c_void, name, desc.as_ptr(), riid, ppv) }
    };
    unsafe { run(key, real, desc, Some(0x278), ppv, &load, create) }
}

pub unsafe fn compute(real: *mut c_void, desc: &[u8], riid: *const Guid, ppv: *mut *mut c_void, create: &dyn Fn(*const u8) -> i32) -> i32 {
    if !enabled() {
        return create(desc.as_ptr());
    }
    let key = root_hash(rd64(desc, 0)).and_then(|r| unsafe { compute_key(desc, r, mode_tag()) });
    let load = |lib: usize, name: *const u16| -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, *const u16, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(lib as *mut c_void, LIB_LOAD_CS)) };
        unsafe { f(lib as *mut c_void, name, desc.as_ptr(), riid, ppv) }
    };
    unsafe { run(key, real, desc, Some(0x20), ppv, &load, create) }
}

pub unsafe fn stream(real: *mut c_void, desc: &[u8], extra: &str, sd: *const [usize; 2], riid: *const Guid, ppv: *mut *mut c_void, create: &dyn Fn() -> i32) -> i32 {
    if !enabled() {
        return create();
    }
    let key = root_hash(rd64(desc, 0)).and_then(|r| unsafe { gfx_key(desc, r, mode_tag()) }).map(|g| stream_key(g, extra));
    let load = |lib: usize, name: *const u16| -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, *const u16, *const [usize; 2], *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(lib as *mut c_void, LIB_LOAD_STREAM)) };
        unsafe { f(lib as *mut c_void, name, sd, riid, ppv) }
    };
    unsafe { run(key, real, desc, None, ppv, &load, &|_| create()) }
}

fn save() -> Result<String, String> {
    let Some(Some(l)) = LIB.get() else { return Err("no library".into()) };
    let blob = {
        let _w = l.rw.write().unwrap_or_else(|e| e.into_inner());
        let size: unsafe extern "system" fn(*mut c_void) -> usize = unsafe { core::mem::transmute(vslot(l.lib as *mut c_void, LIB_SIZE)) };
        let n = unsafe { size(l.lib as *mut c_void) };
        if n == 0 {
            return Err("empty library".into());
        }
        let mut v = vec![0u8; n];
        let ser: unsafe extern "system" fn(*mut c_void, *mut u8, usize) -> i32 = unsafe { core::mem::transmute(vslot(l.lib as *mut c_void, LIB_SERIALIZE)) };
        let hr = unsafe { ser(l.lib as *mut c_void, v.as_mut_ptr(), n) };
        if hr < 0 {
            return Err(format!("Serialize hr={hr:#010x}"));
        }
        v
    };
    let bytes = file_bytes(&l.ident, &blob);
    if let Some(d) = l.path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = l.path.with_extension(format!("bin.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &l.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename to {}: {e}", l.path.display()));
    }
    SAVED_AT.store(now_s(), Relaxed);
    Ok(format!("saved {} bytes to {}", bytes.len(), l.path.display()))
}

fn save_logged(why: &str) {
    if SAVING.swap(true, Relaxed) {
        return;
    }
    let stored = STORED.load(Relaxed);
    let r = save();
    SAVING.store(false, Relaxed);
    match r {
        Ok(m) => record("d3d12_xs", "pso_lib", &format!("{m} ({why}; {stored} PSOs stored this run); {}", summary())),
        Err(e) => record("d3d12_xs", "pso_lib", &format!("save ({why}) failed: {e}")),
    }
}

fn save_async() {
    if SAVING.load(Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new().name("tombox-pso-lib-save".into()).spawn(|| save_logged("periodic"));
}

static LAST_SAVED_STORED: AtomicU64 = AtomicU64::new(0);

pub fn on_present() {
    if !matches!(LIB.get(), Some(Some(_))) {
        return;
    }
    let s = STORED.load(Relaxed);
    if s != LAST_SAVED_STORED.load(Relaxed) && now_s().saturating_sub(SAVED_AT.load(Relaxed)) >= SAVE_SECS {
        LAST_SAVED_STORED.store(s, Relaxed);
        save_async();
    }
}

extern "C" fn exit_save() {
    if STORED.load(Relaxed) != 0 {
        save_logged("process exit");
    }
}

fn register_exit_save() {
    extern "C" {
        fn atexit(f: extern "C" fn()) -> i32;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        atexit(exit_save);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wr64(d: &mut [u8], o: usize, v: u64) {
        d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn key_follows_content_not_pointers() {
        let vs = vec![1u8, 2, 3, 4];
        let vs2 = vs.clone();
        let name = b"POSITION\0".to_vec();
        let name2 = name.clone();
        let mut el = vec![0u8; 0x20];
        wr64(&mut el, 0, name.as_ptr() as u64);
        el[0xc] = 6;
        let mut el2 = el.clone();
        wr64(&mut el2, 0, name2.as_ptr() as u64);
        let mk = |code: &[u8], il: &[u8]| {
            let mut d = vec![0u8; GFX_DESC];
            wr64(&mut d, 0x08, code.as_ptr() as u64);
            wr64(&mut d, 0x10, code.len() as u64);
            wr64(&mut d, 0x228, il.as_ptr() as u64);
            d[0x230] = 1;
            d[0x240] = 1;
            d
        };
        let (a, b) = (mk(&vs, &el), mk(&vs2, &el2));
        let ka = unsafe { gfx_key(&a, 7, "cbraw=0") }.unwrap();
        assert_eq!(Some(ka), unsafe { gfx_key(&b, 7, "cbraw=0") });
        assert_ne!(Some(ka), unsafe { gfx_key(&a, 8, "cbraw=0") });
        assert_ne!(Some(ka), unsafe { gfx_key(&a, 7, "cbraw=1") });
        let mut c = a.clone();
        c[0x244] = 28;
        assert_ne!(Some(ka), unsafe { gfx_key(&c, 7, "cbraw=0") });
        let mut cached = a.clone();
        wr64(&mut cached, 0x278, 0x1234);
        wr64(&mut cached, 0x280, 99);
        assert_eq!(Some(ka), unsafe { gfx_key(&cached, 7, "cbraw=0") });
        let vs3 = vec![1u8, 2, 3, 5];
        assert_ne!(Some(ka), unsafe { gfx_key(&mk(&vs3, &el), 7, "cbraw=0") });
        let mut bad = a.clone();
        bad[0x230] = 1;
        wr64(&mut bad, 0x228, 0);
        assert_eq!(unsafe { gfx_key(&bad, 7, "cbraw=0") }, None);
        assert_ne!(stream_key(ka, "x"), stream_key(ka, "y"));
        let mut cs = vec![0u8; CS_DESC];
        wr64(&mut cs, 0x08, vs.as_ptr() as u64);
        wr64(&mut cs, 0x10, 4);
        let k1 = unsafe { compute_key(&cs, 1, "cbraw=0") }.unwrap();
        wr64(&mut cs, 0x08, vs2.as_ptr() as u64);
        assert_eq!(Some(k1), unsafe { compute_key(&cs, 1, "cbraw=0") });
        assert_ne!(k1, ka);
    }

    #[test]
    fn stored_library_is_dropped_on_any_identity_or_version_change() {
        let ident = "NVIDIA X [10de:2d05] driver 32.0.16.1742 | cbraw=0";
        let f = file_bytes(ident, b"blob");
        assert_eq!(check_file(Some(&f), ident), FileCheck::Blob(b"blob"));
        assert_eq!(check_file(None, ident), FileCheck::Empty);
        assert!(matches!(check_file(Some(&f), "NVIDIA X [10de:2d05] driver 32.0.16.1800 | cbraw=0"), FileCheck::Rebuild(_)));
        assert!(matches!(check_file(Some(&f), "NVIDIA X [10de:2d05] driver 32.0.16.1742 | cbraw=1"), FileCheck::Rebuild(_)));
        let mut old = f.clone();
        old[4] = 9;
        assert!(matches!(check_file(Some(&old), ident), FileCheck::Rebuild(_)));
        assert!(matches!(check_file(Some(&f[..14]), ident), FileCheck::Rebuild(_)));
        assert!(matches!(check_file(Some(b"junkjunkjunkjunk"), ident), FileCheck::Rebuild(_)));
        assert_eq!(check_file(Some(&file_bytes(ident, b"")), ident), FileCheck::Empty);
    }

    #[test]
    fn driver_rejection_rebuilds_and_missing_support_disables() {
        assert_eq!(on_create_hr(0, true), OnCreate::Use);
        assert_eq!(on_create_hr(DRIVER_MISMATCH, true), OnCreate::RebuildEmpty);
        assert_eq!(on_create_hr(ADAPTER_NOT_FOUND, true), OnCreate::RebuildEmpty);
        assert_eq!(on_create_hr(E_INVALIDARG, true), OnCreate::RebuildEmpty);
        assert_eq!(on_create_hr(E_INVALIDARG, false), OnCreate::Unavailable);
    }

    #[test]
    fn names_are_32_hex_digits_and_nul_terminated() {
        let w = wide(0xabc);
        assert_eq!(w[32], 0);
        assert_eq!(String::from_utf16_lossy(&w[..32]), format!("{:032x}", 0xabcu128));
    }
}
