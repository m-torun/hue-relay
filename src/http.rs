//! The bridge API clients talk to: description, pairing, v1, CLIP v2, the event stream.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::relay::{Relay, short};
use crate::rewrite;

#[derive(Clone)]
pub struct Web {
    pub relay: Arc<Relay>,
    pub secure: bool,
}

pub fn router(relay: Arc<Relay>, secure: bool) -> Router {
    Router::new().fallback(handle).with_state(Web { relay, secure })
}

fn json_response(status: StatusCode, v: &Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn raw_json(status: StatusCode, body: Bytes) -> Response {
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn v1_error(kind: u32, address: &str, desc: &str) -> Response {
    json_response(
        StatusCode::OK,
        &json!([{"error": {"type": kind, "address": address, "description": desc}}]),
    )
}

fn lossy(b: &[u8], n: usize) -> String {
    String::from_utf8_lossy(&b[..b.len().min(n)]).into_owned()
}

async fn handle(
    State(web): State<Web>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let relay = &web.relay;
    let path = uri.path().to_string();
    let key = headers
        .get("hue-application-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    info!(
        "http {} {} {}{} ua={:?} key={} body={:?}",
        peer.ip(),
        method,
        if web.secure { "https " } else { "" },
        uri.path_and_query().map(|p| p.as_str()).unwrap_or(&path),
        &ua[..ua.len().min(60)],
        short(&key),
        lossy(&body, 300)
    );
    if relay.is_client(&key) {
        relay.note_user(&key);
    } else if let Some(u) = path.strip_prefix("/api/").and_then(|p| p.split('/').next())
        && relay.is_client(u)
    {
        relay.note_user(u);
    }

    let resp = if path == "/description.xml" {
        description(relay)
    } else if (path == "/api" || path == "/api/") && method == Method::POST {
        pair(relay, &peer, &body)
    } else if path == "/api" || path.starts_with("/api/") {
        v1(relay, &method, &path, body).await
    } else if path.starts_with("/clip/v2/") {
        v2(relay, &method, &path, &key, &headers, body).await
    } else if path.starts_with("/eventstream/") {
        return eventstream(relay.clone(), &key).await;
    } else if path == "/auth/v1" {
        auth(relay, &key)
    } else {
        (StatusCode::NOT_FOUND, "not found").into_response()
    };
    info!("  -> {}", resp.status().as_u16());
    resp
}

fn description(relay: &Relay) -> Response {
    let ip = relay.cfg.relay_ip;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<URLBase>http://{ip}:80/</URLBase>
<device>
<deviceType>urn:schemas-upnp-org:device:Basic:1</deviceType>
<friendlyName>{name} ({ip})</friendlyName>
<manufacturer>Signify</manufacturer>
<manufacturerURL>http://www.philips-hue.com</manufacturerURL>
<modelDescription>Philips hue Personal Wireless Lighting</modelDescription>
<modelName>Philips hue bridge 2015</modelName>
<modelNumber>BSB002</modelNumber>
<modelURL>http://www.philips-hue.com</modelURL>
<serialNumber>{serial}</serialNumber>
<UDN>uuid:{uuid}</UDN>
<presentationURL>index.html</presentationURL>
</device>
</root>
"#,
        name = relay.cfg.name,
        serial = relay.id.machex,
        uuid = relay.id.upnp_uuid
    );
    ([(header::CONTENT_TYPE, "text/xml")], xml).into_response()
}

fn pair(relay: &Relay, peer: &SocketAddr, body: &[u8]) -> Response {
    let Ok(j) = serde_json::from_slice::<Value>(if body.is_empty() { b"{}" } else { body }) else {
        return v1_error(2, "/", "body contains invalid json");
    };
    let Some(devicetype) = j["devicetype"].as_str() else {
        return v1_error(5, "/", "invalid/missing parameters in body");
    };
    if !relay.cfg.link_open {
        return v1_error(101, "", "link button not pressed");
    }
    let (username, clientkey) = relay.pair(devicetype, j["generateclientkey"].as_bool().unwrap_or(false));
    warn!(
        "PAIRED {devicetype} ({}), clientkey {}",
        peer.ip(),
        if clientkey.is_some() { "yes" } else { "no" }
    );
    let mut ok = json!({"username": username});
    if let Some(k) = clientkey {
        ok["clientkey"] = json!(k);
    }
    json_response(StatusCode::OK, &json!([{"success": ok}]))
}

async fn v1(relay: &Arc<Relay>, method: &Method, path: &str, body: Bytes) -> Response {
    let parts: Vec<&str> = path.split('/').skip(2).collect();
    let user = parts.first().copied().unwrap_or("");
    let rest: Vec<&str> = parts.iter().skip(1).copied().filter(|p| !p.is_empty()).collect();
    if user == "config" || (rest == ["config"] && !relay.is_client(user)) {
        return json_response(StatusCode::OK, &rewrite::public_config(relay));
    }
    if !relay.is_client(user) {
        return v1_error(1, &format!("/{}", rest.join("/")), "unauthorized user");
    }
    if method == Method::PUT
        && rest.len() == 2
        && rest[0] == "groups"
        && let Ok(j) = serde_json::from_slice::<Value>(&body)
        && let Some(stream) = j.get("stream")
    {
        let active = stream["active"].as_bool().unwrap_or(false);
        if let Some(area) = area_for_v1_group(relay, rest[1]).await {
            if active {
                relay.client_start(&area, user);
            } else {
                relay.client_stop(&area, None);
            }
        }
        return json_response(
            StatusCode::OK,
            &json!([{"success": {format!("/groups/{}/stream/active", rest[1]): active}}]),
        );
    }
    let real_path = if rest.is_empty() {
        format!("/api/{}", relay.bridge.key.username)
    } else {
        format!("/api/{}/{}", relay.bridge.key.username, rest.join("/"))
    };
    let send_body = (method != Method::GET).then_some(body);
    match relay
        .bridge
        .request(method.as_str(), &real_path, send_body, Some("application/json"))
        .await
    {
        Ok((status, data)) if method == Method::GET => match serde_json::from_slice::<Value>(&data) {
            Ok(mut j) => {
                if rest.is_empty() {
                    if let Some(c) = j.get_mut("config") {
                        rewrite::rewrite_config(relay, c);
                    }
                } else if rest == ["config"] {
                    rewrite::rewrite_config(relay, &mut j);
                }
                raw_json(status, Bytes::from(j.to_string()))
            }
            Err(_) => raw_json(status, data),
        },
        Ok((status, data)) => raw_json(status, data),
        Err(e) => v1_error(901, path, &format!("Internal error: {e}")),
    }
}

async fn area_for_v1_group(relay: &Relay, gid: &str) -> Option<String> {
    let doc = relay
        .bridge
        .get_json("/clip/v2/resource/entertainment_configuration")
        .await
        .ok()?;
    doc["data"]
        .as_array()?
        .iter()
        .find(|c| c["id_v1"].as_str() == Some(&format!("/groups/{gid}")))
        .and_then(|c| c["id"].as_str().map(str::to_string))
}

async fn v2(relay: &Arc<Relay>, method: &Method, path: &str, key: &str, headers: &HeaderMap, body: Bytes) -> Response {
    if !relay.is_client(key) {
        return json_response(
            StatusCode::FORBIDDEN,
            &json!({"errors": [{"description": "unauthorized user"}], "data": []}),
        );
    }
    let segs: Vec<&str> = path.split('/').skip(3).collect(); // resource, type, id
    if method == Method::PUT && segs.len() == 3 && segs[1] == "entertainment_configuration" {
        let area = segs[2];
        let mut j: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        let action = j.as_object_mut().and_then(|o| o.remove("action"));
        match action.as_ref().and_then(Value::as_str) {
            Some("start") => relay.client_start(area, key),
            Some("stop") => relay.client_stop(area, None),
            _ => {}
        }
        if j.as_object().is_some_and(|o| !o.is_empty()) {
            let _ = relay
                .bridge
                .request("PUT", path, Some(Bytes::from(j.to_string())), Some("application/json"))
                .await;
        }
        return json_response(
            StatusCode::OK,
            &json!({"data": [{"rid": area, "rtype": "entertainment_configuration"}], "errors": []}),
        );
    }
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");
    let send_body = (method != Method::GET).then_some(body);
    match relay.bridge.request(method.as_str(), path, send_body, Some(ct)).await {
        Ok((status, data)) if method == Method::GET => match serde_json::from_slice::<Value>(&data) {
            Ok(mut j) => {
                rewrite::rewrite_v2(relay, &mut j);
                raw_json(status, Bytes::from(j.to_string()))
            }
            Err(_) => raw_json(status, data),
        },
        Ok((status, data)) => raw_json(status, data),
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            &json!({"errors": [{"description": e.to_string()}], "data": []}),
        ),
    }
}

