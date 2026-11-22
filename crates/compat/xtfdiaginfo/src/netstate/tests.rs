use super::capture::{self, Record};
use super::guids::guid_text;
use super::wfp::{self as section, G_FORMAT, G_SESSION};
use super::*;
use crate::trace::{self, WFP_GUID};
use compat_common::netview::sockets;
use compat_common::netview::wfp as store;
use std::os::windows::io::AsRawSocket;

fn ids(v: &[Record], g: &GUID) -> Vec<u16> {
    v.iter().filter(|r| r.guid == *g).map(|r| r.id).collect()
}

fn wtext(b: &[u8]) -> String {
    let w: Vec<u16> = b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(w.last(), Some(&0));
    String::from_utf16(&w[..w.len() - 1]).unwrap()
}

fn atext(b: &[u8]) -> String {
    assert_eq!(b.last(), Some(&0));
    String::from_utf8(b[..b.len() - 1].to_vec()).unwrap()
}

fn d32(b: &[u8]) -> u32 {
    assert_eq!(b.len(), 4);
    u32::from_le_bytes(b.try_into().unwrap())
}

fn d64(b: &[u8]) -> u64 {
    assert_eq!(b.len(), 8);
    u64::from_le_bytes(b.try_into().unwrap())
}

fn find<'a>(v: &'a [Record], g: &GUID, id: u16) -> Vec<&'a Record> {
    v.iter().filter(|r| r.guid == *g && r.id == id).collect()
}

static NETSTATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    NETSTATE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn run<T>(f: impl FnOnce() -> T) -> (T, Vec<Record>) {
    capture::start();
    let out = f();
    (out, capture::take())
}

#[test]
fn record_arguments_use_the_wpp_wire_encoding() {
    let _g = lock();
    let (_, v) = run(|| unsafe { rec(Gate::State, &ADAPTERS_GUID, 0x33, &[Z, Arg::W(None), Arg::A(None), w(&ws("ab")), Arg::A(Some(b"x")), r(5), r(0), u(7)]) });
    assert_eq!(v.len(), 1);
    let p = &v[0].parts;
    assert_eq!(p[0], vec![0u8; 8]);
    assert_eq!(p[1], b"N\0U\0L\0L\0\0\0".to_vec());
    assert_eq!(p[2], b"NULL\0".to_vec());
    assert_eq!(p[3], b"a\0b\0\0\0".to_vec());
    assert_eq!(p[4], b"x\0".to_vec());
    assert_eq!(d64(&p[5]), REDACTED);
    assert_eq!(d64(&p[6]), 0);
    assert_eq!(d32(&p[7]), 7);
    assert_eq!(hr_map(0x15), 0x8007_0015);
    assert_eq!(hr_map(0x8032_0005), 0x8032_0005);
    assert_eq!(hr_map(0), 0);
}

#[test]
fn state_records_use_the_c3d66fc7_block_and_stay_quiet_without_it() {
    let _g = lock();
    assert_eq!(trace::STATE_CONTROL_GUID, GUID::new(0xc3d66fc7, 0x5736, 0x46d5, [0xb9, 0x42, 0xc4, 0x04, 0xf5, 0x02, 0xdd, 0xe4]));
    let _t = crate::tests::TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!capture::active());
    assert_eq!(trace::WPP_STATE.logger.load(Ordering::Acquire), 0);
    assert!(!enabled(Gate::State));
    unsafe {
        assert!(!trace::wpp_message(&trace::STATE_HEADER_GUID, 0xe, 0));
    }
}

