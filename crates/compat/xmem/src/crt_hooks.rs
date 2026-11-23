use core::sync::atomic::{AtomicUsize, Ordering};

use crate::heap_acct::{block_size, slot};
use crate::sys::GetProcessHeap;

pub const NAMES: [&[u8]; 9] = [b"malloc", b"calloc", b"realloc", b"free", b"_aligned_malloc", b"_aligned_realloc", b"_aligned_free", b"_strdup", b"_wcsdup"];

static ORIG: [AtomicUsize; 9] = [const { AtomicUsize::new(0) }; 9];

fn orig(i: usize) -> usize {
    ORIG[i].load(Ordering::Acquire)
}

fn heap() -> usize {
    unsafe { GetProcessHeap() as usize }
}

unsafe fn aligned_raw(p: usize) -> usize {
    core::ptr::read_unaligned(((p & !7) - 8) as *const usize)
}

unsafe fn plain_size(p: usize) -> Option<usize> {
    block_size(heap(), 0, p)
}

unsafe fn aligned_size(p: usize) -> Option<usize> {
    if p < 16 {
        return None;
    }
    block_size(heap(), 0, aligned_raw(p))
}

fn add(n: Option<usize>) {
    if let (Some(n), Some(a)) = (n, slot(heap())) {
        a.add(n);
    }
}

fn sub(n: Option<usize>) {
    if let (Some(n), Some(a)) = (n, slot(heap())) {
        a.sub(n);
    }
}

unsafe extern "C" fn malloc_w(n: usize) -> usize {
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(orig(0))(n);
    if r != 0 {
        add(plain_size(r));
    }
    r
}

unsafe extern "C" fn calloc_w(n: usize, s: usize) -> usize {
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize, usize) -> usize>(orig(1))(n, s);
    if r != 0 {
        add(plain_size(r));
    }
    r
}

unsafe extern "C" fn realloc_w(p: usize, n: usize) -> usize {
    let old = if p != 0 { plain_size(p) } else { None };
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize, usize) -> usize>(orig(2))(p, n);
    if r != 0 {
        sub(old);
        add(plain_size(r));
    } else if n == 0 {
        sub(old);
    }
    r
}

unsafe extern "C" fn free_w(p: usize) {
    let old = if p != 0 { plain_size(p) } else { None };
    core::mem::transmute::<usize, unsafe extern "C" fn(usize)>(orig(3))(p);
    sub(old);
}

unsafe extern "C" fn aligned_malloc_w(n: usize, align: usize) -> usize {
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize, usize) -> usize>(orig(4))(n, align);
    if r != 0 {
        add(aligned_size(r));
    }
    r
}

unsafe extern "C" fn aligned_realloc_w(p: usize, n: usize, align: usize) -> usize {
    let old = if p != 0 { aligned_size(p) } else { None };
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize, usize, usize) -> usize>(orig(5))(p, n, align);
    if r != 0 {
        sub(old);
        add(aligned_size(r));
    } else if n == 0 {
        sub(old);
    }
    r
}

unsafe extern "C" fn aligned_free_w(p: usize) {
    let old = if p != 0 { aligned_size(p) } else { None };
    core::mem::transmute::<usize, unsafe extern "C" fn(usize)>(orig(6))(p);
    sub(old);
}

unsafe extern "C" fn strdup_w(s: usize) -> usize {
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(orig(7))(s);
    if r != 0 {
        add(plain_size(r));
    }
    r
}

unsafe extern "C" fn wcsdup_w(s: usize) -> usize {
    let r = core::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(orig(8))(s);
    if r != 0 {
        add(plain_size(r));
    }
    r
}

pub fn wrappers() -> [usize; 9] {
    [
        malloc_w as *const () as usize,
        calloc_w as *const () as usize,
        realloc_w as *const () as usize,
        free_w as *const () as usize,
        aligned_malloc_w as *const () as usize,
        aligned_realloc_w as *const () as usize,
        aligned_free_w as *const () as usize,
        strdup_w as *const () as usize,
        wcsdup_w as *const () as usize,
    ]
}

fn crt_dll(dll: &[u8]) -> bool {
    let d = dll.to_ascii_lowercase();
    d.starts_with(b"api-ms-win-crt-") || d.starts_with(b"ucrtbase")
}

