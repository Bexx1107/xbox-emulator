use super::sockets::*;
use super::wfp;
use super::*;
use std::os::windows::io::AsRawSocket;

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

unsafe fn gaa(family: u32, flags: u32) -> Vec<u8> {
    let mut size = 0u32;
    assert_eq!(get_adapters_addresses(family, flags, core::ptr::null_mut(), &mut size), ERROR_BUFFER_OVERFLOW);
    let mut small = size - 1;
    let mut buf = vec![0u8; size as usize];
    assert_eq!(get_adapters_addresses(family, flags, buf.as_mut_ptr(), &mut small), ERROR_BUFFER_OVERFLOW);
    assert_eq!(small, size);
    assert_eq!(get_adapters_addresses(family, flags, buf.as_mut_ptr(), &mut size), ERROR_SUCCESS);
    buf
}

unsafe fn sockaddr_at(p: u64, len: u32) -> std::net::SocketAddr {
    parse_sockaddr(p as *const u8, len as i32).unwrap()
}

#[test]
fn adapters_addresses_use_the_lh_layout_with_one_nic_and_loopback() {
    unsafe {
        let buf = gaa(AF_UNSPEC, 6);
        let base = buf.as_ptr() as u64;
        let a = &buf[..];
        assert_eq!(rd32(a, 0), 0x1c0);
        assert_eq!(rd32(a, 4), NIC_IF_INDEX);
        let next = rd64(a, 8);
        assert_eq!(next - base, 0x1c0);
        let name = std::ffi::CStr::from_ptr(rd64(a, 0x10) as *const i8).to_str().unwrap().to_string();
        assert!(name.starts_with('{') && name.len() == 38);
        assert_eq!(&a[0x50..0x56], &NIC_MAC);
        assert_eq!(rd32(a, 0x58), 6);
        assert_eq!(rd32(a, 0x64), IF_TYPE_ETHERNET_CSMACD);
        assert_eq!(rd32(a, 0x68), IF_OPER_STATUS_UP);
        assert_eq!(rd64(a, 0xe0), NIC_LUID);
        assert_eq!(rd64(a, 0x28), 0);
        assert_eq!(rd64(a, 0x30), 0);
        assert_eq!(rd64(a, 0xb0), 0);
        let mut u = rd64(a, 0x18);
        let mut seen = Vec::new();
        while u != 0 {
            let e = core::slice::from_raw_parts(u as *const u8, UNICAST_SIZE);
            assert_eq!(rd32(e, 0), UNICAST_SIZE as u32);
            seen.push((sockaddr_at(rd64(e, 0x10), rd32(e, 0x18)).ip(), e[0x38], rd32(e, 0x20), rd32(e, 0x24), rd32(e, 0x28)));
            u = rd64(e, 8);
        }
        assert_eq!(seen, vec![(IpAddr::V6(NIC_IPV6), 64, 2, 4, 4), (IpAddr::V4(NIC_IPV4), 16, 2, 4, 4)]);
        let lo = core::slice::from_raw_parts(next as *const u8, 0x1c0);
        assert_eq!(rd32(lo, 4), LOOPBACK_IF_INDEX);
        assert_eq!(rd64(lo, 8), 0);
        assert_eq!(rd32(lo, 0x64), IF_TYPE_SOFTWARE_LOOPBACK);
        assert_eq!(rd32(lo, 0x58), 0);
        let fname: Vec<u16> = (0..).map(|i| *(rd64(lo, 0x48) as *const u16).add(i)).take_while(|c| *c != 0).collect();
        assert_eq!(String::from_utf16(&fname).unwrap(), "Loopback Pseudo-Interface 1");
    }
}

#[test]
fn adapter_family_and_list_flags_filter_entries() {
    unsafe {
        let buf = gaa(AF_INET, GAA_FLAG_INCLUDE_PREFIX);
        let a = &buf[..];
        let u = core::slice::from_raw_parts(rd64(a, 0x18) as *const u8, UNICAST_SIZE);
        assert_eq!(sockaddr_at(rd64(u, 0x10), rd32(u, 0x18)).ip(), IpAddr::V4(NIC_IPV4));
        assert_eq!(rd64(u, 8), 0);
        let m = core::slice::from_raw_parts(rd64(a, 0x28) as *const u8, MULTICAST_SIZE);
        assert_eq!(sockaddr_at(rd64(m, 0x10), rd32(m, 0x18)).ip(), IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)));
        let p = core::slice::from_raw_parts(rd64(a, 0xb0) as *const u8, PREFIX_SIZE);
        assert_eq!(sockaddr_at(rd64(p, 0x10), rd32(p, 0x18)).ip(), IpAddr::V4(Ipv4Addr::new(169, 254, 0, 0)));
        assert_eq!(rd32(p, 0x20), 16);
        let mut size = 0u32;
        assert_eq!(get_adapters_addresses(5, 0, core::ptr::null_mut(), &mut size), ERROR_INVALID_PARAMETER);
        assert_eq!(get_adapters_addresses(AF_INET, 0, core::ptr::null_mut(), core::ptr::null_mut()), ERROR_INVALID_PARAMETER);
    }
}

