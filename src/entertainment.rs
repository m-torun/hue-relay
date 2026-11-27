//! Talking to a bridge as an entertainment client: start and stop an area, and open the DTLS stream.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::net::UdpSocket;
use tokio::time::timeout;
use webrtc_dtls::cipher_suite::CipherSuiteId;
use webrtc_dtls::conn::DTLSConn;

/// Open the entertainment stream (UDP 2100, DTLS 1.2, PSK) to a bridge whose area is already started.
pub async fn connect(host: &str, username: &str, clientkey_hex: &str) -> Result<Arc<DTLSConn>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect((host, 2100)).await?;
    let psk = hex::decode(clientkey_hex)?;
    let cfg = webrtc_dtls::config::Config {
        psk: Some(Arc::new(move |_hint: &[u8]| Ok(psk.clone()))),
        psk_identity_hint: Some(username.as_bytes().to_vec()),
        cipher_suites: vec![CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256],
        server_name: host.to_string(),
        insecure_skip_verify: true,
        ..Default::default()
    };
    let conn = timeout(Duration::from_secs(5), DTLSConn::new(Arc::new(sock), cfg, true, None)).await??;
    Ok(Arc::new(conn))
}