#[test]
fn adapters_section_dumps_the_emulated_nic_loopback_and_on_link_routes() {
    let _g = lock();
    let (hr, v) = run(|| unsafe { log_adapters() });
    assert_eq!(hr, 0);
    assert!(v.iter().all(|r| r.guid == ADAPTERS_GUID && r.gate == Gate::State));
    assert_eq!(v.first().unwrap().id, 0x92);
    let last = v.last().unwrap();
    assert_eq!(last.id, 0x99);
    assert_eq!(d32(&last.parts[1]), 0);
    assert_eq!(find(&v, &ADAPTERS_GUID, 0x96).len(), 2);
    let names: Vec<String> = find(&v, &ADAPTERS_GUID, 0x35).iter().map(|r| wtext(&r.parts[1])).collect();
    assert_eq!(names, vec!["Ethernet".to_string(), "Loopback Pseudo-Interface 1".to_string()]);
    let descs: Vec<String> = find(&v, &ADAPTERS_GUID, 0x34).iter().map(|r| wtext(&r.parts[1])).collect();
    assert_eq!(descs[0], "TomBox Emulated Console Network Adapter");
    assert!(find(&v, &ADAPTERS_GUID, 0x13).iter().all(|r| atext(&r.parts[1]).starts_with('{')));
    let mac = find(&v, &ADAPTERS_GUID, 0x36);
    assert_eq!(mac.len(), 1);
    let bytes: Vec<u8> = mac[0].parts[1..].iter().map(|b| d32(b) as u8).collect();
    assert_eq!(bytes, netview::NIC_MAC.to_vec());
    assert_eq!(find(&v, &ADAPTERS_GUID, 0x38).len(), 1);
    let unicast: Vec<String> = find(&v, &ADAPTERS_GUID, 0x0e).iter().filter(|r| wtext(&r.parts[1]) == SP12).map(|r| wtext(&r.parts[3])).collect();
    assert!(unicast.contains(&"169.254.66.1".to_string()));
    assert!(unicast.contains(&"127.0.0.1".to_string()));
    assert!(unicast.contains(&"::1".to_string()));
    assert!(unicast.iter().any(|s| s.starts_with("fe80::54:42ff:fe00:1")));
    assert!(find(&v, &ADAPTERS_GUID, 0x1a).iter().any(|r| atext(&r.parts[2]) == "WellKnown"));
    assert!(find(&v, &ADAPTERS_GUID, 0x1c).iter().all(|r| atext(&r.parts[2]) == "Preferred"));
    assert!(find(&v, &ADAPTERS_GUID, 0x27).iter().all(|r| d64(&r.parts[1]) == 0));
    let dhcp: Vec<String> = find(&v, &ADAPTERS_GUID, 0x0e).iter().filter(|r| wtext(&r.parts[1]) == SP8).map(|r| wtext(&r.parts[3])).collect();
    assert_eq!(dhcp, vec!["nullptr".to_string(); 4]);
    let routes = netview::routes();
    let summary = find(&v, &ADAPTERS_GUID, 0x98);
    assert_eq!(summary.len(), 1);
    assert_eq!(d32(&summary[0].parts[1]) as usize, routes.len());
    assert_eq!(find(&v, &ADAPTERS_GUID, 0x71).len(), routes.len());
    assert!(find(&v, &ADAPTERS_GUID, 0x61).iter().all(|r| wtext(&r.parts[1]) == SP8));
    let dests: Vec<String> = find(&v, &ADAPTERS_GUID, 0x0c).iter().filter(|r| wtext(&r.parts[1]) == SP16).map(|r| wtext(&r.parts[3])).collect();
    assert_eq!(dests.len(), routes.len());
    assert!(dests.contains(&"169.254.0.0".to_string()));
    assert!(!dests.contains(&"0.0.0.0".to_string()));
    assert!(v.iter().all(|r| r.gate != Gate::Error));
}

#[test]
fn teredo_section_reports_the_offline_state_and_tolerates_the_missing_port() {
    let _g = lock();
    let (hr, v) = run(|| unsafe { log_teredo() });
    assert_eq!(hr, 0);
    assert_eq!(ids(&v, &TEREDO_GUID), vec![0x12, 0x16, 0x18, 0x1b]);
    let s = &v[2].parts;
    assert_eq!(s.len(), 6);
    assert_eq!(d32(&s[1]), netview::TEREDO_STATE_OFFLINE);
    assert!(s[2..].iter().all(|b| d32(b) == 0));
    assert_eq!(d32(&v[3].parts[1]), 0);
    assert!(v.iter().all(|r| r.gate == Gate::State));
}

#[test]
fn processes_section_lists_title_processes_and_their_modules() {
    let _g = lock();
    let (hr, v) = run(|| unsafe { log_processes() });
    assert_eq!(hr, 0);
    assert_eq!(v.first().unwrap().id, 0x1d);
    assert_eq!(v.last().unwrap().id, 0x21);
    let listed = d32(&find(&v, &PROCESSES_GUID, 0x1f)[0].parts[1]) as usize;
    assert!(listed >= 1);
    assert_eq!(find(&v, &PROCESSES_GUID, 0x20).len(), listed);
    let me = std::process::id();
    let row = find(&v, &PROCESSES_GUID, 0x20).into_iter().find(|r| d32(&r.parts[1]) == me).expect("current process row");
    let exe = std::env::current_exe().unwrap();
    let leaf = exe.file_name().unwrap().to_string_lossy().to_lowercase();
    let image = wtext(&row.parts[3]).to_lowercase();
    assert!(image.ends_with(&format!("\\{}", leaf)), "{}", image);
    assert!(std::path::Path::new(&image).is_file());
    let mods: Vec<String> = find(&v, &PROCESSES_GUID, 0x19).iter().map(|r| wtext(&r.parts[2]).to_lowercase()).collect();
    assert!(mods.iter().any(|m| m == "ntdll.dll"));
    assert!(mods.iter().any(|m| m == "kernel32.dll"));
    assert!(find(&v, &PROCESSES_GUID, 0x0b).iter().all(|r| d32(&r.parts[1]) % 0x100 == 0));
    unsafe {
        assert_eq!(wtext(&encode(&w(&process_name(0)))), "<invalid process ID>");
        assert_eq!(wtext(&encode(&w(&process_name(4)))), "<system process ID>");
    }
    let (name, ev) = run(|| unsafe { process_name(0x7fff_fff0) });
    assert_eq!(String::from_utf16(&name).unwrap(), "<couldn't open process, err 87>");
    assert_eq!(ids(&ev, &PROCESSES_GUID), vec![0x1a]);
    assert_eq!(ev[0].gate, Gate::Error);
}

