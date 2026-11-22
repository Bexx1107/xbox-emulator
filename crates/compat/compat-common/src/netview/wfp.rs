use std::sync::Mutex;

pub const ERROR_INVALID_HANDLE: u32 = 6;
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

static STORE: Mutex<Store> = Mutex::new(Store { next: 0x5746_5000, sessions: Vec::new(), providers: Vec::new(), enums: Vec::new() });

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
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
    let mut s = store();
    let h = s.alloc();
    s.sessions.push(Session { handle: h, flags: session_flags, txn: false });
    Ok(h)
}

pub fn engine_close(engine: u64) -> u32 {
    let mut s = store();
    let Some(i) = s.sessions.iter().position(|x| x.handle == engine) else {
        return ERROR_INVALID_HANDLE;
    };
    let sess = s.sessions.remove(i);
    if sess.flags & FWPM_SESSION_FLAG_DYNAMIC != 0 {
        s.providers.retain(|p| p.owner != Some(engine));
    }
    s.enums.retain(|(_, e)| e.engine != engine);
    0
}

pub fn transaction_begin(engine: u64) -> u32 {
    let mut s = store();
    match s.session(engine) {
        None => ERROR_INVALID_HANDLE,
        Some(x) if x.txn => FWP_E_TXN_IN_PROGRESS,
        Some(x) => {
            x.txn = true;
            0
        }
    }
}

pub fn transaction_commit(engine: u64) -> u32 {
    let mut s = store();
    match s.session(engine) {
        None => ERROR_INVALID_HANDLE,
        Some(x) if !x.txn => FWP_E_NO_TXN_IN_PROGRESS,
        Some(x) => {
            x.txn = false;
            0
        }
    }
}

pub fn transaction_abort(engine: u64) -> u32 {
    transaction_commit(engine)
}

pub fn provider_add(engine: u64, p: Provider) -> u32 {
    let mut s = store();
    let Some(sess) = s.session(engine) else {
        return ERROR_INVALID_HANDLE;
    };
    let dynamic = sess.flags & FWPM_SESSION_FLAG_DYNAMIC != 0;
    if p.name.is_none() {
        return ERROR_INVALID_PARAMETER;
    }
    if dynamic && p.flags & FWPM_PROVIDER_FLAG_PERSISTENT != 0 {
        return FWP_E_DYNAMIC_SESSION_IN_PROGRESS;
    }
    if s.providers.iter().any(|x| x.provider.key == p.key) {
        return FWP_E_ALREADY_EXISTS;
    }
    s.providers.push(Stored { provider: p, owner: if dynamic { Some(engine) } else { None } });
    0
}

pub fn provider_get_by_key(engine: u64, key: &[u8; 16]) -> Result<Provider, u32> {
    let mut s = store();
    if s.session(engine).is_none() {
        return Err(ERROR_INVALID_HANDLE);
    }
    s.providers.iter().find(|x| &x.provider.key == key).map(|x| x.provider.clone()).ok_or(FWP_E_PROVIDER_NOT_FOUND)
}

pub fn provider_delete_by_key(engine: u64, key: &[u8; 16]) -> u32 {
    let mut s = store();
    if s.session(engine).is_none() {
        return ERROR_INVALID_HANDLE;
    }
    match s.providers.iter().position(|x| &x.provider.key == key) {
        Some(i) => {
            s.providers.remove(i);
            0
        }
        None => FWP_E_PROVIDER_NOT_FOUND,
    }
}

pub fn create_enum_handle(engine: u64, kind: Kind) -> Result<u64, u32> {
    let mut s = store();
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
}

pub fn enum_next(engine: u64, handle: u64, requested: u32) -> Result<Vec<Object>, u32> {
    let mut s = store();
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
}

pub fn destroy_enum_handle(engine: u64, handle: u64) -> u32 {
    let mut s = store();
    if s.session(engine).is_none() {
        return ERROR_INVALID_HANDLE;
    }
    match s.enums.iter().position(|(h, e)| *h == handle && e.engine == engine) {
        Some(i) => {
            s.enums.remove(i);
            0
        }
        None => ERROR_INVALID_HANDLE,
    }
}

pub fn provider_count() -> usize {
    store().providers.len()
}
