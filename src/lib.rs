//! Fast route tracing library.
//!
//! [`librtraceroute`]: https://github.com/toorajtaraz/librtraceroute
extern crate pnet;
extern crate ansi_term;

use pnet::datalink;
use pnet::packet::Packet;
use pnet::packet::icmp::IcmpTypes;
use pnet::packet::icmp::echo_request;
use pnet::packet::icmpv6::{Icmpv6Types, MutableIcmpv6Packet};
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::{icmp, icmpv6, ipv4, ipv6, udp};
use pnet::transport::TransportChannelType::{Layer3, Layer4};
use pnet::transport::TransportProtocol::Ipv4;
use pnet::transport::TransportSender;
use pnet::transport::transport_channel;
use pnet::transport::{icmp_packet_iter, icmpv6_packet_iter};
use pnet::util;
use pnet_macros_support::types::*;
use rand::random;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// This enum represents supported protocols for route tracing.
#[derive(Copy, Clone)]
pub enum TraceRouteProtocol {
    Icmp,
    Udp,
}

/// This struct stores all needed data for representing a hop.
pub struct HopFound {
    pub addr: Option<IpAddr>,
    pub tries: u16,
    pub hop_count: u8,
    pub is_last: bool,
    pub time: Option<Duration>,
}

/// This type is a Result consisting of TraceRoute struct and receiver handle.
pub type TraceRouteRes = Result<(TraceRoute, Receiver<HopFound>), String>;

/// This struct stores all needed data for performing route tracing task.
pub struct TraceRoute {
    pub max_ttl: u8,
    pub max_tries: u16,
    pub begin_ttl: u8,
    pub address: IpAddr,
    pub port: u16,
    pub timeout: u64,
    pub size: usize,
    pub results_sender: Sender<HopFound>,
    pub protocol: TraceRouteProtocol,
}

/// This block implements TraceRoute struct.
impl TraceRoute {
    /// Creates new TraceRoute and returns TraceRouteRes.
    pub fn new(
        max_ttl: Option<u8>,
        begin_ttl: Option<u8>,
        max_tries: Option<u16>,
        timeout: Option<u64>,
        port: Option<u16>,
        size: Option<usize>,
        addr: IpAddr,
        protocol: Option<TraceRouteProtocol>,
    ) -> TraceRouteRes {
        let (send_handle, recieve_handle) = channel();

        let mut trace_route = TraceRoute {
            max_ttl: 30,
            begin_ttl: 1,
            max_tries: 4,
            port: 33434,
            timeout: 200,
            address: addr,
            size: 64,
            results_sender: send_handle,
            protocol: TraceRouteProtocol::Udp,
        };

        if let Some(mt) = max_ttl {
            if mt < 1 {
               return Err(String::from("BAD MAX TTL")); 
            }
            trace_route.max_ttl = mt;
        }

        if let Some(bt) = begin_ttl {
            if bt > trace_route.max_ttl {
               return Err(String::from("BAD START TTL")); 
            }
            trace_route.begin_ttl = bt;
        }

        if let Some(mt) = max_tries {
            trace_route.max_tries = mt;
        }

        if let Some(p) = port {
            trace_route.port = p;
        }

        if let Some(s) = size {
            if s < 12 {
               return Err(String::from("BAD SIZE - MIN=12")); 
            }
            trace_route.size = s;
        }

        if let Some(to) = timeout {
            if to == 0 {
               return Err(String::from("BAD TIMEOUT")); 
            }
            trace_route.timeout = to;
        }

        if let Some(p) = protocol {
            trace_route.protocol = p;
        }

        Ok((trace_route, recieve_handle))
    }

    /// This function executes route tracing.
    pub fn run_trace_route(&self) {
        if self.address.is_ipv4() {
            start_trace_route_on_v4(
                self.results_sender.clone(),
                self.begin_ttl,
                self.max_ttl,
                self.max_tries,
                self.protocol,
                self.port,
                self.address,
                self.timeout,
                self.size,
            );
        } else {
            start_trace_route_on_v6(
                self.results_sender.clone(),
                self.begin_ttl,
                self.max_ttl,
                self.max_tries,
                self.protocol,
                self.port,
                self.address,
                self.timeout,
                self.size,
            );
        }
    }
}

