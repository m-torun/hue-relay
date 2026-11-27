//! Shared state: pairings, gains, the lights' own state, and the sessions that stream to the real bridge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};
use webrtc_dtls::conn::DTLSConn;
use webrtc_util::Conn;

use crate::bridge::Bridge;
use crate::config::{Client, Clients, Config, Identity, RealKey};
use crate::lights::{AreaMap, LightState};
use crate::stream::Gains;

/// Our stream to the real bridge for an area: carrying a client's frames, or (client None, areas in
/// outside_sync) the lights' own state. Both share the stream, so a hand-over never interrupts it.
pub struct Session {
    pub client: Option<String>,
    pub client_since: Instant,
    pub client_last: Instant,
    pub started: Instant,
    pub upstream: Arc<Upstream>,
}

pub struct Relay {
    pub cfg: Config,
    pub id: Identity,
    pub bridge: Bridge,
    pub clients: RwLock<Clients>,
    pub gains: RwLock<Gains>,
    pub sessions: Mutex<HashMap<String, Session>>,
    /// Area id -> id of the DTLS connection feeding it.
    pub active_conn: Mutex<HashMap<String, u64>>,
    pub conn_ids: AtomicU64,
    pub last_user: Mutex<Option<String>>,
    pub real_public: RwLock<Value>,
    pub relay_app_id: RwLock<Option<String>>,
    /// The lights' own state and where each outside_sync area's channels sit.
    pub lights: RwLock<HashMap<String, LightState>>,
    pub area_maps: RwLock<HashMap<String, AreaMap>>,
    /// Set at shutdown: no new streams from then on.
    pub stopping: AtomicBool,
}

pub fn now_iso() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

pub fn short(s: &str) -> &str {
    &s[..s.len().min(8)]
}

