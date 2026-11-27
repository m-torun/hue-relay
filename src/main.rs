// hue-relay: per-segment brightness for Philips Hue gradient lights.
// Copyright (C) 2026 the hue-relay authors.
//
// This program is free software: you can redistribute it and/or modify it under the terms of the
// GNU Affero General Public License as published by the Free Software Foundation, either version 3
// of the License, or (at your option) any later version. It is distributed WITHOUT ANY WARRANTY;
// see the GNU Affero General Public License for details. You should have received a copy of it
// with this program; if not, see <https://www.gnu.org/licenses/>.

//! hue-relay: a stand-in Hue bridge for Hue Sync clients.
//!
//! A client pairs with the relay as if it were a bridge. The relay shows it the real bridge's
//! resources under its own identity, takes the client's entertainment stream, scales each channel's
//! brightness by a per-area gain, and streams the result on to the real bridge with its own key.

mod bridge;
mod cert;
mod config;
mod discovery;
mod dtls_server;
mod entertainment;
mod http;
mod lights;
mod relay;
mod rewrite;
mod standby;
mod stream;
mod tools;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

use crate::config::{Config, RealKey};
use crate::relay::Relay;

#[derive(Parser)]
#[command(
    version,
    about = "A stand-in Hue bridge that applies per-segment brightness to Hue Sync streams"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the relay (the default).
    Run {
        #[arg(long, env = "RELAY_CONFIG", default_value = "/app/config.json")]
        config: PathBuf,
    },
    /// Get an application key with a streaming key from a bridge (press its link button) and print it.
    PairBridge {
        /// The bridge's address.
        bridge: String,
        /// Seconds to wait for the button.
        #[arg(long, default_value_t = 600)]
        wait: u64,
        #[arg(long, default_value = "hue-relay#relay")]
        devicetype: String,
    },
    /// List the bridge's entertainment areas with their channels, to write the gains.
    ListAreas {
        /// The bridge's address.
        #[arg(long)]
        bridge: String,
        /// The relay's key file ({"username", "clientkey"}).
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Stream a test pattern to an entertainment area of a bridge or a relay.
    StreamTest {
        /// Address of the bridge or relay.
        #[arg(long)]
        bridge: String,
        /// {"username", "clientkey"} for it; created by pairing if missing.
        #[arg(long, default_value = "stream-test-key.json")]
        key_file: PathBuf,
        /// Entertainment area name or id.
        #[arg(long)]
        area: String,
        /// Brightness 0-1 before the per-channel levels.
        #[arg(long, default_value_t = 0.75)]
        brightness: f64,
        /// Colour temperature.
        #[arg(long, default_value_t = 366.0)]
        mirek: f64,
        /// Per-channel levels, by channel id, e.g. 0.4,0.3,0.5 (missing ones are 1).
        #[arg(long, value_delimiter = ',')]
        levels: Vec<f64>,
        /// Red, green, blue, then white, 4 s each, instead of a steady pattern.
        #[arg(long)]
        colors: bool,
        #[arg(long, default_value_t = 15.0)]
        seconds: f64,
        /// Seconds between frames.
        #[arg(long, default_value_t = 0.04)]
        interval: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,webrtc_dtls=error".into()),
        )
        .with_target(false)
        .init();

    // webrtc-dtls brings rustls's ring backend, the HTTPS crates aws-lc-rs: rustls then needs to be told.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    match Cli::parse().cmd {
        None => {
            let config = std::env::var("RELAY_CONFIG").unwrap_or_else(|_| "/app/config.json".into());
            run(config.into()).await
        }
        Some(Cmd::Run { config }) => run(config).await,
        Some(Cmd::PairBridge {
            bridge,
            wait,
            devicetype,
        }) => tools::pair_bridge(&bridge, wait, &devicetype).await,
        Some(Cmd::ListAreas { bridge, key_file }) => tools::list_areas(&bridge, &key_file).await,
        Some(Cmd::StreamTest {
            bridge,
            key_file,
            area,
            brightness,
            mirek,
            levels,
            colors,
            seconds,
            interval,
        }) => {
            tools::stream_test(tools::StreamTest {
                bridge,
                key_file,
                area,
                brightness,
                mirek,
                levels,
                colors,
                seconds,
                interval,
            })
            .await
        }
    }
}

