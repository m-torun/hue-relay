//! The relay's configuration file and the identity derived from it.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::stream::Gains;

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    pub relay_ip: Ipv4Addr,
    pub relay_mac: String,
    #[serde(default = "default_name")]
    pub name: String,
    pub real_bridge: String,
    pub key_file: PathBuf,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub link_open: bool,
    #[serde(default)]
    pub gains: Gains,
    /// Areas whose lights keep their per-channel levels between syncs too: the relay then streams
    /// the lights' own state (on, brightness, colour, gradient) itself.
    #[serde(default)]
    pub outside_sync: Vec<String>,
}

fn default_name() -> String {
    "Hue Relay".into()
}

fn default_data_dir() -> PathBuf {
    "/data".into()
}

impl Config {
    pub fn gains_file(&self) -> PathBuf {
        self.data_dir.join("gains.json")
    }
    pub fn clients_file(&self) -> PathBuf {
        self.data_dir.join("clients.json")
    }
}

/// How the relay presents itself: MAC, bridge id (MAC with fffe in the middle, as real bridges),
/// UPnP UUID.
#[derive(Clone, Debug)]
pub struct Identity {
    pub mac: String,
    pub machex: String,
    pub bridge_id: String,
    pub upnp_uuid: String,
}

impl Identity {
    pub fn from_mac(mac: &str) -> Identity {
        let mac = mac.to_lowercase();
        let machex: String = mac.chars().filter(|c| *c != ':').collect();
        let bridge_id = format!("{}fffe{}", &machex[..6], &machex[6..]).to_uppercase();
        let upnp_uuid = format!("2f402f80-da50-11e1-9b23-{machex}");
        Identity {
            mac,
            machex,
            bridge_id,
            upnp_uuid,
        }
    }
}

/// The relay's application key on the real bridge.
#[derive(Deserialize, Clone, Debug)]
pub struct RealKey {
    pub username: String,
    pub clientkey: String,
}

/// A client paired with the relay; the same file format as before, so pairings carry over.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Client {
    pub devicetype: String,
    pub created: String,
    pub auth_id: String,
    #[serde(default)]
    pub clientkey: Option<String>,
}

pub type Clients = BTreeMap<String, Client>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_id_from_mac() {
        let id = Identity::from_mac("02:00:00:AB:CD:01");
        assert_eq!(id.bridge_id, "020000FFFEABCD01");
        assert_eq!(id.machex, "020000abcd01");
        assert_eq!(id.upnp_uuid, "2f402f80-da50-11e1-9b23-020000abcd01");
    }

    #[test]
    fn reads_the_python_files() {
        let cfg: Config = serde_json::from_str(
            r#"{"relay_ip": "192.168.1.250", "relay_mac": "02:00:00:ab:cd:01", "real_bridge": "192.168.1.10",
                "key_file": "/k", "link_open": false, "gains": {"a": [0.4, 0.3]}, "outside_sync": ["a"]}"#,
        )
        .unwrap();
        assert_eq!(cfg.gains["a"], vec![0.4, 0.3]);
        assert_eq!(cfg.outside_sync, vec!["a".to_string()]);
        let clients: Clients = serde_json::from_str(
            r#"{"u": {"devicetype": "Hue Sync TV#LG TV", "created": "2026-11-26T19:34:00", "auth_id": "x", "clientkey": "AB"}}"#,
        )
        .unwrap();
        assert_eq!(clients["u"].clientkey.as_deref(), Some("AB"));
    }
}
