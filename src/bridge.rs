//! Requests to the real bridge, with the relay's own application key.

use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::config::RealKey;

pub struct Bridge {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    pub host: String,
    pub key: RealKey,
}

impl Bridge {
    pub fn new(host: &str, key: RealKey) -> Result<Bridge> {
        // Bridges have certificates for their bridge id, signed by Signify's own CA.
        let build = |timeout: Option<Duration>| {
            let mut b = reqwest::Client::builder().tls_danger_accept_invalid_certs(true);
            if let Some(t) = timeout {
                b = b.timeout(t);
            }
            b.build()
        };
        Ok(Bridge {
            http: build(Some(Duration::from_secs(10)))?,
            stream_http: build(None)?,
            host: host.to_string(),
            key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.host, path)
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Bytes>,
        content_type: Option<&str>,
    ) -> Result<(StatusCode, Bytes)> {
        let method = Method::from_bytes(method.as_bytes()).context("method")?;
        let mut req = self
            .http
            .request(method, self.url(path))
            .header("hue-application-key", &self.key.username);
        if let Some(ct) = content_type {
            req = req.header("Content-Type", ct);
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        let resp = req.send().await?;
        Ok((resp.status(), resp.bytes().await?))
    }

    pub async fn get_json(&self, path: &str) -> Result<Value> {
        let (_, body) = self.request("GET", path, None, None).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Start or stop an entertainment area; returns the bridge's answer for the log.
    pub async fn area_action(&self, area: &str, action: &str) -> String {
        let body = Bytes::from(format!(r#"{{"action":"{action}"}}"#));
        match self
            .request(
                "PUT",
                &format!("/clip/v2/resource/entertainment_configuration/{area}"),
                Some(body),
                Some("application/json"),
            )
            .await
        {
            Ok((status, body)) if status.is_success() => String::from_utf8_lossy(&body).into_owned(),
            Ok((status, body)) => format!("HTTP {status}: {}", String::from_utf8_lossy(&body)),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Our own application id (what the bridge names as active_streamer for our streams).
    pub async fn app_id(&self) -> Option<String> {
        let resp = self
            .http
            .get(self.url("/auth/v1"))
            .header("hue-application-key", &self.key.username)
            .send()
            .await
            .ok()?;
        resp.headers()
            .get("hue-application-id")?
            .to_str()
            .ok()
            .map(str::to_string)
    }

    pub async fn eventstream(&self) -> Result<reqwest::Response> {
        Ok(self
            .stream_http
            .get(self.url("/eventstream/clip/v2"))
            .header("hue-application-key", &self.key.username)
            .header("Accept", "text/event-stream")
            .send()
            .await?)
    }
}