#[test]
fn ports_section_lists_sockets_the_title_opened() {
    let _g = lock();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let ud = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = l.local_addr().unwrap();
    let sa = netview::sockaddr_bytes(&peer.ip(), peer.port(), 0);
    unsafe {
        assert!(sockets::report_socket(c.as_raw_socket() as usize, sa.as_ptr(), sa.len() as i32));
        assert!(sockets::report_socket(ud.as_raw_socket() as usize, core::ptr::null(), 0));
    }
    let (hr, v) = run(|| unsafe { log_ports() });
    assert_eq!(hr, 0);
    assert_eq!(v.first().unwrap().id, 0x3b);
    assert_eq!(v.last().unwrap().id, 0x44);
    for id in [0x3d, 0x3f, 0x41, 0x43] {
        assert_eq!(find(&v, &PORTS_GUID, id).len(), 1);
    }
    let pre = SP8.to_string();
    let states = find(&v, &PORTS_GUID, 0x0a);
    assert!(states.iter().all(|r| wtext(&r.parts[1]) == pre));
    assert!(states.iter().any(|r| d32(&r.parts[2]) == 5 && wtext(&r.parts[3]) == "MIB_TCP_STATE_ESTAB"));
    let lports: Vec<u32> = find(&v, &PORTS_GUID, 0x0c).iter().map(|r| d32(&r.parts[2])).collect();
    assert!(lports.contains(&(c.local_addr().unwrap().port() as u32)));
    let rports: Vec<u32> = find(&v, &PORTS_GUID, 0x0e).iter().map(|r| d32(&r.parts[2])).collect();
    assert!(rports.contains(&(peer.port() as u32)));
    assert!(find(&v, &PORTS_GUID, 0x0b).iter().any(|r| wtext(&r.parts[2]) == "127.0.0.1"));
    let me = std::process::id();
    assert!(find(&v, &PORTS_GUID, 0x0f).iter().all(|r| d32(&r.parts[2]) == me && d32(&r.parts[3]) == me));
    let uports: Vec<u32> = find(&v, &PORTS_GUID, 0x1f).iter().map(|r| d32(&r.parts[2])).collect();
    assert!(uports.contains(&(ud.local_addr().unwrap().port() as u32)));
    assert!(find(&v, &PORTS_GUID, 0x23).iter().all(|r| d32(&r.parts[2]) == 0));
    assert!(find(&v, &PROCESSES_GUID, 0x1a).is_empty());
    assert!(v.iter().all(|r| r.gate == Gate::State));
    drop(c);
    drop(l);
}

#[test]
fn wfp_guid_names_follow_the_console_table() {
    let _g = lock();
    assert_eq!(guid_text(None), "(nullptr)");
    assert_eq!(guid_text(Some(&[0; 16])), "GUID_NULL");
    let inbound = [0xbf, 0xd1, 0x6f, 0xc8, 0xcd, 0x21, 0x7e, 0x49, 0xa0, 0xbb, 0x17, 0x42, 0x5c, 0x88, 0x5c, 0x58];
    assert_eq!(guid_text(Some(&inbound)), "FWPM_LAYER_INBOUND_IPPACKET_V4");
    let mut plumber = [0u8; 16];
    plumber[..14].copy_from_slice(&guids::FW_PLUMBER_PREFIX);
    plumber[14] = 1;
    plumber[15] = 7;
    assert_eq!(guid_text(Some(&plumber)), "FwPlumberCalloutLogging_layer_7");
    plumber[14] = 9;
    assert_eq!(guid_text(Some(&plumber)), "FwPlumberCallout_type_9_layer_7");
    let g = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10];
    assert_eq!(guid_text(Some(&g)), "{04030201-0605-0807-090A-0B0C0D0E0F10}");
    assert_eq!(guids::NAMES.len(), 324);
}

