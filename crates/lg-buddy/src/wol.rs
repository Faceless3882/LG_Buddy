use std::error::Error;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::ptr;

use crate::config::MacAddress;

pub const MAGIC_PACKET_LEN: usize = 102;
pub const DEFAULT_WOL_PORT: u16 = 9;

#[derive(Debug)]
pub enum WakeOnLanError {
    Io {
        action: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for WakeOnLanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, source } => write!(f, "failed to {action}: {source}"),
        }
    }
}

impl Error for WakeOnLanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

pub trait WakeOnLanSender {
    fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError>;

    fn send_magic_packet_to(
        &self,
        mac: &MacAddress,
        _target_ip: Ipv4Addr,
    ) -> Result<(), WakeOnLanError> {
        self.send_magic_packet(mac)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpWakeOnLanSender {
    bind_addr: SocketAddrV4,
    target_addr: SocketAddrV4,
}

impl Default for UdpWakeOnLanSender {
    fn default() -> Self {
        Self::broadcast(DEFAULT_WOL_PORT)
    }
}

impl UdpWakeOnLanSender {
    pub fn new(bind_addr: SocketAddrV4, target_addr: SocketAddrV4) -> Self {
        Self {
            bind_addr,
            target_addr,
        }
    }

    pub fn broadcast(port: u16) -> Self {
        Self::new(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, port),
        )
    }

    pub fn bind_addr(&self) -> SocketAddrV4 {
        self.bind_addr
    }

    pub fn target_addr(&self) -> SocketAddrV4 {
        self.target_addr
    }
}

impl WakeOnLanSender for UdpWakeOnLanSender {
    fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError> {
        self.send_magic_packet_to_targets(mac, &[self.target_addr])
    }

    fn send_magic_packet_to(
        &self,
        mac: &MacAddress,
        target_ip: Ipv4Addr,
    ) -> Result<(), WakeOnLanError> {
        let targets = self.target_addresses_for(target_ip);
        self.send_magic_packet_to_targets(mac, &targets)
    }
}

impl UdpWakeOnLanSender {
    fn send_magic_packet_to_targets(
        &self,
        mac: &MacAddress,
        targets: &[SocketAddrV4],
    ) -> Result<(), WakeOnLanError> {
        let socket = UdpSocket::bind(self.bind_addr).map_err(|source| WakeOnLanError::Io {
            action: "bind UDP socket",
            source,
        })?;
        socket
            .set_broadcast(true)
            .map_err(|source| WakeOnLanError::Io {
                action: "enable UDP broadcast",
                source,
            })?;

        let packet = build_magic_packet(mac);

        let mut sent = false;
        let mut last_error = None;

        for target in targets {
            match socket.send_to(&packet, target) {
                Ok(_) => sent = true,
                Err(source) => {
                    last_error = Some(WakeOnLanError::Io {
                        action: "send Wake-on-LAN packet",
                        source,
                    });
                }
            }
        }

        if sent {
            Ok(())
        } else {
            Err(last_error.unwrap_or_else(|| WakeOnLanError::Io {
                action: "send Wake-on-LAN packet",
                source: io::Error::new(io::ErrorKind::InvalidInput, "no Wake-on-LAN targets"),
            }))
        }
    }

    fn target_addresses_for(&self, target_ip: Ipv4Addr) -> Vec<SocketAddrV4> {
        let mut targets = Vec::new();
        let port = self.target_addr.port();

        if let Ok(Some(broadcast)) = routed_subnet_broadcast(target_ip, self.bind_addr, port) {
            push_unique_target(&mut targets, SocketAddrV4::new(broadcast, port));
        }

        push_unique_target(&mut targets, SocketAddrV4::new(target_ip, port));
        push_unique_target(&mut targets, self.target_addr);
        targets
    }
}

pub fn build_magic_packet(mac: &MacAddress) -> [u8; MAGIC_PACKET_LEN] {
    let mut packet = [0_u8; MAGIC_PACKET_LEN];
    packet[..6].fill(0xFF);

    let mac_octets = mac.octets();
    for chunk in packet[6..].chunks_exact_mut(mac_octets.len()) {
        chunk.copy_from_slice(&mac_octets);
    }

    packet
}

fn push_unique_target(targets: &mut Vec<SocketAddrV4>, target: SocketAddrV4) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

fn routed_subnet_broadcast(
    target_ip: Ipv4Addr,
    bind_addr: SocketAddrV4,
    port: u16,
) -> io::Result<Option<Ipv4Addr>> {
    let source = routed_source_addr(target_ip, bind_addr, port)?;
    interface_broadcast_for_source(source)
}

fn routed_source_addr(
    target_ip: Ipv4Addr,
    bind_addr: SocketAddrV4,
    port: u16,
) -> io::Result<Ipv4Addr> {
    let socket = UdpSocket::bind(bind_addr)?;
    socket.connect(SocketAddrV4::new(target_ip, port))?;
    match socket.local_addr()? {
        std::net::SocketAddr::V4(addr) => Ok(*addr.ip()),
        std::net::SocketAddr::V6(_) => Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "route to IPv4 target used an IPv6 source address",
        )),
    }
}

#[cfg(unix)]
fn interface_broadcast_for_source(source: Ipv4Addr) -> io::Result<Option<Ipv4Addr>> {
    let mut addrs = ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut addrs) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = InterfaceAddrs(addrs);

    let mut current = addrs;
    while !current.is_null() {
        let iface = unsafe { &*current };
        if sockaddr_ipv4(iface.ifa_addr) == Some(source) {
            if let Some(mask) = sockaddr_ipv4(iface.ifa_netmask) {
                return Ok(subnet_broadcast(source, mask));
            }

            if iface.ifa_flags & (libc::IFF_BROADCAST as libc::c_uint) != 0 {
                return Ok(sockaddr_ipv4(iface.ifa_ifu));
            }
        }

        current = iface.ifa_next;
    }

    Ok(None)
}

