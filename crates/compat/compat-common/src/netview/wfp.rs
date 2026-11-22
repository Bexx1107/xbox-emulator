use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

extern "system" {
    fn CreateFileMappingW(file: *mut c_void, sa: *mut c_void, protect: u32, high: u32, low: u32, name: *const u16) -> *mut c_void;
    fn MapViewOfFile(h: *mut c_void, access: u32, high: u32, low: u32, n: usize) -> *mut c_void;
    fn GetCurrentProcessId() -> u32;
}

pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const FWP_E_PROVIDER_NOT_FOUND: u32 = 0x8032_0005;
pub const FWP_E_ALREADY_EXISTS: u32 = 0x8032_0009;
pub const FWP_E_DYNAMIC_SESSION_IN_PROGRESS: u32 = 0x8032_000B;
pub const FWP_E_NO_TXN_IN_PROGRESS: u32 = 0x8032_000D;
pub const FWP_E_TXN_IN_PROGRESS: u32 = 0x8032_000E;

pub const FWPM_SESSION_FLAG_DYNAMIC: u32 = 0x1;
pub const FWPM_PROVIDER_FLAG_PERSISTENT: u32 = 0x1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Provider,
    ProviderContext,
    SubLayer,
    Callout,
    Filter,
    IpsecSaContext,
    IpsecSa,
    IkeSa,
    AleEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub key: [u8; 16],
    pub name: Option<Vec<u16>>,
    pub description: Option<Vec<u16>>,
    pub flags: u32,
    pub data: Vec<u8>,
    pub service_name: Option<Vec<u16>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    Provider(Provider),
}

struct Stored {
    provider: Provider,
    owner: Option<u64>,
}

struct EnumState {
    engine: u64,
    items: Vec<Object>,
    pos: usize,
}

struct Session {
    handle: u64,
    flags: u32,
    txn: bool,
}

struct Store {
    next: u64,
    sessions: Vec<Session>,
    providers: Vec<Stored>,
    enums: Vec<(u64, EnumState)>,
}

pub const SHARED_ARENA: usize = 1 << 20;
pub const FIRST_HANDLE: u64 = 0x5746_5000;

#[repr(C)]
struct Header {
    lock: AtomicU32,
    len: AtomicU32,
    next: AtomicU64,
}

pub fn shared_mapping_name() -> String {
    format!("Local\\TomBox.NetView.Wfp.{}", unsafe { GetCurrentProcessId() })
}

fn shared() -> Option<usize> {
    static P: OnceLock<usize> = OnceLock::new();
    let p = *P.get_or_init(|| unsafe {
        let name: Vec<u16> = shared_mapping_name().encode_utf16().chain(Some(0)).collect();
        let size = (core::mem::size_of::<Header>() + SHARED_ARENA) as u32;
        let h = CreateFileMappingW(-1isize as *mut c_void, core::ptr::null_mut(), 0x04, 0, size, name.as_ptr());
        if h.is_null() {
            return 0;
        }
        MapViewOfFile(h, 0x0006, 0, 0, size as usize) as usize
    });
    (p != 0).then_some(p)
}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
    }
    fn wide(&mut self, v: &Option<Vec<u16>>) {
        match v {
            None => self.u8(0),
            Some(w) => {
                self.u8(1);
                self.u32(w.len() as u32);
                for c in w {
                    self.0.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
    }
    fn provider(&mut self, p: &Provider) {
        self.0.extend_from_slice(&p.key);
        self.wide(&p.name);
        self.wide(&p.description);
        self.u32(p.flags);
        self.bytes(&p.data);
        self.wide(&p.service_name);
    }
    fn object(&mut self, o: &Object) {
        match o {
            Object::Provider(p) => {
                self.u8(0);
                self.provider(p);
            }
        }
    }
}

struct R<'a>(&'a [u8], usize);

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.0.get(self.1..self.1 + n)?;
        self.1 += n;
        Some(b)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        Some(self.take(n)?.to_vec())
    }
    fn wide(&mut self) -> Option<Option<Vec<u16>>> {
        if self.u8()? == 0 {
            return Some(None);
        }
        let n = self.u32()? as usize;
        let b = self.take(n * 2)?;
        Some(Some(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()))
    }
    fn provider(&mut self) -> Option<Provider> {
        let key: [u8; 16] = self.take(16)?.try_into().ok()?;
        Some(Provider { key, name: self.wide()?, description: self.wide()?, flags: self.u32()?, data: self.bytes()?, service_name: self.wide()? })
    }
    fn object(&mut self) -> Option<Object> {
        match self.u8()? {
            0 => Some(Object::Provider(self.provider()?)),
            _ => None,
        }
    }
}

