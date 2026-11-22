use std::collections::HashSet;
use std::sync::Mutex;

pub const MSAA_ALIGN: u64 = 0x40_0000;

static CAPABLE: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

pub fn heap_layout(size: u64, msaa: bool) -> (u64, u64) {
    if msaa || size >= MSAA_ALIGN {
        (size.div_ceil(MSAA_ALIGN) * MSAA_ALIGN, MSAA_ALIGN)
    } else {
        (size, 0)
    }
}

pub fn note_heap(heap: usize, align: u64) {
    if align >= MSAA_ALIGN {
        CAPABLE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(heap);
    }
}

pub fn forget_heap(heap: usize) {
    if let Some(s) = CAPABLE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        s.remove(&heap);
    }
}

pub fn capable(heap: usize) -> bool {
    CAPABLE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(&heap))
}

pub fn max_align(samples: u32) -> u64 {
    if samples > 1 { MSAA_ALIGN } else { 0x1_0000 }
}

pub fn span_fits(samples: u32, va: u64, lo: u64, align: u64) -> bool {
    samples <= 1 || (align != 0 && (va - lo) % align == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msaa_heap_layout_and_span_rules() {
        assert_eq!(heap_layout(0x1_0000, false), (0x1_0000, 0));
        assert_eq!(heap_layout(0x1_0000, true), (MSAA_ALIGN, MSAA_ALIGN));
        assert_eq!(heap_layout(0x41_0000, false), (0x80_0000, MSAA_ALIGN));
        assert_eq!(max_align(1), 0x1_0000);
        assert_eq!(max_align(4), MSAA_ALIGN);
        assert!(span_fits(1, 0x1_2345_0000, 0x1_0000_0000, 0x1_0000));
        assert!(span_fits(4, 0x1_0080_0000, 0x1_0000_0000, MSAA_ALIGN));
        assert!(!span_fits(4, 0x1_0081_0000, 0x1_0000_0000, MSAA_ALIGN));
        note_heap(0x1234, MSAA_ALIGN);
        note_heap(0x5678, 0);
        assert!(capable(0x1234) && !capable(0x5678));
        forget_heap(0x1234);
        assert!(!capable(0x1234));
    }
}

#[cfg(test)]
mod probe {
    use core::ffi::c_void;
    #[repr(C)]
    struct G(u32, u16, u16, [u8; 8]);
    const IID_DEVICE: G = G(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
    const IID_RESOURCE: G = G(0x696442be, 0xa72e, 0x4059, [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad]);
    unsafe fn vt(o: *mut c_void) -> *const usize { unsafe { *(o as *const *const usize) } }
    #[test]
    #[ignore]
    fn probe_reserved_msaa() {
        unsafe {
            let p = crate::host_proc(b"D3D12CreateDevice\0");
            let mk: extern "system" fn(*mut c_void, u32, *const G, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(mk(core::ptr::null_mut(), 0xc000, &IID_DEVICE, &mut dev) >= 0);
            let mut o = [0u32; 10];
            let cf: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 = core::mem::transmute(*vt(dev).add(13));
            cf(dev, 0, o.as_mut_ptr() as *mut c_void, 60);
            println!("options: tiled tier={} heap tier={}", o[5], o[7]);
            for layout in [2u32, 0] {
                let mut d = [0u8; 0x38];
                d[0..4].copy_from_slice(&3u32.to_le_bytes());
                d[0x10..0x18].copy_from_slice(&1024u64.to_le_bytes());
                d[0x18..0x1c].copy_from_slice(&1024u32.to_le_bytes());
                d[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
                d[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
                d[0x20..0x24].copy_from_slice(&28u32.to_le_bytes());
                d[0x24..0x28].copy_from_slice(&4u32.to_le_bytes());
                d[0x2c..0x30].copy_from_slice(&layout.to_le_bytes());
                d[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
                let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, *const c_void, *const G, *mut *mut c_void) -> i32 = core::mem::transmute(*vt(dev).add(30));
                let mut r: *mut c_void = core::ptr::null_mut();
                let hr = f(dev, d.as_ptr(), 4, core::ptr::null(), &IID_RESOURCE, &mut r);
                println!("reserved msaa layout={layout} hr={hr:#010x}");
            }
        }
    }
}