impl Relay {
    pub fn new(cfg: Config, key: RealKey) -> Result<Relay> {
        let clients: Clients = match std::fs::read(cfg.clients_file()) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(_) => Clients::new(),
        };
        Ok(Relay {
            id: Identity::from_mac(&cfg.relay_mac),
            bridge: Bridge::new(&cfg.real_bridge, key)?,
            gains: RwLock::new(cfg.gains.clone()),
            cfg,
            clients: RwLock::new(clients),
            sessions: Mutex::new(HashMap::new()),
            active_conn: Mutex::new(HashMap::new()),
            conn_ids: AtomicU64::new(1),
            last_user: Mutex::new(None),
            real_public: RwLock::new(Value::Null),
            relay_app_id: RwLock::new(None),
            lights: RwLock::new(HashMap::new()),
            area_maps: RwLock::new(HashMap::new()),
            stopping: AtomicBool::new(false),
        })
    }

    // ------------------------------------------------------------ pairings

    pub fn pair(&self, devicetype: &str, with_clientkey: bool) -> (String, Option<String>) {
        let mut raw = [0u8; 30];
        getrandom::fill(&mut raw).expect("random");
        use base64::Engine;
        let username = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(raw)
            .replace('_', "-");
        let clientkey = with_clientkey.then(|| {
            let mut k = [0u8; 16];
            getrandom::fill(&mut k).expect("random");
            hex::encode_upper(k)
        });
        let client = Client {
            devicetype: devicetype.to_string(),
            created: now_iso(),
            auth_id: uuid::Uuid::new_v4().to_string(),
            clientkey: clientkey.clone(),
        };
        let mut clients = self.clients.write().unwrap();
        clients.insert(username.clone(), client);
        if let Err(e) = write_atomic(&self.cfg.clients_file(), &serde_json::to_vec_pretty(&*clients).unwrap()) {
            warn!("cannot save pairings: {e}");
        }
        (username, clientkey)
    }

    pub fn is_client(&self, user: &str) -> bool {
        self.clients.read().unwrap().contains_key(user)
    }

    /// The PSK for a DTLS identity: a username, or the application id from /auth/v1.
    pub fn psk_for(&self, identity: &[u8]) -> Option<Vec<u8>> {
        let identity = std::str::from_utf8(identity).ok()?;
        let clients = self.clients.read().unwrap();
        let c = clients
            .get(identity)
            .or_else(|| clients.values().find(|c| c.auth_id == identity))?;
        hex::decode(c.clientkey.as_ref()?).ok()
    }

    pub fn note_user(&self, user: &str) {
        *self.last_user.lock().unwrap() = Some(user.to_string());
    }

    fn client_name(&self, user: &str) -> String {
        self.clients
            .read()
            .unwrap()
            .get(user)
            .map(|c| c.devicetype.clone())
            .unwrap_or_else(|| short(user).to_string())
    }

    // ------------------------------------------------------------ sessions

    pub fn outside_sync(&self, area: &str) -> bool {
        self.cfg.outside_sync.iter().any(|a| a == area)
    }

    fn new_session(self: &Arc<Self>, area: &str, client: Option<String>) -> Session {
        let now = Instant::now();
        Session {
            client,
            client_since: now,
            client_last: now,
            started: now,
            upstream: Upstream::spawn(self.clone(), area.to_string()),
        }
    }

    /// A client starts streaming to an area. It takes over our running stream if there is one (another
    /// client's, or the lights' own state), so the light never drops out of streaming in between.
    pub fn client_start(self: &Arc<Self>, area: &str, user: &str) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        let reuse = sessions.get(area).is_some_and(|s| !s.upstream.is_closed());
        if !reuse {
            sessions.insert(area.to_string(), self.new_session(area, None));
        }
        let s = sessions.get_mut(area).unwrap();
        if s.client.as_deref() != Some(user) {
            info!("session {}: client {}", short(area), self.client_name(user));
        }
        s.client = Some(user.to_string());
        s.client_since = now;
        s.client_last = now;
    }

    /// The client stopped or went away. Areas in outside_sync go back to the lights' own state on the
    /// same stream; others end it. With since=t, only if the client started before t: an old
    /// connection ending must not end a newer start.
    pub fn client_stop(&self, area: &str, since: Option<Instant>) {
        let end = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(s) = sessions.get_mut(area) else { return };
            let Some(client) = s.client.clone() else { return };
            if since.is_some_and(|t| s.client_since >= t) {
                return;
            }
            info!(
                "session {}: {} done after {:.0} s",
                short(area),
                self.client_name(&client),
                s.client_since.elapsed().as_secs_f64()
            );
            s.client = None;
            !self.outside_sync(area)
        };
        if end {
            self.stop_session(area);
        }
    }

    pub fn stop_session(&self, area: &str) {
        if let Some(s) = self.sessions.lock().unwrap().remove(area) {
            info!(
                "session {}: stream stopped after {:.0} s",
                short(area),
                s.started.elapsed().as_secs_f64()
            );
            s.upstream.close();
        }
    }

    /// Our stream for showing the lights' own state, started if need be; None while a client streams.
    pub fn own_upstream(self: &Arc<Self>, area: &str) -> Option<Arc<Upstream>> {
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(area) {
            Some(s) if s.client.is_some() => None,
            Some(s) if !s.upstream.is_closed() => Some(s.upstream.clone()),
            _ => {
                info!("session {}: showing the lights' own state", short(area));
                let s = self.new_session(area, None);
                let up = s.upstream.clone();
                sessions.insert(area.to_string(), s);
                Some(up)
            }
        }
    }

    /// A client's frame: start its turn if it streams without a start call, and pass the frame on.
    pub fn client_frame(self: &Arc<Self>, area: &str, frame: Vec<u8>) {
        let running = self
            .sessions
            .lock()
            .unwrap()
            .get(area)
            .is_some_and(|s| s.client.is_some() && !s.upstream.is_closed());
        if !running {
            let user = self
                .last_user
                .lock()
                .unwrap()
                .clone()
                .or_else(|| self.clients.read().unwrap().keys().next().cloned())
                .unwrap_or_default();
            self.client_start(area, &user);
        }
        let up = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(s) = sessions.get_mut(area) else { return };
            s.client_last = Instant::now();
            s.upstream.clone()
        };
        up.send(frame);
    }

    /// The client streaming to an area, if any.
    pub fn area_client(&self, area: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(area).and_then(|s| s.client.clone())
    }

    /// A client gone for 15 s loses its turn, so a vanished client never freezes the light.
    pub async fn watchdog(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let gone: Vec<String> = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, s)| s.client.is_some() && s.client_last.elapsed() > Duration::from_secs(15))
                .map(|(a, _)| a.clone())
                .collect();
            for area in gone {
                info!("session {}: no frames from the client for 15 s", short(&area));
                self.client_stop(&area, None);
            }
        }
    }

    // ------------------------------------------------------------ live gains

    /// Apply gains.json, an override of the config's gains, whenever it changes or goes away.
    pub async fn watch_gains(self: Arc<Self>) {
        let file = self.cfg.gains_file();
        let mut seen = Some(None);
        loop {
            let m = mtime(&file);
            if seen != Some(m) {
                seen = Some(m);
                let g = match m {
                    None => Ok(self.cfg.gains.clone()),
                    Some(_) => read_json::<Gains>(&file),
                };
                match g {
                    Ok(g) => {
                        info!(
                            "gains now {:?} ({})",
                            g,
                            if m.is_some() { "gains.json" } else { "config" }
                        );
                        *self.gains.write().unwrap() = g;
                    }
                    Err(e) => warn!("gains.json unreadable, keeping the gains: {e}"),
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}

pub fn write_atomic(p: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, p)
}

// ---------------------------------------------------------------- upstream

enum Msg {
    Frame(Vec<u8>),
    Close,
}

/// Our own entertainment stream to the real bridge for one area. Frames go through a small queue;
/// when it is full the newest frame is dropped, which a light never notices at 60 frames a second.
pub struct Upstream {
    tx: mpsc::Sender<Msg>,
    closed: AtomicBool,
}

impl Upstream {
    fn spawn(relay: Arc<Relay>, area: String) -> Arc<Upstream> {
        let (tx, rx) = mpsc::channel(8);
        let up = Arc::new(Upstream {
            tx,
            closed: AtomicBool::new(false),
        });
        tokio::spawn(upstream_task(relay, area, rx, up.clone()));
        up
    }

    pub fn send(&self, frame: Vec<u8>) {
        let _ = self.tx.try_send(Msg::Frame(frame));
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Close).await;
        });
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

async fn upstream_open(relay: &Relay, area: &str) -> Result<Arc<DTLSConn>> {
    info!(
        "upstream {}: start -> {}",
        short(area),
        relay.bridge.area_action(area, "start").await
    );
    crate::entertainment::connect(
        &relay.cfg.real_bridge,
        &relay.bridge.key.username,
        &relay.bridge.key.clientkey,
    )
    .await
}

async fn upstream_task(relay: Arc<Relay>, area: String, mut rx: mpsc::Receiver<Msg>, me: Arc<Upstream>) {
    let a = short(&area).to_string();
    let mut owned = false;
    let mut last: Option<Vec<u8>> = None;
    let mut last_fed = Instant::now() - Duration::from_secs(3600);
    let mut last_sent = Instant::now();
    let mut attempt = 0u32;
    'outer: while !me.is_closed() {
        attempt += 1;
        let conn = match upstream_open(&relay, &area).await {
            Ok(c) => {
                owned = true;
                info!("upstream {a}: streaming to the real bridge (attempt {attempt})");
                c
            }
            Err(e) => {
                warn!("upstream {a}: no handshake ({e}); is another app streaming to it?");
                tokio::time::sleep(Duration::from_secs(attempt.min(5) as u64)).await;
                continue;
            }
        };
        attempt = 0;
        loop {
            match timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Some(Msg::Frame(f))) => {
                    last_fed = Instant::now();
                    if let Err(e) = conn.send(&f).await {
                        warn!("upstream {a}: lost ({e}), reopening");
                        continue 'outer;
                    }
                    last_sent = Instant::now();
                    last = Some(f);
                }
                Ok(Some(Msg::Close)) | Ok(None) => {
                    let _ = conn.close().await;
                    break 'outer;
                }
                Err(_) => {
                    // A pause: the light keeps the last frame while the session lives, and the bridge
                    // ends a session after 10 s without frames. Repeat the last frame every 4 s,
                    // for up to 10 s after the last new one, so a vanished sender never freezes the light.
                    if me.is_closed() {
                        let _ = conn.close().await;
                        break 'outer;
                    }
                    if let Some(f) = &last
                        && last_sent.elapsed() >= Duration::from_secs(4)
                        && last_fed.elapsed() < Duration::from_secs(10)
                    {
                        if conn.send(f).await.is_err() {
                            warn!("upstream {a}: lost, reopening");
                            continue 'outer;
                        }
                        last_sent = Instant::now();
                    }
                }
            }
        }
    }
    // The bridge accepts a stop from any app, also for someone else's stream, so only stop our own.
    if owned {
        info!(
            "upstream {a}: stop -> {}",
            relay.bridge.area_action(&area, "stop").await
        );
    } else {
        info!("upstream {a}: never streamed, not sending stop");
    }
}