fn build_udp_send_v4(
    tx: &mut TransportSender,
    addr: IpAddr,
    size: usize,
    port: u16,
    ttl: u8,
    my_ip: Ipv4Addr,
) -> Result<usize, std::io::Error> {
    let mut vec: Vec<u8> = vec![0; size];
    let mut udp_packet = udp::MutableUdpPacket::new(&mut vec[..]).unwrap();
    udp_packet.set_source(random::<u16>());
    udp_packet.set_destination(port);
    udp_packet.set_length(size as u16);
    udp_packet.set_payload(&mut vec![0; size - 8]);
    let csum = udp::ipv4_checksum(
        &udp_packet.to_immutable(),
        &get_ip_addr(true)
            .unwrap()
            .to_string()
            .parse::<Ipv4Addr>()
            .unwrap(),
        &addr.to_string().parse::<Ipv4Addr>().unwrap(),
    );
    udp_packet.set_checksum(csum);

    let mut ipv4_vec: Vec<u8> = vec![0; ipv4::MutableIpv4Packet::minimum_packet_size() + vec.len()];
    let mut ipv4_packet = ipv4::MutableIpv4Packet::new(&mut ipv4_vec[..]).unwrap();
    ipv4_packet.set_header_length(5);
    ipv4_packet.set_fragment_offset(16384);
    ipv4_packet.set_identification(rand::random::<u16>());
    ipv4_packet.set_version(4);
    ipv4_packet.set_ttl(ttl);
    ipv4_packet.set_next_level_protocol(IpNextHeaderProtocols::Udp);
    let ip = addr.to_string().parse::<Ipv4Addr>().unwrap();
    ipv4_packet.set_source(my_ip);
    ipv4_packet.set_destination(ip);
    ipv4_packet
        .set_total_length((ipv4::MutableIpv4Packet::minimum_packet_size() + vec.len()) as u16);
    ipv4_packet.set_payload(&mut vec[..]);

    let csum = ipv4::checksum(&ipv4_packet.to_immutable());
    ipv4_packet.set_checksum(csum);
    tx.send_to(ipv4_packet, addr)
}

fn build_udp_send_v6(
    tx: &mut TransportSender,
    addr: IpAddr,
    size: usize,
    port: u16,
    ttl: u8,
    my_ip: Ipv6Addr,
) -> Result<usize, std::io::Error> {
    let mut vec: Vec<u8> = vec![0; size];
    let mut udp_packet = udp::MutableUdpPacket::new(&mut vec[..]).unwrap();
    udp_packet.set_source(random::<u16>());
    udp_packet.set_destination(port);
    udp_packet.set_length(size as u16);
    udp_packet.set_payload(&mut vec![0; size - 8]);
    let csum = udp::ipv4_checksum(
        &udp_packet.to_immutable(),
        &get_ip_addr(true)
            .unwrap()
            .to_string()
            .parse::<Ipv4Addr>()
            .unwrap(),
        &addr.to_string().parse::<Ipv4Addr>().unwrap(),
    );
    udp_packet.set_checksum(csum);

    let mut ipv6_vec: Vec<u8> = vec![0; ipv6::MutableIpv6Packet::minimum_packet_size() + vec.len()];
    let mut ipv6_packet = ipv6::MutableIpv6Packet::new(&mut ipv6_vec[..]).unwrap();
    ipv6_packet.set_version(6);
    ipv6_packet.set_hop_limit(ttl);
    ipv6_packet.set_next_header(IpNextHeaderProtocols::Udp);
    let ip = addr.to_string().parse::<Ipv6Addr>().unwrap();
    ipv6_packet.set_source(my_ip);
    ipv6_packet.set_destination(ip);
    ipv6_packet.set_payload_length((vec.len()) as u16);
    ipv6_packet.set_payload(&mut vec[..]);

    tx.send_to(ipv6_packet, addr)
}