pub fn crt_rule(dll: &[u8], name: Option<&[u8]>, value: usize) -> Option<usize> {
    if !crt_dll(dll) || wrappers().contains(&value) {
        return None;
    }
    let i = NAMES.iter().position(|n| Some(*n) == name)?;
    match ORIG[i].compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(wrappers()[i]),
        Err(v) if v == value => Some(wrappers()[i]),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn malloc(n: usize) -> usize;
        fn calloc(n: usize, s: usize) -> usize;
        fn realloc(p: usize, n: usize) -> usize;
        fn free(p: usize);
        fn _aligned_malloc(n: usize, a: usize) -> usize;
        fn _aligned_realloc(p: usize, n: usize, a: usize) -> usize;
        fn _aligned_free(p: usize);
        fn _strdup(s: usize) -> usize;
    }

    fn w(i: usize, real: usize) -> usize {
        let dll: &[u8] = if i == 7 { b"api-ms-win-crt-string-l1-1-0.dll" } else { b"api-ms-win-crt-heap-l1-1-0.dll" };
        crt_rule(dll, Some(NAMES[i]), real).expect("crt slot claims")
    }

    #[test]
    fn ucrt_heap_is_the_process_heap() {
        let p = unsafe { malloc(100) };
        assert_eq!(unsafe { plain_size(p) }, Some(100));
        let q = unsafe { _aligned_malloc(100, 64) };
        assert_eq!(q % 64, 0);
        assert!(unsafe { aligned_size(q) }.unwrap() >= 100);
        unsafe {
            free(p);
            _aligned_free(q);
        }
    }

    #[test]
    fn title_crt_calls_move_the_title_heap_and_return_to_zero() {
        let h = heap();
        crate::heap_acct::register_title(h, 0, 0).unwrap();
        let base = crate::heap_acct::local_delta(h);
        let m: unsafe extern "C" fn(usize) -> usize = unsafe { core::mem::transmute(w(0, malloc as *const () as usize)) };
        let c: unsafe extern "C" fn(usize, usize) -> usize = unsafe { core::mem::transmute(w(1, calloc as *const () as usize)) };
        let r: unsafe extern "C" fn(usize, usize) -> usize = unsafe { core::mem::transmute(w(2, realloc as *const () as usize)) };
        let f: unsafe extern "C" fn(usize) = unsafe { core::mem::transmute(w(3, free as *const () as usize)) };
        let am: unsafe extern "C" fn(usize, usize) -> usize = unsafe { core::mem::transmute(w(4, _aligned_malloc as *const () as usize)) };
        let ar: unsafe extern "C" fn(usize, usize, usize) -> usize = unsafe { core::mem::transmute(w(5, _aligned_realloc as *const () as usize)) };
        let af: unsafe extern "C" fn(usize) = unsafe { core::mem::transmute(w(6, _aligned_free as *const () as usize)) };
        let sd: unsafe extern "C" fn(usize) -> usize = unsafe { core::mem::transmute(w(7, _strdup as *const () as usize)) };
        let d = || crate::heap_acct::local_delta(h) - base;
        unsafe {
            let a = m(0x1000);
            assert_eq!(d(), 0x1000);
            let b = c(4, 0x100);
            assert_eq!(d(), 0x1400);
            let a2 = r(a, 0x3000);
            assert_eq!(d(), 0x3400);
            let x = r(0, 0x20);
            assert_eq!(d(), 0x3420);
            assert_eq!(r(x, 0), 0);
            assert_eq!(d(), 0x3400);
            f(a2);
            f(b);
            f(0);
            assert_eq!(d(), 0);
            let al = am(0x500, 256);
            assert_eq!(al % 256, 0);
            let raw = d();
            assert!(raw >= 0x500 && raw < 0x500 + 256 + 16, "{raw:#x}");
            let al2 = ar(al, 0x2000, 256);
            assert_eq!(al2 % 256, 0);
            assert!(d() >= 0x2000);
            af(al2);
            assert_eq!(d(), 0);
            let al3 = am(0x40, 64);
            assert_eq!(ar(al3, 0, 64), 0);
            assert_eq!(d(), 0);
            let s = sd(b"title\0".as_ptr() as usize);
            assert_eq!(d(), 6);
            f(s);
            assert_eq!(d(), 0);
        }
        assert!(crt_rule(b"KERNEL32.dll", Some(b"malloc"), 0x1234).is_none());
        assert!(crt_rule(b"api-ms-win-crt-heap-l1-1-0.dll", Some(b"malloc"), 0x1234).is_none());
        assert!(crt_rule(b"api-ms-win-crt-heap-l1-1-0.dll", Some(b"malloc"), wrappers()[0]).is_none());
    }
}