impl Store {
    fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.u32(self.sessions.len() as u32);
        for x in &self.sessions {
            w.u64(x.handle);
            w.u32(x.flags);
            w.u8(x.txn as u8);
        }
        w.u32(self.providers.len() as u32);
        for x in &self.providers {
            w.provider(&x.provider);
            match x.owner {
                None => w.u8(0),
                Some(o) => {
                    w.u8(1);
                    w.u64(o);
                }
            }
        }
        w.u32(self.enums.len() as u32);
        for (h, e) in &self.enums {
            w.u64(*h);
            w.u64(e.engine);
            w.u32(e.pos as u32);
            w.u32(e.items.len() as u32);
            for o in &e.items {
                w.object(o);
            }
        }
        w.0
    }
    fn decode(next: u64, b: &[u8]) -> Option<Store> {
        let mut s = Store { next, sessions: Vec::new(), providers: Vec::new(), enums: Vec::new() };
        if b.is_empty() {
            return Some(s);
        }
        let mut r = R(b, 0);
        for _ in 0..r.u32()? {
            s.sessions.push(Session { handle: r.u64()?, flags: r.u32()?, txn: r.u8()? != 0 });
        }
        for _ in 0..r.u32()? {
            let provider = r.provider()?;
            let owner = if r.u8()? == 0 { None } else { Some(r.u64()?) };
            s.providers.push(Stored { provider, owner });
        }
        for _ in 0..r.u32()? {
            let h = r.u64()?;
            let engine = r.u64()?;
            let pos = r.u32()? as usize;
            let mut items = Vec::new();
            for _ in 0..r.u32()? {
                items.push(r.object()?);
            }
            s.enums.push((h, EnumState { engine, items, pos }));
        }
        Some(s)
    }
}

fn with_store<T>(f: impl FnOnce(&mut Store) -> Result<T, u32>) -> Result<T, u32> {
    let base = shared().ok_or(ERROR_NOT_ENOUGH_MEMORY)?;
    let hdr = unsafe { &*(base as *const Header) };
    while hdr.lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        std::thread::yield_now();
    }
    let arena = unsafe { core::slice::from_raw_parts_mut((base + core::mem::size_of::<Header>()) as *mut u8, SHARED_ARENA) };
    let len = (hdr.len.load(Ordering::Relaxed) as usize).min(SHARED_ARENA);
    let next = match hdr.next.load(Ordering::Relaxed) {
        0 => FIRST_HANDLE,
        n => n,
    };
    let out = match Store::decode(next, &arena[..len]) {
        None => Err(ERROR_NOT_ENOUGH_MEMORY),
        Some(mut st) => match f(&mut st) {
            Err(e) => Err(e),
            Ok(v) => {
                let bytes = st.encode();
                if bytes.len() > SHARED_ARENA {
                    Err(ERROR_NOT_ENOUGH_MEMORY)
                } else {
                    arena[..bytes.len()].copy_from_slice(&bytes);
                    hdr.len.store(bytes.len() as u32, Ordering::Relaxed);
                    hdr.next.store(st.next, Ordering::Relaxed);
                    Ok(v)
                }
            }
        },
    };
    hdr.lock.store(0, Ordering::Release);
    out
}

fn status(r: Result<u32, u32>) -> u32 {
    match r {
        Ok(v) | Err(v) => v,
    }
}

impl Store {
    fn alloc(&mut self) -> u64 {
        self.next += 4;
        self.next
    }
    fn session(&mut self, h: u64) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.handle == h)
    }
}

pub fn engine_open(session_flags: u32) -> Result<u64, u32> {
    with_store(|s| {
        let h = s.alloc();
        s.sessions.push(Session { handle: h, flags: session_flags, txn: false });
        Ok(h)
    })
}

pub fn engine_close(engine: u64) -> u32 {
    status(with_store(|s| {
        let Some(i) = s.sessions.iter().position(|x| x.handle == engine) else {
            return Ok(ERROR_INVALID_HANDLE);
        };
        let sess = s.sessions.remove(i);
        if sess.flags & FWPM_SESSION_FLAG_DYNAMIC != 0 {
            s.providers.retain(|p| p.owner != Some(engine));
        }
        s.enums.retain(|(_, e)| e.engine != engine);
        Ok(0)
    }))
}

