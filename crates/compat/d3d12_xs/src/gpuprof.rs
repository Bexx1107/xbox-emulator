use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use compat_common::record;

use crate::device_wrap::Guid;

const IID_DEVICE: Guid =
    Guid { d1: 0x1898_19F1, d2: 0x1DB6, d3: 0x4B57, d4: [0xBE, 0x54, 0x18, 0x21, 0x33, 0x9B, 0x85, 0xF7] };
const IID_QUERY_HEAP: Guid =
    Guid { d1: 0x0d96_58ae, d2: 0xed45, d3: 0x469e, d4: [0xa6, 0x1d, 0x97, 0x0e, 0xc5, 0x83, 0xca, 0xb4] };
const IID_RESOURCE: Guid =
    Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };
const SLOTS: u32 = 1 << 17;
const FRAMES: u64 = 16;
const SETTLE: u64 = 8;
const PERIOD_SECS: u64 = 20;
const QUERY_TIMESTAMP: u32 = 2;

#[derive(Clone, Copy)]
struct Label {
    a: u64,
    b: u64,
    compute: bool,
}

#[derive(Clone, Copy)]
struct Ev {
    slot: u32,
    label: Label,
    kind: u8,
    count: bool,
    frame: u32,
}

#[derive(Default)]
struct ListRec {
    label: Option<Label>,
    evs: Vec<Ev>,
    last_kind: u8,
}

pub const TAG_BLENDCLAMP: u8 = 1;
pub const TAG_CBV_COPY: u8 = 2;
pub const TAG_RESOLVE: u8 = 3;
pub const TAG_UPLOAD: u8 = 4;
pub const TAG_PRESENT: u8 = 5;
const KIND_PASS: u8 = 16;
const KIND_CLOSE: u8 = 255;

fn tag_name(tag: u64) -> &'static str {
    match tag as u8 {
        TAG_BLENDCLAMP => "blendclamp",
        TAG_CBV_COPY => "cbv_copy",
        TAG_RESOLVE => "resolve",
        TAG_UPLOAD => "upload_pump",
        TAG_PRESENT => "present_copy",
        _ => "emu?",
    }
}

fn stampable(ty: u32) -> bool {
    ty == 0 || ty == 2
}

struct State {
    heap: usize,
    rb: usize,
    lists: HashMap<usize, ListRec>,
    labels: HashMap<usize, Label>,
    done: Vec<Vec<Ev>>,
    presents: u64,
    window: Option<u64>,
    finalize_at: Option<u64>,
    last: Option<Instant>,
    reports: u32,
}

static NEXT: [AtomicUsize; 128] = [const { AtomicUsize::new(0) }; 128];
static ARMED: AtomicBool = AtomicBool::new(false);
static SLOT: AtomicU32 = AtomicU32::new(0);
static FRAME_IX: AtomicU32 = AtomicU32::new(0);

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn frames() -> u64 {
    static F: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *F.get_or_init(|| env_u64("TOMBOX_GPUPROF_FRAMES").unwrap_or(FRAMES).clamp(1, 240))
}

fn period_ms() -> u64 {
    static P: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *P.get_or_init(|| env_u64("TOMBOX_GPUPROF_PERIOD_MS").unwrap_or(PERIOD_SECS * 1000).max(250))
}

