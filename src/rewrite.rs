//! What clients see: the real bridge's answers, under the relay's identity and with their own streams.

use serde_json::{Map, Value, json};

use crate::relay::{Relay, now_iso};

pub fn public_config(relay: &Relay) -> Value {
    let real = relay.real_public.read().unwrap();
    let pick = |k: &str, d: &str| real.get(k).cloned().unwrap_or_else(|| json!(d));
    json!({
        "name": relay.cfg.name,
        "datastoreversion": pick("datastoreversion", "199"),
        "swversion": pick("swversion", "1978688000"),
        "apiversion": pick("apiversion", "1.78.0"),
        "mac": relay.id.mac,
        "bridgeid": relay.id.bridge_id,
        "factorynew": false,
        "replacesbridgeid": null,
        "modelid": "BSB002",
        "starterkitid": "",
    })
}

/// The full v1 config, from the real bridge's, with our identity and our own clients as whitelist.
pub fn rewrite_config(relay: &Relay, c: &mut Value) {
    let Some(obj) = c.as_object_mut() else { return };
    obj.insert("name".into(), json!(relay.cfg.name));
    obj.insert("mac".into(), json!(relay.id.mac));
    obj.insert("bridgeid".into(), json!(relay.id.bridge_id));
    obj.insert("ipaddress".into(), json!(relay.cfg.relay_ip.to_string()));
    obj.insert("modelid".into(), json!("BSB002"));
    obj.insert("linkbutton".into(), json!(relay.cfg.link_open));
    let now = now_iso();
    let whitelist: Map<String, Value> = relay
        .clients
        .read()
        .unwrap()
        .iter()
        .map(|(u, c)| {
            (
                u.clone(),
                json!({"name": c.devicetype, "create date": c.created, "last use date": now}),
            )
        })
        .collect();
    obj.insert("whitelist".into(), Value::Object(whitelist));
}

/// CLIP v2 documents: our bridge id, our name on the bridge device, and each client's own stream.
pub fn rewrite_v2(relay: &Relay, doc: &mut Value) {
    let Some(items) = doc.get_mut("data").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        match item["type"].as_str() {
            Some("bridge") => {
                item["bridge_id"] = json!(relay.id.bridge_id.to_lowercase());
            }
            Some("device") => {
                let is_bridge = item["services"]
                    .as_array()
                    .is_some_and(|s| s.iter().any(|sv| sv["rtype"] == "bridge"));
                if is_bridge {
                    if !item["metadata"].is_object() {
                        item["metadata"] = json!({});
                    }
                    item["metadata"]["name"] = json!(relay.cfg.name);
                }
            }
            Some("entertainment_configuration") => fix_area(relay, item),
            _ => {}
        }
    }
}

/// On the real bridge every relayed stream belongs to the relay's key; a client that sees that
/// reports "another app is syncing". Show the client streaming as the streamer, and a stream of ours
/// with no client behind it (just ending) as none.
pub fn fix_area(relay: &Relay, item: &mut Value) {
    let Some(area) = item["id"].as_str().map(str::to_string) else {
        return;
    };
    if let Some(user) = relay.area_client(&area) {
        let auth = relay
            .clients
            .read()
            .unwrap()
            .get(&user)
            .map(|c| c.auth_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        item["status"] = json!("active");
        item["active_streamer"] = json!({"rid": auth, "rtype": "auth_v1"});
        return;
    }
    let ours = relay.relay_app_id.read().unwrap().clone();
    if ours.is_some() && item["active_streamer"]["rid"].as_str() == ours.as_deref() {
        item["status"] = json!("inactive");
        if let Some(o) = item.as_object_mut() {
            o.remove("active_streamer");
        }
    }
}

/// One server-sent event from the real bridge's event stream, with its area items fixed.
pub fn rewrite_event(relay: &Relay, ev: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ev.len());
    for (i, line) in ev.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        if let Some(data) = line.strip_prefix(b"data: ")
            && let Ok(mut events) = serde_json::from_slice::<Value>(data)
        {
            if let Some(list) = events.as_array_mut() {
                for e in list {
                    if let Some(items) = e.get_mut("data").and_then(Value::as_array_mut) {
                        for item in items {
                            if item["type"] == "entertainment_configuration" {
                                fix_area(relay, item);
                            }
                        }
                    }
                }
            }
            out.extend_from_slice(b"data: ");
            out.extend_from_slice(&serde_json::to_vec(&events).unwrap());
            continue;
        }
        out.extend_from_slice(line);
    }
    out
}
