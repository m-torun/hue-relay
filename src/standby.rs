//! Between syncs, for areas in `outside_sync`: keep streaming the lights' own state with the area's
//! per-channel levels.
//!
//! The bridge keeps accepting commands for a light while it streams and announces them on its event
//! stream, so apps go on controlling the light as usual and the relay shows what they set. Frames go
//! out when something changes (fading over about 0.4 s) and otherwise every 4 s: a light keeps the
//! last frame while the session lives, and the bridge ends a session after 10 s without frames.
//! When every light of an area is off, the relay fades to black and ends the stream; the bridge then
//! leaves the lights off.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use tracing::{info, warn};

use crate::lights::{AreaMap, LightState};
use crate::relay::{Relay, short};
use crate::stream;

const TICK: Duration = Duration::from_millis(40);
const REFRESH: Duration = Duration::from_secs(4);
/// Largest brightness step per tick: a full change takes 0.4 s.
const STEP: f64 = 0.1;

/// Read the outside_sync areas' layout and their lights' state from the bridge.
pub async fn load(relay: &Relay) -> Result<()> {
    let areas = relay
        .bridge
        .get_json("/clip/v2/resource/entertainment_configuration")
        .await?;
    let ent = relay.bridge.get_json("/clip/v2/resource/entertainment").await?;
    let lights = relay.bridge.get_json("/clip/v2/resource/light").await?;
    let ent = ent["data"].as_array().context("entertainment")?;
    let lights = lights["data"].as_array().context("lights")?;
    let mut maps = HashMap::new();
    for a in areas["data"].as_array().context("areas")? {
        let Some(id) = a["id"].as_str() else { continue };
        if relay.outside_sync(id) {
            let m = AreaMap::build(a, ent, lights);
            info!(
                "outside sync {}: {} channels on {} light(s)",
                short(id),
                m.channels.len(),
                m.lights().len()
            );
            maps.insert(id.to_string(), m);
        }
    }
    for want in &relay.cfg.outside_sync {
        if !maps.contains_key(want) {
            warn!("outside sync: no entertainment area {want} on the bridge");
        }
    }
    let wanted: Vec<String> = maps.values().flat_map(|m| m.lights()).collect();
    let mut states = HashMap::new();
    for l in lights {
        if let Some(id) = l["id"].as_str().filter(|id| wanted.iter().any(|w| w == id)) {
            let mut s = LightState::default();
            s.merge(l);
            states.insert(id.to_string(), s);
        }
    }
    *relay.area_maps.write().unwrap() = maps;
    *relay.lights.write().unwrap() = states;
    Ok(())
}