fn worst_frame(per_frame: &HashMap<u32, u64>) -> Option<(u32, u64, u64)> {
    if per_frame.is_empty() {
        return None;
    }
    let mut v: Vec<u64> = per_frame.values().copied().collect();
    v.sort_unstable();
    let median = v[v.len() / 2];
    let (&f, &w) = per_frame.iter().max_by_key(|(f, t)| (**t, core::cmp::Reverse(**f)))?;
    Some((f, w, median))
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

static LIVE: crate::live_sw::Armed = crate::live_sw::Armed::new("TOMBOX_GPUPROF", "gpuprof", false);

fn cfg_default() -> bool {
    compat_common::cfg("gpuprof").as_deref() == Some("1")
}

pub fn enabled() -> bool {
    LIVE.armed_or(cfg_default)
}

fn windows_on() -> bool {
    LIVE.on_or(cfg_default)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(|| State {
        heap: 0,
        rb: 0,
        lists: HashMap::new(),
        labels: HashMap::new(),
        done: Vec::new(),
        presents: 0,
        window: None,
        finalize_at: None,
        last: None,
        reports: 0,
    });
    f(s)
}

fn next(slot: usize) -> usize {
    NEXT[slot].load(Ordering::Relaxed)
}

unsafe fn real(slot: usize) -> usize {
    unsafe { crate::cmdlist_wrap::real_method(slot) }
}

pub unsafe fn install() {
    if !enabled() {
        return;
    }
    let hooks: [(usize, usize); 7] = [
        (9, h_close as usize), (10, h_reset as usize), (12, h_draw as usize), (13, h_draw_indexed as usize),
        (14, h_dispatch as usize), (25, h_set_pso as usize), (59, h_indirect as usize),
    ];
    for (slot, _) in hooks {
        let prev = crate::cmdlist_wrap::vt_get(slot);
        if prev == 0 {
            record("d3d12_xs", "gpuprof", &format!("command-list slot {slot} empty - profiler not installed"));
            return;
        }
        NEXT[slot].store(prev, Ordering::Relaxed);
    }
    for (slot, f) in hooks {
        crate::cmdlist_wrap::vt_set(slot, f);
    }
    record("d3d12_xs", "gpuprof", &format!("installed: {}-frame GPU timestamp windows every {} ms (TOMBOX_GPUPROF_FRAMES / TOMBOX_GPUPROF_PERIOD_MS); each window also reports its slowest frame against the median", frames(), period_ms()));
}

fn label_of(pso: usize) -> Label {
    let codes = crate::device_x::pso_stage_codes(pso);
    let h = |name: &str| -> u64 {
        codes.iter().find(|(n, _)| *n == name)
            .and_then(|(_, c)| crate::shader_xlat::origin_hash(*c))
            .unwrap_or(0)
    };
    if codes.iter().any(|(n, _)| *n == "cs") {
        Label { a: h("cs"), b: 0, compute: true }
    } else {
        Label { a: h("vs"), b: h("ps"), compute: false }
    }
}

fn on_set_pso(list: usize, pso: usize) {
    with(|s| {
        let l = match s.labels.get(&pso) {
            Some(l) => *l,
            None => {
                let l = label_of(pso);
                s.labels.insert(pso, l);
                l
            }
        };
        s.lists.entry(list).or_default().label = Some(l);
    });
}

unsafe fn ensure_resources(s: &mut State, list: *mut c_void) -> bool {
    if s.heap != 0 && s.rb != 0 {
        return true;
    }
    if s.heap == usize::MAX {
        return false;
    }
    let get: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(real(7)) };
    let mut dev: *mut c_void = core::ptr::null_mut();
    if unsafe { get(list, &IID_DEVICE, &mut dev) } < 0 || dev.is_null() {
        return false;
    }
    let vt = unsafe { *(dev as *const *const usize) };
    let qdesc: [u32; 3] = [1, SLOTS, 0];
    let create_qh: unsafe extern "system" fn(*mut c_void, *const u32, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt.add(39)) };
    let mut heap: *mut c_void = core::ptr::null_mut();
    let hr_h = unsafe { create_qh(dev, qdesc.as_ptr(), &IID_QUERY_HEAP, &mut heap) };
    let props: [u32; 5] = [3, 0, 0, 1, 1];
    let mut bd = [0u8; 0x38];
    bd[0..4].copy_from_slice(&1u32.to_le_bytes());
    bd[0x10..0x18].copy_from_slice(&(SLOTS as u64 * 8).to_le_bytes());
    bd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    bd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    bd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    bd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    bd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    let create_cr: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt.add(27)) };
    let mut rb: *mut c_void = core::ptr::null_mut();
    let hr_r = unsafe { create_cr(dev, props.as_ptr(), 0, bd.as_ptr(), 0x400, core::ptr::null(), &IID_RESOURCE, &mut rb) };
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*vt.add(2)) };
    unsafe { release(dev) };
    if hr_h < 0 || hr_r < 0 || heap.is_null() || rb.is_null() {
        record("d3d12_xs", "gpuprof", &format!("resource creation FAILED heap hr={hr_h:#010x} readback hr={hr_r:#010x}; profiler disabled"));
        s.heap = usize::MAX;
        return false;
    }
    s.heap = heap as usize;
    s.rb = rb as usize;
    true
}