async fn run(config: PathBuf) -> Result<()> {
    let path = config.display().to_string();
    let cfg: Config = serde_json::from_slice(&std::fs::read(&config).with_context(|| format!("reading {path}"))?)
        .with_context(|| format!("parsing {path}"))?;
    let key: RealKey = serde_json::from_slice(&std::fs::read(&cfg.key_file).context("reading key_file")?)
        .context("parsing key_file")?;
    std::fs::create_dir_all(&cfg.data_dir)?;
    let relay = Arc::new(Relay::new(cfg, key)?);

    // The real bridge's public config and our application id there; wait for the bridge if need be.
    loop {
        match relay.bridge.get_json("/api/config").await {
            Ok(v) => {
                *relay.real_public.write().unwrap() = v;
                break;
            }
            Err(e) => {
                warn!("real bridge {} not answering ({e}); retrying", relay.cfg.real_bridge);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    *relay.relay_app_id.write().unwrap() = relay.bridge.app_id().await;
    {
        let real = relay.real_public.read().unwrap();
        info!(
            "relay {} (bridge id {}) in front of {} ({}, sw {})",
            relay.cfg.relay_ip,
            relay.id.bridge_id,
            relay.cfg.real_bridge,
            real["bridgeid"].as_str().unwrap_or("?"),
            real["swversion"].as_str().unwrap_or("?")
        );
    }
    info!(
        "our application id on the real bridge: {:?}",
        relay.relay_app_id.read().unwrap()
    );
    info!(
        "gains: {:?}; outside sync: {:?}; pairing {}; {} paired clients",
        relay.cfg.gains,
        relay.cfg.outside_sync,
        if relay.cfg.link_open { "OPEN" } else { "closed" },
        relay.clients.read().unwrap().len()
    );

    let (cert, key) = cert::ensure(&relay.cfg.data_dir, &relay.id.bridge_id)?;
    let tls = RustlsConfig::from_pem_file(cert, key)
        .await
        .context("loading the certificate")?;

    let http_app = http::router(relay.clone(), false).into_make_service_with_connect_info::<SocketAddr>();
    let https_app = http::router(relay.clone(), true).into_make_service_with_connect_info::<SocketAddr>();
    // Only on the relay's own address: on a shared machine, other services keep their ports elsewhere.
    let ip = relay.cfg.relay_ip;
    let http = tokio::spawn(axum_server::bind(SocketAddr::from((ip, 80))).serve(http_app));
    let https = tokio::spawn(axum_server::bind_rustls(SocketAddr::from((ip, 443)), tls).serve(https_app));
    let dtls = tokio::spawn(dtls_server::serve(relay.clone()));
    tokio::spawn(relay.clone().watch_gains());
    tokio::spawn(relay.clone().watchdog());
    if !relay.cfg.outside_sync.is_empty() {
        tokio::spawn(standby::watch(relay.clone()));
        tokio::spawn(standby::render(relay.clone()));
    }
    discovery::ssdp(relay.clone())?;
    let _mdns = discovery::mdns(&relay)?;

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = http => error!("http server ended: {r:?}"),
        r = https => error!("https server ended: {r:?}"),
        r = dtls => error!("dtls server ended: {r:?}"),
        _ = term.recv() => {
            info!("stopping");
            standby::shutdown(&relay).await;
            std::process::exit(0);
        }
        _ = tokio::signal::ctrl_c() => {
            info!("stopping");
            standby::shutdown(&relay).await;
            std::process::exit(0);
        }
    }
    std::process::exit(1);
}
