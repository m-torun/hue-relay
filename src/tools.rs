//! Helper commands: get a key from a bridge, and stream test patterns to a bridge or a relay.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use webrtc_util::Conn;

use crate::config::RealKey;
use crate::{entertainment, stream};

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()?)
}

async fn request_key(bridge: &str, devicetype: &str) -> Result<Option<RealKey>> {
    let reply: Value = http()?
        .post(format!("https://{bridge}/api"))
        .json(&json!({"devicetype": devicetype, "generateclientkey": true}))
        .send()
        .await?
        .json()
        .await?;
    let ok = &reply[0]["success"];
    if ok.is_object() {
        return Ok(Some(serde_json::from_value(ok.clone())?));
    }
    if reply[0]["error"]["type"] == 101 {
        return Ok(None); // link button not pressed yet
    }
    bail!("bridge said {reply}")
}

/// Wait for the link button and print {"username", "clientkey"} for the relay's key_file.
pub async fn pair_bridge(bridge: &str, wait: u64, devicetype: &str) -> Result<()> {
    eprintln!("Press the link button on the bridge at {bridge} (waiting up to {wait} s)...");
    let end = Instant::now() + Duration::from_secs(wait);
    while Instant::now() < end {
        if let Some(k) = request_key(bridge, devicetype).await? {
            println!("{}", json!({"username": k.username, "clientkey": k.clientkey}));
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("no button press in time")
}

pub struct StreamTest {
    pub bridge: String,
    pub key_file: PathBuf,
    pub area: String,
    pub brightness: f64,
    pub mirek: f64,
    pub levels: Vec<f64>,
    pub colors: bool,
    pub seconds: f64,
    pub interval: f64,
}

/// Start an area, stream a pattern, stop it. The area is taken from any client streaming to it.
pub async fn stream_test(t: StreamTest) -> Result<()> {
    let key: RealKey = match std::fs::read(&t.key_file) {
        Ok(b) => serde_json::from_slice(&b).context("parsing the key file")?,
        Err(_) => {
            // A relay with pairing open answers at once; a real bridge needs its button.
            eprintln!(
                "no key in {}: pairing with {} (press its link button if it is a real bridge)",
                t.key_file.display(),
                t.bridge
            );
            let end = Instant::now() + Duration::from_secs(60);
            let key = loop {
                if let Some(k) = request_key(&t.bridge, "stream-test#hue-relay").await? {
                    break k;
                }
                if Instant::now() > end {
                    bail!("not paired");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            };
            std::fs::write(
                &t.key_file,
                json!({"username": key.username, "clientkey": key.clientkey}).to_string(),
            )?;
            tokio::time::sleep(Duration::from_secs(1)).await; // a relay loads the new key
            key
        }
    };
    let client = http()?;
    let areas: Value = client
        .get(format!(
            "https://{}/clip/v2/resource/entertainment_configuration",
            t.bridge
        ))
        .header("hue-application-key", &key.username)
        .send()
        .await?
        .json()
        .await?;
    let area = areas["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["id"] == t.area.as_str() || a["metadata"]["name"] == t.area.as_str())
        .with_context(|| format!("no area {:?}", t.area))?;
    let id = area["id"].as_str().unwrap().to_string();
    let channels: Vec<u8> = area["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["channel_id"].as_u64().map(|v| v as u8))
        .collect();
    let action = |a: &'static str| {
        let client = client.clone();
        let url = format!("https://{}/clip/v2/resource/entertainment_configuration/{id}", t.bridge);
        let user = key.username.clone();
        async move {
            client
                .put(url)
                .header("hue-application-key", user)
                .json(&json!({"action": a}))
                .send()
                .await?
                .text()
                .await
                .map_err(anyhow::Error::from)
        }
    };
    eprintln!("start: {}", action("start").await?);
    let conn = entertainment::connect(&t.bridge, &key.username, &key.clientkey).await?;

    let (wx, wy) = stream::mirek_to_xy(t.mirek);
    let level = |ch: u8| t.levels.get(ch as usize).copied().unwrap_or(1.0);
    let steady = stream::xyb_frame_per_channel(
        &id,
        &channels
            .iter()
            .map(|&ch| (ch, wx, wy, t.brightness * level(ch)))
            .collect::<Vec<_>>(),
    );
    let steps: Vec<(&str, Vec<u8>, f64)> = if t.colors {
        let rgb = |r: u16, g: u16, b: u16| {
            let mut p = stream::xyb_frame(&id, &channels, 0.0, 0.0, 0.0);
            p[14] = 0; // RGB colour space
            let mut off = 52;
            while off + 7 <= p.len() {
                p[off + 1..off + 7].copy_from_slice(&[
                    (r >> 8) as u8,
                    r as u8,
                    (g >> 8) as u8,
                    g as u8,
                    (b >> 8) as u8,
                    b as u8,
                ]);
                off += 7;
            }
            p
        };
        vec![
            ("red", rgb(65535, 0, 0), 4.0),
            ("green", rgb(0, 65535, 0), 4.0),
            ("blue", rgb(0, 0, 65535), 4.0),
            ("white", steady, 4.0),
        ]
    } else {
        vec![("pattern", steady, t.seconds)]
    };
    for (name, frame, seconds) in steps {
        eprintln!("{} {name}", time::OffsetDateTime::now_utc().time());
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            conn.send(&frame).await?;
            tokio::time::sleep(Duration::from_secs_f64(t.interval)).await;
        }
    }
    let _ = conn.close().await;
    eprintln!("stop: {}", action("stop").await?);
    Ok(())
}

/// Print each entertainment area: id, name, and per channel the light and its position along it.
pub async fn list_areas(bridge: &str, key_file: &std::path::Path) -> Result<()> {
    let key: RealKey = serde_json::from_slice(&std::fs::read(key_file).context("reading the key file")?)?;
    let client = http()?;
    let get = |what: &str| {
        let (client, user) = (client.clone(), key.username.clone());
        let url = format!("https://{bridge}/clip/v2/resource/{what}");
        async move {
            Ok::<Value, anyhow::Error>(
                client
                    .get(url)
                    .header("hue-application-key", user)
                    .send()
                    .await?
                    .json()
                    .await?,
            )
        }
    };
    let (areas, ent, lights) = (
        get("entertainment_configuration").await?,
        get("entertainment").await?,
        get("light").await?,
    );
    let names: std::collections::HashMap<&str, &str> = lights["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| Some((l["id"].as_str()?, l["metadata"]["name"].as_str()?)))
        .collect();
    let (ent, lights) = (
        ent["data"].as_array().cloned().unwrap_or_default(),
        lights["data"].as_array().cloned().unwrap_or_default(),
    );
    for a in areas["data"].as_array().into_iter().flatten() {
        println!(
            "{}  \"{}\"  ({})",
            a["id"].as_str().unwrap_or("?"),
            a["metadata"]["name"].as_str().unwrap_or("?"),
            a["status"].as_str().unwrap_or("?")
        );
        let map = crate::lights::AreaMap::build(a, &ent, &lights);
        for (ch, light, t) in map.channels {
            println!(
                "  channel {ch}: {} at {:.0}% of its length",
                names.get(light.as_str()).unwrap_or(&"?"),
                t * 100.0
            );
        }
    }
    Ok(())
}