unsafe fn stamp(list: *mut c_void, kind: u8) {
    unsafe { stamp_ev(list, None, kind, true) };
}

unsafe fn stamp_ev(list: *mut c_void, tag: Option<u8>, kind: u8, count: bool) {
    if !ARMED.load(Ordering::Relaxed) || !stampable(unsafe { crate::fill::list_type(list) }) {
        return;
    }
    with(|s| {
        if !unsafe { ensure_resources(s, list) } {
            return;
        }
        let slot = SLOT.fetch_add(1, Ordering::Relaxed);
        if slot >= SLOTS {
            return;
        }
        let end: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32) = unsafe { core::mem::transmute(real(53)) };
        unsafe { end(list, s.heap as *mut c_void, QUERY_TIMESTAMP, slot) };
        let r = s.lists.entry(list as usize).or_default();
        let (label, kind) = match tag {
            Some(t) => (Label { a: t as u64, b: 0, compute: false }, KIND_PASS),
            None if count => {
                r.last_kind = kind;
                (r.label.unwrap_or(Label { a: 0, b: 0, compute: kind == 2 }), kind)
            }
            None => (r.label.unwrap_or(Label { a: 0, b: 0, compute: r.last_kind == 2 }), r.last_kind),
        };
        r.evs.push(Ev { slot, label, kind, count, frame: FRAME_IX.load(Ordering::Relaxed) });
    });
}

pub unsafe fn pass_begin(list: *mut c_void, tag: u8) {
    unsafe { stamp_ev(list, Some(tag), KIND_PASS, true) };
}

pub unsafe fn pass_end(list: *mut c_void) {
    unsafe { stamp_ev(list, None, 0, false) };
}

pub unsafe fn own_begin(list: *mut c_void, tag: u8) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    with(|s| {
        let r = s.lists.entry(list as usize).or_default();
        r.evs.clear();
        r.label = None;
        r.last_kind = 4;
    });
    unsafe { pass_begin(list, tag) };
}

pub unsafe fn own_close(list: *mut c_void) {
    unsafe { close_list(list) };
}

unsafe fn close_list(list: *mut c_void) {
    with(|s| {
        let Some(r) = s.lists.get_mut(&(list as usize)) else { return };
        if r.evs.is_empty() || s.heap == 0 || s.heap == usize::MAX {
            r.evs.clear();
            return;
        }
        let mut evs = core::mem::take(&mut r.evs);
        let slot = SLOT.fetch_add(1, Ordering::Relaxed);
        if slot >= SLOTS {
            return;
        }
        let end: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32) = unsafe { core::mem::transmute(real(53)) };
        let resolve: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, *mut c_void, u64) =
            unsafe { core::mem::transmute(real(54)) };
        unsafe { end(list, s.heap as *mut c_void, QUERY_TIMESTAMP, slot) };
        evs.push(Ev { slot, label: Label { a: 0, b: 0, compute: false }, kind: KIND_CLOSE, count: false, frame: FRAME_IX.load(Ordering::Relaxed) });
        let mut slots: Vec<u32> = evs.iter().map(|e| e.slot).collect();
        slots.sort_unstable();
        let mut i = 0;
        while i < slots.len() {
            let mut j = i + 1;
            while j < slots.len() && slots[j] == slots[j - 1] + 1 {
                j += 1;
            }
            unsafe { resolve(list, s.heap as *mut c_void, QUERY_TIMESTAMP, slots[i], (j - i) as u32, s.rb as *mut c_void, slots[i] as u64 * 8) };
            i = j;
        }
        s.done.push(evs);
    });
}