#[cfg(not(unix))]
fn interface_broadcast_for_source(_source: Ipv4Addr) -> io::Result<Option<Ipv4Addr>> {
    Ok(None)
}

#[cfg(unix)]
struct InterfaceAddrs(*mut libc::ifaddrs);

#[cfg(unix)]
impl Drop for InterfaceAddrs {
    fn drop(&mut self) {
        unsafe { libc::freeifaddrs(self.0) };
    }
}

#[cfg(unix)]
fn sockaddr_ipv4(addr: *const libc::sockaddr) -> Option<Ipv4Addr> {
    if addr.is_null() || unsafe { (*addr).sa_family } != libc::AF_INET as libc::sa_family_t {
        return None;
    }

    let addr = unsafe { &*(addr.cast::<libc::sockaddr_in>()) };
    Some(ipv4_from_network_order_s_addr(addr.sin_addr.s_addr))
}

#[cfg(unix)]
fn ipv4_from_network_order_s_addr(s_addr: libc::in_addr_t) -> Ipv4Addr {
    Ipv4Addr::from(s_addr.to_ne_bytes())
}

fn subnet_broadcast(addr: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let addr = u32::from_be_bytes(addr.octets());
    let netmask = u32::from_be_bytes(netmask.octets());
    if netmask == u32::MAX {
        return None;
    }

    Some(Ipv4Addr::from((addr | !netmask).to_be_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{
        build_magic_packet, subnet_broadcast, UdpWakeOnLanSender, WakeOnLanSender,
        DEFAULT_WOL_PORT, MAGIC_PACKET_LEN,
    };
    use crate::config::MacAddress;
    use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn default_sender_uses_broadcast_port_9() {
        let sender = UdpWakeOnLanSender::default();

        assert_eq!(
            sender.bind_addr(),
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)
        );
        assert_eq!(
            sender.target_addr(),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, DEFAULT_WOL_PORT)
        );
    }

    #[test]
    fn magic_packet_has_expected_layout() {
        let mac = parse_mac("aa:bb:cc:dd:ee:ff");
        let packet = build_magic_packet(&mac);

        assert_eq!(packet.len(), MAGIC_PACKET_LEN);
        assert_eq!(&packet[..6], &[0xFF; 6]);

        for chunk in packet[6..].as_chunks::<6>().0 {
            assert_eq!(chunk, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        }
    }

    #[test]
    fn subnet_broadcast_uses_ipv4_address_and_netmask() {
        assert_eq!(
            subnet_broadcast(
                "10.0.0.33".parse().expect("addr"),
                "255.255.255.0".parse().expect("mask"),
            ),
            Some("10.0.0.255".parse().expect("broadcast"))
        );
        assert_eq!(
            subnet_broadcast(
                "192.168.10.70".parse().expect("addr"),
                "255.255.252.0".parse().expect("mask"),
            ),
            Some("192.168.11.255".parse().expect("broadcast"))
        );
        assert_eq!(
            subnet_broadcast(
                "10.0.0.33".parse().expect("addr"),
                "255.255.255.255".parse().expect("mask"),
            ),
            None
        );
    }

    #[test]
    fn targeted_udp_sender_can_deliver_to_tv_ip_destination() {
        let listener =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set listener timeout");
        let target = match listener.local_addr().expect("listener addr") {
            std::net::SocketAddr::V4(addr) => addr,
            std::net::SocketAddr::V6(_) => panic!("expected IPv4 listener address"),
        };

        let receiver = thread::spawn(move || {
            let mut buf = [0_u8; MAGIC_PACKET_LEN];
            let (size, _) = listener.recv_from(&mut buf).expect("receive magic packet");
            (size, buf)
        });

        let sender = UdpWakeOnLanSender::new(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, target.port()),
        );
        let mac = parse_mac("01:23:45:67:89:ab");
        sender
            .send_magic_packet_to(&mac, Ipv4Addr::LOCALHOST)
            .expect("send targeted magic packet over udp");

        let (size, received) = receiver.join().expect("join receiver thread");
        assert_eq!(size, MAGIC_PACKET_LEN);
        assert_eq!(received, build_magic_packet(&mac));
    }

    #[cfg(unix)]
    #[test]
    fn network_order_sockaddr_address_preserves_octets() {
        let s_addr = u32::from_ne_bytes([192, 168, 1, 20]);

        assert_eq!(
            super::ipv4_from_network_order_s_addr(s_addr),
            Ipv4Addr::new(192, 168, 1, 20)
        );
    }

    #[test]
    fn udp_sender_delivers_magic_packet() {
        let listener =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set listener timeout");
        let target = match listener.local_addr().expect("listener addr") {
            std::net::SocketAddr::V4(addr) => addr,
            std::net::SocketAddr::V6(_) => panic!("expected IPv4 listener address"),
        };

        let receiver = thread::spawn(move || {
            let mut buf = [0_u8; MAGIC_PACKET_LEN];
            let (size, _) = listener.recv_from(&mut buf).expect("receive magic packet");
            (size, buf)
        });

        let sender = UdpWakeOnLanSender::new(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), target);
        let mac = parse_mac("01:23:45:67:89:ab");
        sender
            .send_magic_packet(&mac)
            .expect("send magic packet over udp");

        let (size, received) = receiver.join().expect("join receiver thread");
        assert_eq!(size, MAGIC_PACKET_LEN);
        assert_eq!(received, build_magic_packet(&mac));
    }

    fn parse_mac(value: &str) -> MacAddress {
        value.parse().expect("parse mac address")
    }
}