fn build_icmp_send_v4(
    tx: &mut TransportSender,
    addr: IpAddr,
    size: usize,
    ttl: u8,
    my_ip: Ipv4Addr,
) -> Result<usize, std::io::Error> {
    let mut vec: Vec<u8> = vec![0; size];
    let mut echo_packet = echo_request::MutableEchoRequestPacket::new(&mut vec[..]).unwrap();
    echo_packet.set_sequence_number(random::<u16>());
    echo_packet.set_identifier(random::<u16>());
    echo_packet.set_icmp_type(IcmpTypes::EchoRequest);

    let csum = icmp_checksum(&echo_packet);
    echo_packet.set_checksum(csum);

    let mut ipv4_vec: Vec<u8> = vec![0; ipv4::MutableIpv4Packet::minimum_packet_size() + vec.len()];
    let mut ipv4_packet = ipv4::MutableIpv4Packet::new(&mut ipv4_vec[..]).unwrap();
    ipv4_packet.set_header_length(5);
    ipv4_packet.set_fragment_offset(16384);
    ipv4_packet.set_identification(rand::random::<u16>());
    ipv4_packet.set_version(4);
    ipv4_packet.set_ttl(ttl);
    ipv4_packet.set_next_level_protocol(IpNextHeaderProtocols::Icmp);
    let ip = addr.to_string().parse::<Ipv4Addr>().unwrap();
    ipv4_packet.set_source(my_ip);
    ipv4_packet.set_destination(ip);
    ipv4_packet
        .set_total_length((ipv4::MutableIpv4Packet::minimum_packet_size() + vec.len()) as u16);
    ipv4_packet.set_payload(&mut vec[..]);

    let csum = ipv4::checksum(&ipv4_packet.to_immutable());
    ipv4_packet.set_checksum(csum);

    tx.send_to(ipv4_packet, addr)
}

fn build_icmp_send_v6(
    tx: &mut TransportSender,
    addr: IpAddr,
    size: usize,
    ttl: u8,
    my_ip: Ipv6Addr,
) -> Result<usize, std::io::Error> {
    let mut vec: Vec<u8> = vec![0; size];

    let mut echo_packet = MutableIcmpv6Packet::new(&mut vec[..]).unwrap();
    echo_packet.set_icmpv6_type(Icmpv6Types::EchoRequest);

    let csum = icmpv6_checksum(&echo_packet);
    echo_packet.set_checksum(csum);

    let mut ipv6_vec: Vec<u8> = vec![0; ipv6::MutableIpv6Packet::minimum_packet_size() + vec.len()];
    let mut ipv6_packet = ipv6::MutableIpv6Packet::new(&mut ipv6_vec[..]).unwrap();
    ipv6_packet.set_version(6);
    ipv6_packet.set_hop_limit(ttl);
    ipv6_packet.set_next_header(IpNextHeaderProtocols::Icmpv6);
    let ip = addr.to_string().parse::<Ipv6Addr>().unwrap();
    ipv6_packet.set_source(my_ip);
    ipv6_packet.set_destination(ip);
    ipv6_packet.set_payload_length((vec.len()) as u16);
    ipv6_packet.set_payload(&mut vec[..]);

    tx.send_to(ipv6_packet, addr)
}

fn get_ip_addr(v4: bool) -> Option<IpAddr> {
    for iface in datalink::interfaces() {
        if !iface.is_loopback() && iface.is_up() {
            for ip in iface.ips {
                if ip.ip().is_ipv4() && v4 {
                    return Some(ip.ip());
                }
                if ip.ip().is_ipv6() && !v4 {
                    return Some(ip.ip());
                }
            }
        }
    }
    None
}