fn name(l: &Label) -> String {
    if l.compute {
        format!("cs={:016x}", l.a)
    } else {
        format!("vs={:016x} ps={}", l.a, if l.b == 0 { "-".to_string() } else { format!("{:016x}", l.b) })
    }
}

unsafe fn report(s: &mut State) {
    let map: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut u8) -> i32 =
        unsafe { core::mem::transmute(*(*(s.rb as *const *const usize)).add(8)) };
    let unmap: unsafe extern "system" fn(*mut c_void, u32, *const c_void) =
        unsafe { core::mem::transmute(*(*(s.rb as *const *const usize)).add(9)) };
    let mut p: *mut u8 = core::ptr::null_mut();
    if unsafe { map(s.rb as *mut c_void, 0, core::ptr::null(), &mut p) } < 0 || p.is_null() {
        record("d3d12_xs", "gpuprof", "readback Map FAILED");
        return;
    }
    let ts = |slot: u32| unsafe { core::ptr::read_unaligned((p as *const u64).add(slot as usize)) };
    let freq = crate::timestamp::freq();
    let tick_ms = if freq != 0 { 1000.0 / freq as f64 } else { 0.0 };
    let max_ticks = if freq != 0 { freq / 10 } else { u64::MAX };
    let mut agg: HashMap<(u64, u64, bool, u8), (u64, u64)> = HashMap::new();
    let mut per_frame: HashMap<u32, u64> = HashMap::new();
    let mut by_frame: HashMap<(u32, u64, u64, bool, u8), u64> = HashMap::new();
    let (mut lo, mut hi, mut bad, mut n) = (u64::MAX, 0u64, 0u64, 0u64);
    for seq in &s.done {
        for w in seq.windows(2) {
            let (t0, t1) = (ts(w[0].slot), ts(w[1].slot));
            if t0 == 0 || t1 < t0 || t1 - t0 > max_ticks {
                bad += 1;
                continue;
            }
            lo = lo.min(t0);
            hi = hi.max(t1);
            n += 1;
            let e = agg.entry((w[0].label.a, w[0].label.b, w[0].label.compute, w[0].kind)).or_default();
            e.0 += t1 - t0;
            e.1 += w[0].count as u64;
            *per_frame.entry(w[0].frame).or_default() += t1 - t0;
            *by_frame.entry((w[0].frame, w[0].label.a, w[0].label.b, w[0].label.compute, w[0].kind)).or_default() += t1 - t0;
        }
    }
    unsafe { unmap(s.rb as *mut c_void, 0, core::ptr::null()) };
    let mut rows: Vec<_> = agg.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let total: u64 = rows.iter().map(|r| r.1 .0).sum();
    let nf = frames();
    let per = |t: u64| t as f64 * tick_ms / nf as f64;
    let span = if hi > lo { per(hi - lo) } else { 0.0 };
    let mut out = format!(
        "[gpuprof] window {}: {nf} frames, {n} timed events ({bad} rejected), freq={freq} Hz | summed GPU {:.3} ms/frame, wall span {:.3} ms/frame\n",
        s.reports, per(total), span);
    out.push_str(&format!("[gpuprof]   passes: {}\n", pass_summary(&rows, &per, nf)));
    if let Some((wf, wt, med)) = worst_frame(&per_frame) {
        let ms = |t: u64| t as f64 * tick_ms;
        let mut d: Vec<(f64, String)> = rows.iter().map(|((a, b, compute, kind), (t, _))| {
            let in_worst = by_frame.get(&(wf, *a, *b, *compute, *kind)).copied().unwrap_or(0);
            let who = if *kind == KIND_PASS { tag_name(*a).to_string() } else { name(&Label { a: *a, b: *b, compute: *compute }) };
            (ms(in_worst) - per(*t), who)
        }).collect();
        d.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(core::cmp::Ordering::Equal));
        let top: Vec<String> = d.iter().take(8).filter(|x| x.0 > 0.05).map(|(x, w)| format!("+{x:.2} {w}")).collect();
        out.push_str(&format!("[gpuprof]   slowest frame {wf}: {:.3} ms vs median {:.3} ms over {} frames | over its per-frame average: {}\n",
            ms(wt), ms(med), per_frame.len(), if top.is_empty() { "-".to_string() } else { top.join(", ") }));
    }
    for ((a, b, compute, kind), (t, c)) in rows.iter().take(40) {
        let k = match *kind { 0 => "draw", 1 => "drawidx", 2 => "dispatch", 3 => "indirect", 4 => "listhead", KIND_PASS => "emupass", _ => "?" };
        let who = if *kind == KIND_PASS { tag_name(*a).to_string() } else { name(&Label { a: *a, b: *b, compute: *compute }) };
        out.push_str(&format!("[gpuprof]   {:7.3} ms/f {:5.1}% {:6.1} calls/f {k:8} {who}\n",
            per(*t), *t as f64 * 100.0 / total.max(1) as f64, *c as f64 / nf as f64));
    }
    record("d3d12_xs", "gpuprof", &out);
}