pub fn transaction_begin(engine: u64) -> u32 {
    status(with_store(|s| {
        Ok(match s.session(engine) {
            None => ERROR_INVALID_HANDLE,
            Some(x) if x.txn => FWP_E_TXN_IN_PROGRESS,
            Some(x) => {
                x.txn = true;
                0
            }
        })
    }))
}

pub fn transaction_commit(engine: u64) -> u32 {
    status(with_store(|s| {
        Ok(match s.session(engine) {
            None => ERROR_INVALID_HANDLE,
            Some(x) if !x.txn => FWP_E_NO_TXN_IN_PROGRESS,
            Some(x) => {
                x.txn = false;
                0
            }
        })
    }))
}

pub fn transaction_abort(engine: u64) -> u32 {
    transaction_commit(engine)
}

pub fn provider_add(engine: u64, p: Provider) -> u32 {
    status(with_store(|s| {
        let Some(sess) = s.session(engine) else {
            return Ok(ERROR_INVALID_HANDLE);
        };
        let dynamic = sess.flags & FWPM_SESSION_FLAG_DYNAMIC != 0;
        if p.name.is_none() {
            return Ok(ERROR_INVALID_PARAMETER);
        }
        if dynamic && p.flags & FWPM_PROVIDER_FLAG_PERSISTENT != 0 {
            return Ok(FWP_E_DYNAMIC_SESSION_IN_PROGRESS);
        }
        if s.providers.iter().any(|x| x.provider.key == p.key) {
            return Ok(FWP_E_ALREADY_EXISTS);
        }
        s.providers.push(Stored { provider: p, owner: if dynamic { Some(engine) } else { None } });
        Ok(0)
    }))
}

pub fn provider_get_by_key(engine: u64, key: &[u8; 16]) -> Result<Provider, u32> {
    with_store(|s| {
        if s.session(engine).is_none() {
            return Err(ERROR_INVALID_HANDLE);
        }
        s.providers.iter().find(|x| &x.provider.key == key).map(|x| x.provider.clone()).ok_or(FWP_E_PROVIDER_NOT_FOUND)
    })
}

pub fn provider_delete_by_key(engine: u64, key: &[u8; 16]) -> u32 {
    status(with_store(|s| {
        if s.session(engine).is_none() {
            return Ok(ERROR_INVALID_HANDLE);
        }
        Ok(match s.providers.iter().position(|x| &x.provider.key == key) {
            Some(i) => {
                s.providers.remove(i);
                0
            }
            None => FWP_E_PROVIDER_NOT_FOUND,
        })
    }))
}

pub fn create_enum_handle(engine: u64, kind: Kind) -> Result<u64, u32> {
    with_store(|s| {
        if s.session(engine).is_none() {
            return Err(ERROR_INVALID_HANDLE);
        }
        let items = match kind {
            Kind::Provider => s.providers.iter().map(|p| Object::Provider(p.provider.clone())).collect(),
            _ => Vec::new(),
        };
        let h = s.alloc();
        s.enums.push((h, EnumState { engine, items, pos: 0 }));
        Ok(h)
    })
}

pub fn enum_next(engine: u64, handle: u64, requested: u32) -> Result<Vec<Object>, u32> {
    with_store(|s| {
        if s.session(engine).is_none() {
            return Err(ERROR_INVALID_HANDLE);
        }
        let Some((_, e)) = s.enums.iter_mut().find(|(h, e)| *h == handle && e.engine == engine) else {
            return Err(ERROR_INVALID_HANDLE);
        };
        let end = (e.pos + requested as usize).min(e.items.len());
        let out = e.items[e.pos..end].to_vec();
        e.pos = end;
        Ok(out)
    })
}

pub fn destroy_enum_handle(engine: u64, handle: u64) -> u32 {
    status(with_store(|s| {
        if s.session(engine).is_none() {
            return Ok(ERROR_INVALID_HANDLE);
        }
        Ok(match s.enums.iter().position(|(h, e)| *h == handle && e.engine == engine) {
            Some(i) => {
                s.enums.remove(i);
                0
            }
            None => ERROR_INVALID_HANDLE,
        })
    }))
}

pub fn provider_count() -> usize {
    with_store(|s| Ok(s.providers.len())).unwrap_or(0)
}

#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