#[test]
fn forward_table_has_on_link_routes_and_no_default_route() {
    let t = ip_forward_table2(AF_UNSPEC).unwrap();
    let n = rd32(&t, 0) as usize;
    assert_eq!(t.len(), 8 + n * ROUTE_ROW_SIZE);
    assert_eq!(n, routes().len());
    let row = &t[8..8 + ROUTE_ROW_SIZE];
    assert_eq!(rd64(row, 0), LOOPBACK_LUID);
    assert_eq!(rd32(row, 8), LOOPBACK_IF_INDEX);
    assert_eq!(&row[0xc..0x14], &[2, 0, 0, 0, 127, 0, 0, 0]);
    assert_eq!(row[0x28], 8);
    assert_eq!(rd32(row, 0x54), 256);
    assert_eq!(rd32(row, 0x58), ROUTE_PROTOCOL_LOCAL);
    assert_eq!(row[0x5c], 1);
    assert_eq!(rd32(row, 0x64), ROUTE_ORIGIN_WELL_KNOWN);
    assert!(routes().iter().all(|r| r.prefix_len != 0));
    assert!(has_route_to(&"127.5.5.5".parse().unwrap()));
    assert!(has_route_to(&IpAddr::V4(NIC_IPV4)));
    assert!(!has_route_to(&"8.8.8.8".parse().unwrap()));
    assert!(!has_route_to(&"2001:db8::1".parse().unwrap()));
    let v6 = ip_forward_table2(AF_INET6).unwrap();
    assert_eq!(rd32(&v6, 0) as usize, routes().iter().filter(|r| r.dest.is_ipv6()).count());
    assert_eq!(ip_forward_table2(7), Err(ERROR_INVALID_PARAMETER));
}

#[test]
fn teredo_reports_offline_state_and_no_port() {
    unsafe {
        let mut rw = [0xffu8; 12];
        let m = NPI_MS_TEREDO_MODULEID;
        let n = core::ptr::null_mut();
        assert_eq!(nsi_get_all_parameters(1, m.as_ptr(), 1, core::ptr::null(), 0, rw.as_mut_ptr(), 12, n, 0, n, 0), 0);
        assert_eq!(rd32(&rw, 0), TEREDO_STATE_OFFLINE);
        assert_ne!(rd32(&rw, 0), TEREDO_STATE_QUALIFIED);
        assert_eq!(&rw[4..], &[0; 8]);
        let mut port = [0u8; 2];
        assert_eq!(nsi_get_all_parameters(1, m.as_ptr(), 3, core::ptr::null(), 0, port.as_mut_ptr(), 2, n, 0, n, 0), ERROR_NOT_READY);
        assert_eq!(nsi_get_all_parameters(1, m.as_ptr(), 1, core::ptr::null(), 0, rw.as_mut_ptr(), 8, n, 0, n, 0), ERROR_INVALID_PARAMETER);
        let mut other = m;
        other[8] = 0;
        assert_eq!(nsi_get_all_parameters(1, other.as_ptr(), 1, core::ptr::null(), 0, rw.as_mut_ptr(), 12, n, 0, n, 0), ERROR_NOT_FOUND);
    }
}

#[test]
fn socket_registry_reports_live_title_sockets_in_owner_module_tables() {
    let _g = super::sockets::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = l.local_addr().unwrap();
    let sa = sockaddr_bytes(&peer.ip(), peer.port(), 0);
    unsafe {
        assert!(report_socket(l.as_raw_socket() as usize, core::ptr::null(), 0));
        assert!(report_socket(c.as_raw_socket() as usize, sa.as_ptr(), sa.len() as i32));
        assert!(report_socket(u.as_raw_socket() as usize, core::ptr::null(), 0));
        assert!(!report_socket(0xdead_beef, core::ptr::null(), 0));
        let mut size = 0u32;
        assert_eq!(get_extended_tcp_table(core::ptr::null_mut(), &mut size, true, AF_INET, TCP_TABLE_OWNER_MODULE_ALL, 0), ERROR_INSUFFICIENT_BUFFER);
        let mut buf = vec![0u8; size as usize];
        assert_eq!(get_extended_tcp_table(buf.as_mut_ptr(), &mut size, true, AF_INET, TCP_TABLE_OWNER_MODULE_ALL, 0), ERROR_SUCCESS);
        let n = rd32(&buf, 0) as usize;
        let rows: Vec<&[u8]> = (0..n).map(|i| &buf[8 + i * TCP_ROW_OWNER_MODULE_SIZE..8 + (i + 1) * TCP_ROW_OWNER_MODULE_SIZE]).collect();
        let pid = std::process::id();
        let listen = rows.iter().find(|r| rd32(r, 0) == MIB_TCP_STATE_LISTEN && u16::from_be_bytes([r[8], r[9]]) == peer.port()).expect("listener row");
        assert_eq!(&listen[4..8], &[127, 0, 0, 1]);
        assert_eq!(rd32(listen, 0x14), pid);
        assert_ne!(rd64(listen, 0x18), 0);
        let est = rows.iter().find(|r| rd32(r, 0) == MIB_TCP_STATE_ESTAB && u16::from_be_bytes([r[0x10], r[0x11]]) == peer.port()).expect("connected row");
        assert_eq!(u16::from_be_bytes([est[8], est[9]]), c.local_addr().unwrap().port());
        let t = udp_table(AF_INET, UDP_TABLE_OWNER_MODULE, true).unwrap();
        let un = rd32(&t, 0) as usize;
        assert!((0..un).any(|i| {
            let r = &t[8 + i * UDP_ROW_OWNER_MODULE_SIZE..];
            u16::from_be_bytes([r[4], r[5]]) == u.local_addr().unwrap().port() && rd32(r, 8) == pid
        }));
        let pidt = tcp_table(AF_INET, TCP_TABLE_OWNER_PID_LISTENER, false).unwrap();
        assert_eq!((pidt.len() - 4) % TCP_ROW_OWNER_PID_SIZE, 0);
        assert!(tcp_table(AF_INET, 2, true).is_err());
        let ls = l.as_raw_socket() as u64;
        drop(l);
        assert!(!endpoints().iter().any(|e| e.socket == ls));
        assert!(!snapshot().iter().any(|e| e.socket == ls));
    }
}