fn start_trace_route_on_v4(
    tx: Sender<HopFound>,
    begin_ttl: u8,
    end_ttl: u8,
    max_tries: u16,
    trace_route_protocol: TraceRouteProtocol,
    port: u16,
    ip: IpAddr,
    timeout: u64,
    packet_size: usize,
) {
    let self_ip = match get_ip_addr(true) {
        Some(ip) => ip.to_string().parse::<Ipv4Addr>().unwrap(),
        None => {
            panic!("No <UP> interface was found, please connect to internet.");
        }
    };
    let mut seen: BTreeSet<IpAddr> = BTreeSet::new();
    let protocol = Layer4(Ipv4(IpNextHeaderProtocols::Icmp));
    let (_, transport_rx) = match transport_channel(4096, protocol) {
        Ok((tx, rx)) => (tx, rx),
        Err(_) => return,
    };
    thread::spawn(move || {
        let ipv4_protocol = match trace_route_protocol {
            TraceRouteProtocol::Udp => Layer3(IpNextHeaderProtocols::Udp),
            TraceRouteProtocol::Icmp => Layer3(IpNextHeaderProtocols::Icmp),
        };
        let (mut ipv4_tx, _) = match transport_channel(4096, ipv4_protocol) {
            Ok((tx, rx)) => (tx, rx),
            Err(_) => return,
        };

        let mut receiver = transport_rx;
        let mut iter = icmp_packet_iter(&mut receiver);
        let mut i: u8 = begin_ttl;
        let mut tries: u16 = 0;
        let mut has_changed = false;
        let mut timer;
        loop {
            if i > end_ttl {
                tx.send(HopFound {
                    addr: None,
                    hop_count: i,
                    tries,
                    is_last: true,
                    time: None,
                })
                .unwrap();
                break;
            }
            match trace_route_protocol {
                TraceRouteProtocol::Udp => {
                    match build_udp_send_v4(
                        &mut ipv4_tx,
                        ip,
                        packet_size,
                        port + i as u16,
                        i,
                        self_ip,
                    ) {
                        Ok(_) => timer = Instant::now(),
                        Err(e) => {
                            panic!("Could not send packet, make sure this program has needed privilages, Error<{}>", e.to_string());
                        }
                    }
                }
                TraceRouteProtocol::Icmp => {
                    match build_icmp_send_v4(&mut ipv4_tx, ip, 64, i, self_ip) {
                        Ok(_) => timer = Instant::now(),
                        Err(e) => {
                            panic!("Could not send packet, make sure this program has needed privilages, Error<{}>", e.to_string());
                        }
                    }
                }
            };
            match iter.next_with_timeout(Duration::from_millis(timeout)) {
                Ok(p) => match p {
                    Some((packet, addr)) => match seen.get(&addr) {
                        None => {
                            seen.insert(addr);
                            if packet.get_icmp_type() == icmp::IcmpType::new(11) {
                                tx.send(HopFound {
                                    addr: Some(addr),
                                    hop_count: i,
                                    tries,
                                    is_last: false,
                                    time: Some(Instant::now() - timer),
                                })
                                .unwrap();
                                has_changed = true;
                                i += 1;
                                tries = 0;
                            } else {
                                match trace_route_protocol {
                                    TraceRouteProtocol::Udp => {
                                        if packet.get_icmp_type() == icmp::IcmpType::new(3) {
                                            tx.send(HopFound {
                                                addr: Some(addr),
                                                hop_count: i,
                                                tries,
                                                is_last: true,
                                                time: Some(Instant::now() - timer),
                                            })
                                            .unwrap();
                                            break;
                                        } else {
                                            println!(
                                                "UNEXPECTED ICMP PACKET WITH <{:?}>",
                                                packet.get_icmp_type()
                                            );
                                        }
                                    }
                                    TraceRouteProtocol::Icmp => {
                                        if packet.get_icmp_type() == icmp::IcmpType::new(0) {
                                            tx.send(HopFound {
                                                addr: Some(addr),
                                                hop_count: i,
                                                tries,
                                                is_last: true,
                                                time: Some(Instant::now() - timer),
                                            })
                                            .unwrap();
                                            break;
                                        } else {
                                            println!(
                                                "UNEXPECTED ICMP PACKET WITH <{:?}>",
                                                packet.get_icmp_type()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            if tries > 0 {
                                tries -= 1;
                            }
                        }
                    },
                    _ => has_changed = false,
                },
                _ => has_changed = false,
            }
            tries += 1;
            if tries >= max_tries && !has_changed {
                tx.send(HopFound {
                    addr: None,
                    hop_count: i,
                    tries,
                    is_last: false,
                    time: None,
                })
                .unwrap();
                tries = 0;
                i += 1;
                has_changed = false;
            }
        }
    });
}

fn start_trace_route_on_v6(
    tx: Sender<HopFound>,
    begin_ttl: u8,
    end_ttl: u8,
    max_tries: u16,
    trace_route_protocol: TraceRouteProtocol,
    port: u16,
    ip: IpAddr,
    timeout: u64,
    packet_size: usize,
) {
    let self_ip = match get_ip_addr(false) {
        Some(ip) => ip.to_string().parse::<Ipv6Addr>().unwrap(),
        None => {
            panic!("No <UP> interface was found, please connect to internet.");
        }
    };
    let mut seen: BTreeSet<IpAddr> = BTreeSet::new();
    let protocol = Layer4(Ipv4(IpNextHeaderProtocols::Icmpv6));
    let (_, transport_rx) = match transport_channel(4096, protocol) {
        Ok((tx, rx)) => (tx, rx),
        Err(_) => return,
    };
    thread::spawn(move || {
        let ipv6_protocol = match trace_route_protocol {
            TraceRouteProtocol::Udp => Layer3(IpNextHeaderProtocols::Udp),
            TraceRouteProtocol::Icmp => Layer3(IpNextHeaderProtocols::Icmpv6),
        };
        let (mut ipv6_tx, _) = match transport_channel(4096, ipv6_protocol) {
            Ok((tx, rx)) => (tx, rx),
            Err(_) => return,
        };

        let mut receiver = transport_rx;
        let mut iter = icmpv6_packet_iter(&mut receiver);
        let mut i: u8 = begin_ttl;
        let mut tries: u16 = 0;
        let mut has_changed = false;
        let mut timer;
        loop {
            if i > end_ttl {
                tx.send(HopFound {
                    addr: None,
                    hop_count: i,
                    tries,
                    is_last: true,
                    time: None,
                })
                .unwrap();
                break;
            }
            match trace_route_protocol {
                TraceRouteProtocol::Udp => {
                    match build_udp_send_v6(
                        &mut ipv6_tx,
                        ip,
                        packet_size,
                        port + i as u16,
                        i,
                        self_ip,
                    ) {
                        Ok(_) => timer = Instant::now(),
                        Err(e) => {
                            panic!("Could not send packet, make sure this program has needed privilages, Error<{}>", e.to_string());
                        }
                    }
                }
                TraceRouteProtocol::Icmp => {
                    match build_icmp_send_v6(&mut ipv6_tx, ip, 64, i, self_ip) {
                        Ok(_) => timer = Instant::now(),
                        Err(e) => {
                            panic!("Could not send packet, make sure this program has needed privilages, Error<{}>", e.to_string());
                        }
                    }
                }
            };
            match iter.next_with_timeout(Duration::from_millis(timeout)) {
                Ok(p) => match p {
                    Some((packet, addr)) => match seen.get(&addr) {
                        None => {
                            seen.insert(addr);
                            if packet.get_icmpv6_type() == icmpv6::Icmpv6Type::new(0) && addr != ip
                            {
                                tx.send(HopFound {
                                    addr: Some(addr),
                                    hop_count: i,
                                    tries,
                                    is_last: false,
                                    time: Some(Instant::now() - timer),
                                })
                                .unwrap();
                                has_changed = true;
                                i += 1;
                                tries = 0;
                            } else {
                                match trace_route_protocol {
                                    TraceRouteProtocol::Udp => {
                                        if packet.get_icmpv6_type() == icmpv6::Icmpv6Type::new(4) {
                                            tx.send(HopFound {
                                                addr: Some(addr),
                                                hop_count: i,
                                                tries,
                                                is_last: true,
                                                time: Some(Instant::now() - timer),
                                            })
                                            .unwrap();
                                            break;
                                        } else {
                                            println!(
                                                "UNEXPECTED ICMP PACKET WITH <{:?}>",
                                                packet.get_icmpv6_type()
                                            );
                                        }
                                    }
                                    TraceRouteProtocol::Icmp => {
                                        if packet.get_icmpv6_type() == icmpv6::Icmpv6Type::new(0) {
                                            tx.send(HopFound {
                                                addr: Some(addr),
                                                hop_count: i,
                                                tries,
                                                is_last: true,
                                                time: Some(Instant::now() - timer),
                                            })
                                            .unwrap();
                                            break;
                                        } else {
                                            println!(
                                                "UNEXPECTED ICMP PACKET WITH <{:?}>",
                                                packet.get_icmpv6_type()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            tries -= 1;
                        }
                    },
                    _ => has_changed = false,
                },
                _ => has_changed = false,
            }
            tries += 1;
            if tries >= max_tries && !has_changed {
                tx.send(HopFound {
                    addr: None,
                    hop_count: i,
                    tries,
                    is_last: false,
                    time: None,
                })
                .unwrap();
                tries = 0;
                i += 1;
                has_changed = false;
            }
        }
    });
}

fn icmp_checksum(packet: &echo_request::MutableEchoRequestPacket) -> u16be {
    util::checksum(packet.packet(), 1)
}

fn icmpv6_checksum(packet: &MutableIcmpv6Packet) -> u16be {
    util::checksum(packet.packet(), 1)
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creating_new_tracer() {
        let (_, _) = TraceRoute::new(
            Some(128), Some(12), None, None, None, None, IpAddr::from([127, 0, 0, 1]), None,
        )
        .unwrap();
    }
    #[test]
    #[should_panic]
    fn creating_bad_tracer() {
        let (_, _) = TraceRoute::new(
            None, Some(128), None, None, None, None, IpAddr::from([127, 0, 0, 1]), None,
        )
        .unwrap();
    }
}