fn pass_summary(rows: &[((u64, u64, bool, u8), (u64, u64))], per: &dyn Fn(u64) -> f64, nf: u64) -> String {
    let mut title = (0u64, 0u64);
    let mut tags: Vec<(u64, u64, u64)> = Vec::new();
    for ((a, _, _, kind), (t, c)) in rows {
        if *kind == KIND_PASS {
            match tags.iter_mut().find(|e| e.0 == *a) {
                Some(e) => {
                    e.1 += t;
                    e.2 += c;
                }
                None => tags.push((*a, *t, *c)),
            }
        } else {
            title.0 += t;
            title.1 += c;
        }
    }
    tags.sort_by_key(|e| e.0);
    let mut out = format!("title {:.3} ms/f {:.1} calls/f", per(title.0), title.1 as f64 / nf as f64);
    for (a, t, c) in tags {
        out.push_str(&format!(" | {} {:.3} ms/f {:.1} calls/f", tag_name(a), per(t), c as f64 / nf as f64));
    }
    out
}

pub fn on_present() {
    if !enabled() {
        return;
    }
    with(|s| {
        s.presents += 1;
        if let Some(at) = s.finalize_at {
            if s.presents >= at {
                if s.heap != 0 && s.heap != usize::MAX {
                    unsafe { report(s) };
                }
                s.reports += 1;
                s.finalize_at = None;
                s.done.clear();
                s.last = Some(Instant::now());
            }
            return;
        }
        if let Some(start) = s.window {
            FRAME_IX.store((s.presents - start) as u32, Ordering::Relaxed);
            if s.presents >= start + frames() {
                ARMED.store(false, Ordering::Relaxed);
                s.window = None;
                s.finalize_at = Some(s.presents + SETTLE);
            }
            return;
        }
        let due = match s.last {
            None => {
                s.last = Some(Instant::now());
                false
            }
            Some(t) => t.elapsed().as_millis() as u64 >= period_ms(),
        };
        if due && windows_on() {
            for r in s.lists.values_mut() {
                r.evs.clear();
            }
            s.done.clear();
            SLOT.store(0, Ordering::Relaxed);
            FRAME_IX.store(0, Ordering::Relaxed);
            s.window = Some(s.presents);
            ARMED.store(true, Ordering::Relaxed);
        }
    });
}

unsafe extern "system" fn h_close(this: *mut c_void) -> i32 {
    unsafe { crate::blendclamp::flush(this) };
    unsafe { close_list(this) };
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(next(9)) };
    unsafe { f(this) }
}

