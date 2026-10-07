// kestrel :: http — the no-browser engine: keep-alive HTTP with cookies,
// compression, the needsJS escalation heuristic, and the HtmlDoc over scraper.
pub mod html;

use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::time::Duration;

pub use html::{HtmlDoc, Link};

#[derive(Debug, Clone)]
pub struct LiteResponse {
    pub url: String,
    pub start_url: String,
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: bytes::Bytes,
    pub ms: u128,
}

impl LiteResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
    pub fn doc(&self) -> HtmlDoc {
        HtmlDoc::parse(&self.text(), &self.url)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct LiteOpts {
    pub timeout: Duration,
    pub headers: Vec<(String, String)>,
    pub user_agent: Option<String>,
    pub proxy: Option<String>,
    pub max_redirects: usize,
}

impl Default for TimeoutDefault {
    fn default() -> Self {
        TimeoutDefault
    }
}
pub struct TimeoutDefault;

pub fn default_opts() -> LiteOpts {
    LiteOpts {
        timeout: Duration::from_secs(20),
        headers: vec![],
        user_agent: None,
        proxy: None,
        max_redirects: 10,
    }
}

/// Fetch one URL. Cookie state lives in the returned response's jar handle
/// (a reqwest Client reused by the caller keeps the keep-alive pool warm).
pub async fn fetch(client: &reqwest::Client, url: &str, opts: &LiteOpts) -> Result<LiteResponse> {
    let start = std::time::Instant::now();
    let start_url = url.to_string();
    let mut req = client.get(url).timeout(opts.timeout);
    if let Some(ua) = &opts.user_agent {
        req = req.header(reqwest::header::USER_AGENT, ua);
    }
    for (k, v) in &opts.headers {
        req = req.header(k, v);
    }
    let res = req.send().await.map_err(|e| anyhow!("{url}: {e}"))?;
    let final_url = res.url().to_string();
    let status = res.status().as_u16();
    let mut headers = std::collections::HashMap::new();
    for (name, value) in res.headers().iter() {
        headers.insert(
            name.as_str().to_lowercase(),
            value.to_str().unwrap_or("").to_string(),
        );
    }
    let body = res.bytes().await.map_err(|e| anyhow!("{url}: body {e}"))?;
    Ok(LiteResponse {
        url: final_url,
        start_url,
        status,
        headers,
        body,
        ms: start.elapsed().as_millis(),
    })
}

/// Shared client builder (keep-alive pool + compression + optional proxy).
pub fn build_client(opts: &LiteOpts) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(opts.user_agent.clone().unwrap_or_else(|| {
            "Mozilla/5.0 (compatible; Kestrel/2.7; +https://kestrel.dev)".to_string()
        }))
        .timeout(opts.timeout)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(
            opts.max_redirects.max(1),
        ));
    if let Some(proxy) = &opts.proxy {
        b = b.proxy(reqwest::Proxy::all(proxy).map_err(|e| anyhow!("proxy: {e}"))?);
    }
    Ok(b.build()?)
}

/// Arc'd client for pooling across many fetches.
pub fn shared_client(opts: &LiteOpts) -> Result<Arc<reqwest::Client>> {
    Ok(Arc::new(build_client(opts)?))
}

pub use crate::needs_js::needs_js;
