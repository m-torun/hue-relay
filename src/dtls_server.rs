//! The entertainment stream from clients: DTLS 1.2 with a pre-shared key on UDP 2100.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::time::timeout;
use tracing::{info, warn};
use webrtc_dtls::cipher_suite::CipherSuiteId;
use webrtc_dtls::conn::DTLSConn;
use webrtc_util::Conn;
use webrtc_util::conn::Listener;
use webrtc_util::conn::conn_udp_listener::ListenConfig;

use crate::relay::Relay;
use crate::stream::{self, Frame};

fn server_config(relay: Arc<Relay>) -> webrtc_dtls::config::Config {
    webrtc_dtls::config::Config {
        // The client's PSK identity: its username or the application id from /auth/v1. Looked up on
        // each handshake, so new pairings work at once.
        psk: Some(Arc::new(move |identity: &[u8]| {
            relay.psk_for(identity).ok_or_else(|| {
                webrtc_dtls::Error::Other(format!("unknown PSK identity {:?}", String::from_utf8_lossy(identity)))
            })
        })),
        cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256],
        ..Default::default()
    }
}

pub async fn serve(relay: Arc<Relay>) -> Result<()> {
    let mut lc = ListenConfig {
        // Only a handshake record from an unknown peer opens a connection.
        accept_filter: Some(Box::new(|packet: &[u8]| {
            let handshake = packet.len() > 13 && packet[0] == 22;
            Box::pin(async move { handshake })
        })),
        ..Default::default()
    };
    let listener = lc.listen((relay.cfg.relay_ip, 2100)).await?;
    info!("dtls: listening on {}:2100", relay.cfg.relay_ip);
    loop {
        let (conn, peer) = listener.accept().await?;
        let relay = relay.clone();
        tokio::spawn(async move {
            match timeout(
                Duration::from_secs(10),
                DTLSConn::new(conn, server_config(relay.clone()), false, None),
            )
            .await
            {
                Ok(Ok(dtls)) => {
                    info!("dtls {}: handshake done", peer.ip());
                    serve_client(relay, dtls, peer).await;
                }
                Ok(Err(e)) => warn!("dtls {}: handshake failed: {e}", peer.ip()),
                Err(_) => warn!("dtls {}: handshake timed out", peer.ip()),
            }
        });
    }
}

/// One client's stream. Bad or stale records are dropped, as DTLS does; the connection ends after 10 s
/// without a valid frame, on errors with no valid frame for 1 s (the client reconnecting from the same
/// port), or when a newer connection takes over its area.
async fn serve_client(relay: Arc<Relay>, dtls: DTLSConn, peer: SocketAddr) {
    let me = relay.conn_ids.fetch_add(1, Ordering::SeqCst);
    let ip = peer.ip();
    let t0 = Instant::now();
    let (mut area, mut last_valid, mut errors, mut frames) = (None::<String>, Instant::now(), 0u64, 0u64);
    let (mut last_report, mut last_sample, mut described, mut warned_other) =
        (t0, t0 - Duration::from_secs(10), false, false);
    let mut buf = vec![0u8; 4096];
    loop {
        if let Some(a) = &area
            && relay.active_conn.lock().unwrap().get(a) != Some(&me)
        {
            info!("dtls {ip}: superseded by a newer connection");
            area = None;
            break;
        }
        if last_valid.elapsed() > Duration::from_secs(10) {
            info!("dtls {ip}: no valid frames for 10 s, closing");
            break;
        }
        let n = match timeout(Duration::from_secs(1), dtls.recv(&mut buf)).await {
            Err(_) => continue,
            Ok(Err(e)) => {
                errors += 1;
                if errors <= 3 || errors % 500 == 0 {
                    info!("dtls {ip}: dropped a record ({e}), {errors} so far");
                }
                if errors >= 3 && last_valid.elapsed() > Duration::from_secs(1) {
                    info!("dtls {ip}: errors and no valid frames for 1 s, closing");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Ok(Ok(n)) => n,
        };
        let gains = relay.gains.read().unwrap().clone();
        let data = &buf[..n];
        match stream::apply_gains(data, &gains) {
            Frame::Invalid => continue,
            Frame::Other(_) => {
                if !warned_other {
                    warn!("dtls {ip}: frames of another protocol version, ignored");
                    warned_other = true;
                }
                continue;
            }
            Frame::V2 { area: a, xyb, out } => {
                last_valid = Instant::now();
                if area.as_deref() != Some(a.as_str()) {
                    relay.active_conn.lock().unwrap().insert(a.clone(), me);
                    area = Some(a.clone());
                }
                if !described {
                    info!(
                        "stream: v2, colour space {}, area {}, {} channels, first frame {:?}",
                        if xyb { "XYB" } else { "RGB" },
                        &a[..8.min(a.len())],
                        stream::channels(&out).len(),
                        stream::channels(data)
                    );
                    described = true;
                }
                if last_sample.elapsed() > Duration::from_secs(5) {
                    let pick = |p: &[u8]| stream::channels(p).into_iter().map(|(_, v)| v).collect::<Vec<_>>();
                    info!("dtls {ip}: sample in {:?} out {:?}", pick(data), pick(&out));
                    last_sample = Instant::now();
                }
                relay.client_frame(&a, out);
                frames += 1;
                if last_report.elapsed() > Duration::from_secs(60) {
                    info!(
                        "dtls {ip}: {frames} frames, {:.1} fps",
                        frames as f64 / t0.elapsed().as_secs_f64()
                    );
                    last_report = Instant::now();
                }
            }
        }
    }
    let _ = dtls.close().await;
    if let Some(a) = area {
        let mine = {
            let mut ac = relay.active_conn.lock().unwrap();
            let mine = ac.get(&a) == Some(&me);
            if mine {
                ac.remove(&a);
            }
            mine
        };
        if mine {
            relay.client_stop(&a, Some(t0)); // a start after this connection began is not ours
        }
    }
}