unsafe extern "system" fn h_reset(this: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    with(|s| {
        let r = s.lists.entry(this as usize).or_default();
        r.evs.clear();
        r.label = None;
    });
    if !pso.is_null() {
        on_set_pso(this as usize, pso as usize);
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(next(10)) };
    let hr = unsafe { f(this, alloc, pso) };
    if hr >= 0 && ARMED.load(Ordering::Relaxed) && unsafe { crate::fill::list_type(this) } != 1 {
        unsafe { stamp(this, 4) };
    }
    hr
}

unsafe extern "system" fn h_set_pso(this: *mut c_void, pso: *mut c_void) {
    on_set_pso(this as usize, pso as usize);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(next(25)) };
    unsafe { f(this, pso) }
}

unsafe extern "system" fn h_draw(this: *mut c_void, a: u32, b: u32, c: u32, d: u32) {
    unsafe { stamp(this, 0) };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = unsafe { core::mem::transmute(next(12)) };
    unsafe { f(this, a, b, c, d) }
}

unsafe extern "system" fn h_draw_indexed(this: *mut c_void, a: u32, b: u32, c: u32, d: i32, e: u32) {
    unsafe { stamp(this, 1) };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32) = unsafe { core::mem::transmute(next(13)) };
    unsafe { f(this, a, b, c, d, e) }
}

unsafe extern "system" fn h_dispatch(this: *mut c_void, a: u32, b: u32, c: u32) {
    unsafe { stamp(this, 2) };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = unsafe { core::mem::transmute(next(14)) };
    unsafe { f(this, a, b, c) }
}

unsafe extern "system" fn h_indirect(this: *mut c_void, sig: *mut c_void, n: u32, buf: *mut c_void, off: u64, cbuf: *mut c_void, coff: u64) {
    unsafe { stamp(this, 3) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u64, *mut c_void, u64) = unsafe { core::mem::transmute(next(59)) };
    unsafe { f(this, sig, n, buf, off, cbuf, coff) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_print_console_hashes() {
        assert_eq!(name(&Label { a: 0x1e8bbd9bbe7ab9b7, b: 0, compute: true }), "cs=1e8bbd9bbe7ab9b7");
        assert_eq!(name(&Label { a: 1, b: 0, compute: false }), "vs=0000000000000001 ps=-");
    }

    #[test]
    fn pass_summary_splits_title_time_from_each_emulator_pass() {
        let rows = vec![
            ((7, 8, false, 0), (1600, 32)),
            ((TAG_BLENDCLAMP as u64, 0, false, KIND_PASS), (320, 48)),
            ((9, 0, true, 2), (160, 16)),
            ((TAG_CBV_COPY as u64, 0, false, KIND_PASS), (16, 160)),
        ];
        let per = |t: u64| t as f64 / FRAMES as f64;
        assert_eq!(pass_summary(&rows, &per, FRAMES), "title 110.000 ms/f 3.0 calls/f | blendclamp 20.000 ms/f 3.0 calls/f | cbv_copy 1.000 ms/f 10.0 calls/f");
    }

    #[test]
    fn worst_frame_picks_the_slowest_and_reports_the_median() {
        let m: HashMap<u32, u64> = [(0, 10), (1, 12), (2, 40), (3, 11), (4, 13)].into_iter().collect();
        assert_eq!(worst_frame(&m), Some((2, 40, 12)));
        let tie: HashMap<u32, u64> = [(5, 9), (3, 9)].into_iter().collect();
        assert_eq!(worst_frame(&tie), Some((3, 9, 9)));
        assert_eq!(worst_frame(&HashMap::new()), None);
    }

    #[test]
    fn only_direct_and_compute_lists_take_timestamps() {
        assert!(stampable(0));
        assert!(stampable(2));
        assert!(!stampable(1));
        assert!(!stampable(3));
    }

    #[test]
    fn every_tag_has_a_name() {
        for t in [TAG_BLENDCLAMP, TAG_CBV_COPY, TAG_RESOLVE, TAG_UPLOAD, TAG_PRESENT] {
            assert_ne!(tag_name(t as u64), "emu?");
        }
    }
}