#[test]
fn console_view_maps_host_lan_addresses_to_the_emulated_nic() {
    let _g = super::sockets::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a: std::net::SocketAddr = "192.168.1.20:5000".parse().unwrap();
    assert_eq!(console_local(a), std::net::SocketAddr::new(IpAddr::V4(NIC_IPV4), 5000));
    let l: std::net::SocketAddr = "127.0.0.1:7".parse().unwrap();
    assert_eq!(console_local(l), l);
    let z: std::net::SocketAddr = "0.0.0.0:9".parse().unwrap();
    assert_eq!(console_local(z), z);
}

#[test]
fn wfp_store_is_empty_and_keeps_dynamic_providers_per_session() {
    let e = wfp::engine_open(wfp::FWPM_SESSION_FLAG_DYNAMIC).unwrap();
    for kind in [wfp::Kind::ProviderContext, wfp::Kind::SubLayer, wfp::Kind::Callout, wfp::Kind::Filter, wfp::Kind::IpsecSaContext, wfp::Kind::IpsecSa, wfp::Kind::IkeSa, wfp::Kind::AleEndpoint] {
        let h = wfp::create_enum_handle(e, kind).unwrap();
        assert!(wfp::enum_next(e, h, 0x40).unwrap().is_empty());
        assert_eq!(wfp::destroy_enum_handle(e, h), 0);
        assert_eq!(wfp::destroy_enum_handle(e, h), wfp::ERROR_INVALID_HANDLE);
    }
    let key = [7u8; 16];
    assert_eq!(wfp::provider_get_by_key(e, &key), Err(wfp::FWP_E_PROVIDER_NOT_FOUND));
    let p = wfp::Provider { key, name: Some("p".encode_utf16().collect()), description: None, flags: 0, data: Vec::new(), service_name: None };
    assert_eq!(wfp::provider_add(e, wfp::Provider { flags: wfp::FWPM_PROVIDER_FLAG_PERSISTENT, ..p.clone() }), wfp::FWP_E_DYNAMIC_SESSION_IN_PROGRESS);
    assert_eq!(wfp::provider_add(e, p.clone()), 0);
    assert_eq!(wfp::provider_add(e, p.clone()), wfp::FWP_E_ALREADY_EXISTS);
    assert_eq!(wfp::provider_get_by_key(e, &key), Ok(p.clone()));
    let h = wfp::create_enum_handle(e, wfp::Kind::Provider).unwrap();
    let first = wfp::enum_next(e, h, 0x40).unwrap();
    assert!(first.contains(&wfp::Object::Provider(p.clone())));
    assert!(wfp::enum_next(e, h, 0x40).unwrap().is_empty());
    assert_eq!(wfp::destroy_enum_handle(e, h), 0);
    assert_eq!(wfp::transaction_abort(e), wfp::FWP_E_NO_TXN_IN_PROGRESS);
    assert_eq!(wfp::transaction_begin(e), 0);
    assert_eq!(wfp::transaction_begin(e), wfp::FWP_E_TXN_IN_PROGRESS);
    assert_eq!(wfp::transaction_abort(e), 0);
    assert_eq!(wfp::engine_close(e), 0);
    assert_eq!(wfp::engine_close(e), wfp::ERROR_INVALID_HANDLE);
    let e2 = wfp::engine_open(0).unwrap();
    assert_eq!(wfp::provider_get_by_key(e2, &key), Err(wfp::FWP_E_PROVIDER_NOT_FOUND));
    assert_eq!(wfp::provider_delete_by_key(e2, &key), wfp::FWP_E_PROVIDER_NOT_FOUND);
    assert_eq!(wfp::engine_close(e2), 0);
    assert_eq!(wfp::create_enum_handle(e2, wfp::Kind::Provider), Err(wfp::ERROR_INVALID_HANDLE));
}