#[test]
fn wfp_section_enumerates_its_dynamic_provider_and_empty_object_stores() {
    let _g = lock();
    let before = store::provider_count();
    let (hr, v) = run(|| unsafe { section::log_wfp() });
    assert_eq!(hr, 0);
    assert_eq!(store::provider_count(), before);
    assert!(v.iter().all(|r| r.gate != Gate::Error));
    let s = ids(&v, &WFP_GUID);
    assert_eq!(s.first(), Some(&0x9c));
    assert_eq!(s.last(), Some(&0xa7));
    let fin = find(&v, &WFP_GUID, 0xa7);
    assert_eq!(d32(&fin[0].parts[1]), 0);
    let provider_names: Vec<String> = find(&v, &G_FORMAT, 0xb).iter().map(|r| wtext(&r.parts[1])).collect();
    assert_eq!(provider_names, vec![section::PROVIDER_NAME.to_string()]);
    assert_eq!(wtext(&find(&v, &G_FORMAT, 0x1c)[0].parts[1]), "0x00000000 ()");
    assert_eq!(ids(&v, &G_FORMAT), vec![0x1a, 0x1b, 0xb, 0xe, 0x1c, 0x1d, 0x17, 0x19, 0x1f]);
    assert_eq!(find(&v, &WFP_GUID, 0xe).len(), 1);
    for (end, id) in [(0xf, "providers"), (0x15, "contexts"), (0x1b, "sublayers"), (0x21, "callouts"), (0x7f, "filters"), (0x95, "ipsec"), (0x9b, "ale")] {
        let r = find(&v, &WFP_GUID, end);
        assert_eq!(r.len(), 1, "{}", id);
        assert_eq!(d32(&r[0].parts[1]), 0, "{}", id);
    }
    for (pre, _) in section::FILTER_LAYERS.iter() {
        assert_eq!(find(&v, &WFP_GUID, *pre).len(), 1);
    }
    let counts = find(&v, &WFP_GUID, 0x7e);
    assert_eq!(counts.len(), 17);
    assert!(counts.iter().all(|r| d32(&r.parts[1]) == 0));
    assert!(find(&v, &WFP_GUID, 0x7b).is_empty());
    for id in [0x8a, 0x8d, 0x94, 0x99] {
        assert!(find(&v, &WFP_GUID, id).is_empty());
    }
    assert_eq!(find(&v, &G_SESSION, 0x1b).len(), 1);
    assert_eq!(find(&v, &G_SESSION, 0x19).len(), 1);
    assert_eq!(find(&v, &G_SESSION, 0x2d).len(), 1);
    assert_eq!(find(&v, &G_SESSION, 0x35).len(), 1);
    assert_eq!(find(&v, &G_SESSION, 0x36).len(), 2);
    let created: usize = [0x5c, 0x7f, 0x9a, 0xa3, 0xc5, 0xf9, 0x10b, 0x12a, 0x13e].iter().map(|id| find(&v, &G_SESSION, *id).len()).sum();
    assert_eq!(created, 4 + 17 + 4);
    let destroyed: usize = [0x62, 0x87, 0xa0, 0xa9, 0xcb, 0xff, 0x111, 0x132, 0x144].iter().map(|id| find(&v, &G_SESSION, *id).len()).sum();
    assert_eq!(destroyed, created);
    assert!(find(&v, &G_SESSION, 0x5e).is_empty());
}

#[test]
fn log_state_sections_runs_all_five_sections_without_errors() {
    let _g = lock();
    let (out, v) = run(|| unsafe { log_state_sections() });
    assert_eq!(out, [0; 5], "{:?}", v.iter().filter(|r| r.gate == Gate::Error).map(|r| (r.guid, r.id)).collect::<Vec<_>>());
    for g in [ADAPTERS_GUID, TEREDO_GUID, PROCESSES_GUID, PORTS_GUID, WFP_GUID] {
        assert!(v.iter().any(|r| r.guid == g));
    }
    let me = std::process::id();
    let per_process = [0x1a, 0x1c, 0x14, 0x11, 0x12, 0x17];
    for r in v.iter().filter(|r| r.gate == Gate::Error) {
        assert!(r.guid == PROCESSES_GUID && per_process.contains(&r.id), "unexpected error record {:x}", r.id);
        if matches!(r.id, 0x1a | 0x1c | 0x14) {
            assert_ne!(d32(&r.parts[1]), me, "own process failed in record {:x}", r.id);
        }
    }
}
