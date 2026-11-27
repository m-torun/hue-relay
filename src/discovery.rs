//! Being found like a bridge: mDNS (_hue._tcp) and SSDP.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use socket2::{Domain, Protocol, Socket, Type};
use tracing::info;

use crate::relay::Relay;

/// Advertise `_hue._tcp`; keep the returned daemon alive.
pub fn mdns(relay: &Relay) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new()?;
    let tail = &relay.id.machex[6..];
    let instance = format!("Philips Hue - {}", tail.to_uppercase());
    let host = format!("hue-relay-{tail}.local.");
    let props = [
        ("bridgeid", relay.id.bridge_id.to_lowercase()),
        ("modelid", "BSB002".to_string()),
    ];
    let info = ServiceInfo::new(
        "_hue._tcp.local.",
        &instance,
        &host,
        relay.cfg.relay_ip.to_string(),
        443,
        &props[..],
    )?;
    daemon.register(info)?;
    info!("mdns: advertised _hue._tcp as {instance}");
    Ok(daemon)
}

/// Answer SSDP searches for root devices, basic devices, our UUID and ssdp:all.
pub fn ssdp(relay: Arc<Relay>) -> Result<()> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 1900).into())?;
    sock.join_multicast_v4(&Ipv4Addr::new(239, 255, 255, 250), &relay.cfg.relay_ip)?;
    let sock: UdpSocket = sock.into();
    let uuid = format!("uuid:{}", relay.id.upnp_uuid);
    info!("ssdp: listening");
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf) else {
                continue;
            };
            let msg = String::from_utf8_lossy(&buf[..n]);
            if !msg.starts_with("M-SEARCH") || peer.ip() == relay.cfg.relay_ip {
                continue;
            }
            let st = msg
                .split("\r\n")
                .find_map(|l| {
                    l.get(..3)
                        .filter(|h| h.eq_ignore_ascii_case("st:"))
                        .map(|_| l[3..].trim())
                })
                .unwrap_or("");
            let wanted: Vec<String> = match st {
                "ssdp:all" => vec![
                    "upnp:rootdevice".into(),
                    uuid.clone(),
                    "urn:schemas-upnp-org:device:basic:1".into(),
                ],
                "upnp:rootdevice" | "urn:schemas-upnp-org:device:basic:1" => vec![st.to_string()],
                s if s == uuid => vec![uuid.clone()],
                _ => vec![],
            };
            info!(
                "ssdp: M-SEARCH from {} ST={st}{}",
                peer.ip(),
                if wanted.is_empty() { "" } else { " (answered)" }
            );
            let api = relay.real_public.read().unwrap()["apiversion"]
                .as_str()
                .unwrap_or("1.78.0")
                .to_string();
            for w in wanted {
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nHOST: 239.255.255.250:1900\r\nEXT:\r\nCACHE-CONTROL: max-age=100\r\n\
                     LOCATION: http://{}:80/description.xml\r\nSERVER: Hue/1.0 UPnP/1.0 IpBridge/{api}\r\n\
                     hue-bridgeid: {}\r\nST: {w}\r\nUSN: uuid:{}::{w}\r\n\r\n",
                    relay.cfg.relay_ip, relay.id.bridge_id, relay.id.upnp_uuid
                );
                let _ = sock.send_to(reply.as_bytes(), peer);
            }
        }
    });
    Ok(())
}
