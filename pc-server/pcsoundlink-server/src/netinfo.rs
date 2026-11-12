//! Сетевая диагностика: какой адрес показывать телефону, есть ли VPN и не
//! является ли сеть нестандартной.
//!
//! Телефон ищет компьютер широковещательными UDP-пакетами, поэтому:
//! * VPN-адаптеры могут «забрать» широковещательный трафик;
//! * адрес из чужой подсети (CGNAT, публичный IP, мобильный интернет) телефону
//!   бесполезен.
//!
//! Модуль разбирает список адаптеров Windows через `GetAdaptersAddresses` и
//! выбирает тот, к которому подключён телефон (физический адаптер со шлюзом).

use std::net::Ipv4Addr;

use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_TYPE_TUNNEL: u32 = 131;
const ERROR_BUFFER_OVERFLOW: u32 = 111;

#[derive(Debug, Clone)]
pub struct Adapter {
    pub name: String,
    pub ip: Option<Ipv4Addr>,
    pub has_gateway: bool,
    pub vpn: bool,
    pub up: bool,
    pub metric: u32,
}

#[derive(Debug, Clone, Default)]
pub struct NetReport {
    pub primary: Option<Ipv4Addr>,
    pub primary_name: Option<String>,
    pub adapters: Vec<Adapter>,
    pub warnings: Vec<String>,
}

pub fn analyze() -> NetReport {
    let adapters = enumerate_adapters();
    let primary = pick_primary(&adapters);
    let primary_name = primary.and_then(|ip| {
        adapters
            .iter()
            .find(|a| a.ip == Some(ip))
            .map(|a| a.name.clone())
    });

    let mut warnings = Vec::new();
    let vpn = adapters
        .iter()
        .filter(|a| a.vpn && a.up)
        .map(|a| a.name.clone())
        .collect::<Vec<_>>();

    if !vpn.is_empty() {
        warnings.push(format!(
            "Похоже, включён VPN ({}). Он может мешать: телефон не найдёт компьютер, \
             а звук будет прерываться. Если поиск не работает — отключите VPN.",
            vpn.join(", ")
        ));
    }

    match primary {
        Some(ip) if !ip.is_private() => warnings.push(format!(
            "Сеть нестандартная: адрес {ip} не из домашнего диапазона. Возможно, \
             включён VPN или мобильный интернет — телефон может не найти компьютер."
        )),
        Some(_) => {}
        None => warnings.push(
            "Похоже, нет подключения к локальной сети. Подключите компьютер и телефон \
             к одному Wi-Fi."
                .to_string(),
        ),
    }

    let mut subnets = Vec::new();
    for adapter in &adapters {
        if !adapter.up || adapter.vpn {
            continue;
        }
        let Some(ip) = adapter.ip else {
            continue;
        };
        if !ip.is_private() {
            continue;
        }
        let octets = ip.octets();
        let subnet = format!("{}.{}.{}.x", octets[0], octets[1], octets[2]);
        if !subnets.contains(&subnet) {
            subnets.push(subnet);
        }
    }
    if subnets.len() > 1 {
        if let Some(ip) = primary {
            warnings.push(format!(
                "В компьютере несколько сетей ({}). Убедитесь, что телефон подключён \
                 к той же сети, что и адрес {ip}.",
                subnets.join(", ")
            ));
        }
    }

    NetReport {
        primary,
        primary_name,
        adapters,
        warnings,
    }
}

fn pick_primary(adapters: &[Adapter]) -> Option<Ipv4Addr> {
    let mut usable: Vec<&Adapter> = adapters
        .iter()
        .filter(|a| a.up && !a.vpn && a.ip.is_some())
        .collect();
    usable.sort_by_key(|a| a.metric);

    // 1. Физический адаптер со шлюзом и домашним адресом.
    if let Some(adapter) = usable
        .iter()
        .find(|a| a.has_gateway && a.ip.map(|ip| ip.is_private()).unwrap_or(false))
    {
        return adapter.ip;
    }
    // 2. Любой физический адаптер с домашним адресом.
    if let Some(adapter) = usable
        .iter()
        .find(|a| a.ip.map(|ip| ip.is_private()).unwrap_or(false))
    {
        return adapter.ip;
    }
    // 3. Любой физический адаптер со шлюзом.
    if let Some(adapter) = usable.iter().find(|a| a.has_gateway) {
        return adapter.ip;
    }
    // 4. Что угодно, кроме VPN.
    if let Some(adapter) = usable.first() {
        return adapter.ip;
    }
    // 5. Последний шанс — VPN-адрес.
    adapters
        .iter()
        .find(|a| a.up && a.ip.is_some())
        .and_then(|a| a.ip)
}

fn enumerate_adapters() -> Vec<Adapter> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS;
    let mut size: u32 = 16 * 1024;
    let mut buffer: Vec<u8> = vec![0; size as usize];

    let mut result = 0u32;
    for _ in 0..4 {
        result = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            )
        };
        if result != ERROR_BUFFER_OVERFLOW {
            break;
        }
        buffer = vec![0; size as usize];
    }
    if result != 0 {
        return Vec::new();
    }

    let mut adapters = Vec::new();
    let mut node = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        while !node.is_null() {
            let raw = &*node;
            let name = wide_to_string(raw.FriendlyName);
            let description = wide_to_string(raw.Description);
            let ip = first_ipv4(raw.FirstUnicastAddress);
            let has_gateway = !raw.FirstGatewayAddress.is_null();
            let up = raw.OperStatus == IfOperStatusUp;
            let vpn = raw.IfType == IF_TYPE_TUNNEL || looks_like_vpn(&name, &description);
            if raw.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                adapters.push(Adapter {
                    name,
                    ip,
                    has_gateway,
                    vpn,
                    up,
                    metric: raw.Ipv4Metric,
                });
            }
            node = raw.Next;
        }
    }
    adapters
}

unsafe fn first_ipv4(
    mut address: *mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_UNICAST_ADDRESS_LH,
) -> Option<Ipv4Addr> {
    while !address.is_null() {
        let entry = &*address;
        if let Some(ip) = socket_ipv4(&entry.Address) {
            if !ip.is_link_local() && !ip.is_loopback() {
                return Some(ip);
            }
        }
        address = entry.Next;
    }
    None
}

unsafe fn socket_ipv4(
    address: &windows::Win32::Networking::WinSock::SOCKET_ADDRESS,
) -> Option<Ipv4Addr> {
    let sockaddr = address.lpSockaddr;
    if sockaddr.is_null() {
        return None;
    }
    let family = (*sockaddr).sa_family;
    if family != AF_INET {
        return None;
    }
    let sin = &*(sockaddr as *const SOCKADDR as *const SOCKADDR_IN);
    Some(Ipv4Addr::from(sin.sin_addr.S_un.S_addr.to_ne_bytes()))
}

fn wide_to_string(value: windows::core::PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    unsafe { value.to_string().unwrap_or_default() }
}

fn looks_like_vpn(name: &str, description: &str) -> bool {
    const MARKERS: &[&str] = &[
        "vpn", "wireguard", "openvpn", "tap-windows", "tap adapter", "tun", "tailscale",
        "zerotier", "hamachi", "radmin", "warp", "cloudflare", "nord", "proton", "expressvpn",
        "surfshark", "amnezia", "outline", "psiphon", "shadowsocks", "pptp", "l2tp", "sstp",
        "ikev2", "softether", "wireguard tunnel",
    ];
    let haystack = format!("{name} {description}").to_lowercase();
    MARKERS.iter().any(|marker| haystack.contains(marker))
}