/// Follow the bridge's event stream for changes to those lights; reload after every reconnect.
pub async fn watch(relay: Arc<Relay>) {
    loop {
        if let Err(e) = load(&relay).await {
            warn!("outside sync: cannot read the bridge ({e}); retrying");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        match relay.bridge.eventstream().await {
            Ok(resp) => {
                let mut body = resp.bytes_stream();
                let mut buf = Vec::<u8>::new();
                while let Some(Ok(chunk)) = body.next().await {
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
                        let ev: Vec<u8> = buf.drain(..pos + 2).collect();
                        apply_event(&relay, &ev);
                    }
                }
                warn!("outside sync: the bridge's event stream ended; reconnecting");
            }
            Err(e) => warn!("outside sync: no event stream ({e}); retrying"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn apply_event(relay: &Relay, ev: &[u8]) {
    for line in ev.split(|b| *b == b'\n') {
        let Some(data) = line.strip_prefix(b"data: ") else {
            continue;
        };
        let Ok(events) = serde_json::from_slice::<Value>(data) else {
            continue;
        };
        for e in events.as_array().into_iter().flatten() {
            for item in e["data"].as_array().into_iter().flatten() {
                if item["type"] != "light" {
                    continue;
                }
                let Some(id) = item["id"].as_str() else { continue };
                if let Some(s) = relay.lights.write().unwrap().get_mut(id) {
                    let before = s.clone();
                    s.merge(item);
                    if *s != before {
                        info!(
                            "light {}: {}, {:.0}%{}",
                            short(id),
                            if s.on { "on" } else { "off" },
                            s.brightness,
                            if s.gradient.len() >= 2 { ", gradient" } else { "" }
                        );
                    }
                }
            }
        }
    }
}

/// Show the lights' own state on each outside_sync area while no client streams to it.
pub async fn render(relay: Arc<Relay>) {
    let mut shown: HashMap<String, Vec<(u8, f64, f64, f64)>> = HashMap::new();
    let mut sent: HashMap<String, Instant> = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let maps = relay.area_maps.read().unwrap().clone();
        for (area, map) in maps {
            if relay.area_client(&area).is_some() {
                shown.remove(&area); // the client has the stream; pick up from the lights' state after it
                continue;
            }
            let target = map.render(&relay.lights.read().unwrap());
            let any_on = target.iter().any(|c| c.3 > 0.0);
            let has_session = relay.sessions.lock().unwrap().contains_key(&area);
            if !any_on && !has_session {
                shown.remove(&area);
                continue;
            }
            // Start from the target itself (no dip when we take over); fade later changes.
            let cur = shown.entry(area.clone()).or_insert_with(|| target.clone());
            let mut changed = false;
            for (c, t) in cur.iter_mut().zip(&target) {
                let next = (
                    c.0,
                    c.1 + (t.1 - c.1) * 0.3,
                    c.2 + (t.2 - c.2) * 0.3,
                    c.3 + (t.3 - c.3).clamp(-STEP, STEP),
                );
                let next = if (t.1 - next.1).abs() < 1e-4 && (t.2 - next.2).abs() < 1e-4 {
                    (next.0, t.1, t.2, next.3)
                } else {
                    next
                };
                if next != *c {
                    changed = true;
                    *c = next;
                }
            }
            let dark = cur.iter().all(|c| c.3 <= 1e-4);
            if !any_on && dark {
                // Faded out: end the stream; the bridge leaves the lights off.
                relay.stop_session(&area);
                shown.remove(&area);
                sent.remove(&area);
                continue;
            }
            let Some(up) = relay.own_upstream(&area) else { continue };
            if changed || sent.get(&area).is_none_or(|t| t.elapsed() >= REFRESH) {
                let gains = relay.gains.read().unwrap().clone();
                if let stream::Frame::V2 { out, .. } =
                    stream::apply_gains(&stream::xyb_frame_per_channel(&area, cur), &gains)
                {
                    up.send(out);
                }
                sent.insert(area.clone(), Instant::now());
            }
        }
    }
}

/// On shutdown: start nothing new, end our streams, and switch back on the lights the relay was
/// showing as on. The bridge turns an area's lights off about a second after its stream ends; this way
/// they keep their own state (with equal segments) until the relay is back. A crash cannot do this,
/// so then the lights go off.
pub async fn shutdown(relay: &Relay) {
    relay.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
    let own: Vec<String> = relay
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, s)| s.client.is_none())
        .map(|(a, _)| a.clone())
        .collect();
    let lit: Vec<String> = {
        let maps = relay.area_maps.read().unwrap();
        let states = relay.lights.read().unwrap();
        own.iter()
            .filter_map(|a| maps.get(a))
            .flat_map(|m| m.lights())
            .filter(|l| states.get(l).is_some_and(|s| s.on))
            .collect()
    };
    let areas: Vec<String> = relay.sessions.lock().unwrap().keys().cloned().collect();
    for a in &areas {
        relay.stop_session(a);
    }
    if lit.is_empty() {
        tokio::time::sleep(Duration::from_millis(500)).await; // let the stops go out
        return;
    }
    tokio::time::sleep(Duration::from_millis(2500)).await; // our stops go out; the bridge turns the lights off
    for attempt in 1..=3 {
        let mut still_off = Vec::new();
        for l in &lit {
            let body = bytes::Bytes::from_static(br#"{"on":{"on":true}}"#);
            if let Err(e) = relay
                .bridge
                .request(
                    "PUT",
                    &format!("/clip/v2/resource/light/{l}"),
                    Some(body),
                    Some("application/json"),
                )
                .await
            {
                warn!("light {}: could not switch it back on: {e}", short(l));
            }
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        for l in &lit {
            let on = relay
                .bridge
                .get_json(&format!("/clip/v2/resource/light/{l}"))
                .await
                .ok()
                .and_then(|v| v["data"][0]["on"]["on"].as_bool());
            if on == Some(true) {
                info!("light {}: switched back on", short(l));
            } else {
                still_off.push(l.clone());
            }
        }
        if still_off.is_empty() {
            return;
        }
        warn!("{} light(s) still off after attempt {attempt}", still_off.len());
    }
}
