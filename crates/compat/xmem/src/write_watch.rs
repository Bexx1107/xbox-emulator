use core::ffi::c_void;
use std::sync::Mutex;

use crate::sys::*;

pub const OWNER_NONE: u32 = 0;
pub const OWNER_EMULATOR: u32 = 1;
pub const OWNER_TITLE: u32 = 2;

static REGIONS: Mutex<Vec<(usize, usize, u32)>> = Mutex::new(Vec::new());

pub fn wanted() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("TOMBOX_WRITE_WATCH").map(|v| v.trim() != "0").unwrap_or(true))
}

fn note(base: usize, len: usize, owner: u32) {
    let mut r = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|&(b, n, _)| b + n <= base || base + len <= b);
    let at = r.partition_point(|&(b, _, _)| b < base);
    r.insert(at, (base, len, owner));
}

pub fn reserve_at(base: usize, len: usize, align: usize, alloc_type: u32, protect: u32) -> *mut c_void {
    let title = alloc_type & MEM_WRITE_WATCH != 0;
    let add = !title && wanted() && alloc_type & MEM_RESERVE != 0;
    let go = |t: u32| -> *mut c_void {
        if base == 0 && align > 0x1_0000 {
            reserve_aligned(len, align, t, protect)
        } else {
            unsafe { VirtualAlloc(base as *mut c_void, len, t, protect) }
        }
    };
    let mut p = if add { go(alloc_type | MEM_WRITE_WATCH) } else { core::ptr::null_mut() };
    let watched = !p.is_null();
    if p.is_null() {
        p = go(alloc_type);
    }
    if !p.is_null() && alloc_type & MEM_RESERVE != 0 {
        if title {
            note(p as usize, len, OWNER_TITLE);
        } else if watched {
            note(p as usize, len, OWNER_EMULATOR);
        }
    }
    p
}

pub fn forget(base: usize) {
    let mut r = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|&(b, _, _)| b != base);
}

pub fn reserve(len: usize, align: usize, alloc_type: u32, protect: u32) -> *mut c_void {
    reserve_at(0, len, align, alloc_type, protect)
}

fn alloc_base(addr: usize) -> Option<usize> {
    let mut m = MemoryBasicInformation::default();
    let n = unsafe { VirtualQueryEx(GetCurrentProcess(), addr as *const c_void, &mut m, core::mem::size_of::<MemoryBasicInformation>()) };
    if n == 0 || m.state == 0x1_0000 {
        None
    } else {
        Some(m.alloc_base)
    }
}

pub fn owner(addr: usize) -> (u32, usize, usize) {
    let mut r = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    let i = r.partition_point(|&(b, _, _)| b <= addr);
    if i == 0 {
        return (OWNER_NONE, 0, 0);
    }
    let (b, n, o) = r[i - 1];
    if addr >= b + n {
        return (OWNER_NONE, 0, 0);
    }
    if alloc_base(addr) != Some(b) {
        r.remove(i - 1);
        return (OWNER_NONE, 0, 0);
    }
    (o, b, n)
}

#[no_mangle]
pub unsafe extern "system" fn TomboxWriteWatchOwner(addr: usize, base: *mut usize, len: *mut usize) -> u32 {
    let (o, b, n) = owner(addr);
    if !base.is_null() {
        *base = b;
    }
    if !len.is_null() {
        *len = n;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "system" {
        fn GetWriteWatch(flags: u32, base: *mut c_void, size: usize, addresses: *mut *mut c_void, count: *mut usize, granularity: *mut u32) -> u32;
    }

    fn dirty(p: usize, n: usize) -> Option<Vec<usize>> {
        let mut a = vec![core::ptr::null_mut::<c_void>(); n / 0x1000];
        let mut c = a.len();
        let mut g = 0u32;
        let r = unsafe { GetWriteWatch(1, p as *mut c_void, n, a.as_mut_ptr(), &mut c, &mut g) };
        (r == 0).then(|| a[..c].iter().map(|&x| x as usize - p).collect())
    }

    #[test]
    fn emulator_reservations_are_watched_and_owned() {
        let n = 0x2_0000;
        let p = reserve(n, 0x1_0000, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) as usize;
        assert_ne!(p, 0);
        let want = if wanted() { OWNER_EMULATOR } else { OWNER_NONE };
        assert_eq!(owner(p + 0x1234), if wanted() { (want, p, n) } else { (OWNER_NONE, 0, 0) });
        if wanted() {
            assert_eq!(dirty(p, n), Some(vec![]));
            unsafe { *((p + 0x5010) as *mut u8) = 1 };
            assert_eq!(dirty(p, n), Some(vec![0x5000]));
        }
        unsafe { VirtualFree(p as *mut c_void, 0, MEM_RELEASE) };
        assert_eq!(owner(p + 0x1234).0, OWNER_NONE);
    }

    #[test]
    fn title_write_watch_reservations_are_title_owned() {
        let n = 0x1_0000;
        let p = reserve(n, 0, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) as usize;
        assert_ne!(p, 0);
        assert_eq!(owner(p).0, OWNER_TITLE);
        unsafe { VirtualFree(p as *mut c_void, 0, MEM_RELEASE) };
    }
}