fn auth(relay: &Relay, key: &str) -> Response {
    match relay.clients.read().unwrap().get(key) {
        Some(c) => {
            let mut r = StatusCode::OK.into_response();
            if let Ok(v) = HeaderValue::from_str(&c.auth_id) {
                r.headers_mut().insert("hue-application-id", v);
            }
            r
        }
        None => StatusCode::FORBIDDEN.into_response(),
    }
}

/// The real bridge's event stream, event by event, with each area shown as the client sees it.
async fn eventstream(relay: Arc<Relay>, key: &str) -> Response {
    if !relay.is_client(key) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let resp = match relay.bridge.eventstream().await {
        Ok(r) => r,
        Err(e) => {
            warn!("eventstream from the real bridge: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let events = futures_util::stream::unfold(
        (resp.bytes_stream(), Vec::<u8>::new(), relay),
        |(mut upstream, mut buf, relay)| async move {
            loop {
                if let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
                    let ev: Vec<u8> = buf.drain(..pos + 2).collect();
                    let mut out = rewrite::rewrite_event(&relay, &ev[..pos]);
                    out.extend_from_slice(b"\n\n");
                    return Some((Ok::<Bytes, std::io::Error>(Bytes::from(out)), (upstream, buf, relay)));
                }
                match upstream.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    _ => return None,
                }
            }
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(events))
        .unwrap()
}
