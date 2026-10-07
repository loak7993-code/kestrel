// kestrel :: cdp/browser — launch or attach to any Chromium-family browser and
// hand out pages. Targets are created over the HTTP /json endpoints and each
// page owns its own WebSocket connection (see transport.rs).
use super::transport::CdpConn;
use crate::discovery::find_browser;
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct LaunchOpts {
    /// Path or name; defaults to $VELOX_BROWSER then discovery order.
    pub browser: Option<String>,
    pub headless: bool,
    pub proxy: Option<String>,
    pub args: Vec<String>,
    /// Connect timeout for the DevTools endpoint.
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectOpts {
    /// `ws://…` browser endpoint or `host:port` (http probe resolves the ws url).
    pub endpoint: String,
}

pub struct Browser {
    pub http: reqwest::Client,
    pub port: u16,
    pub ws_url: String,
    child: Option<tokio::process::Child>,
    conn: Option<Arc<CdpConn>>,
    closed: Arc<AtomicBool>,
}

impl Browser {
    /// Launch any installed Chromium-family browser with remote debugging on.
    pub async fn launch(opts: LaunchOpts) -> Result<Browser> {
        let exe = find_browser(opts.browser.as_deref())?;
        let is_shell = exe.contains("headless-shell");
        let root = nix_root();
        let dir = mkdtemp_profile();
        let mut cli = vec![
            format!("--user-data-dir={dir}"),
            "--remote-debugging-port=0".to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-sync".to_string(),
            "--mute-audio".to_string(),
            "--disable-component-update".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-extensions".to_string(),
            "--disable-hang-monitor".to_string(),
            "--disable-breakpad".to_string(),
            "--no-service-autorun".to_string(),
        ];
        if opts.headless {
            cli.push("--disable-gpu".to_string());
            if is_shell {
                cli.push("--headless=new".to_string());
            } else if looks_like_chrome(&exe) {
                // full chrome needs the explicit headless switch
                cli.push("--headless=new".to_string());
            }
        }
        if root || std::env::var("VELOX_NO_SANDBOX").is_ok() {
            cli.push("--no-sandbox".to_string());
            cli.push("--disable-dev-shm-usage".to_string());
        }
        if let Some(proxy) = &opts.proxy {
            cli.push(format!("--proxy-server={proxy}"));
        }
        for a in &opts.args {
            cli.push(a.clone());
        }

        let mut child = tokio::process::Command::new(&exe)
            .args(&cli)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("spawn {exe}: {e}"))?;

        // Chrome prints "DevTools listening on ws://127.0.0.1:<port>/devtools/browser/<id>"
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let wait = if opts.timeout < Duration::from_millis(1000) {
            Duration::from_secs(20)
        } else {
            opts.timeout
        };
        let ws_url = read_ws_url(stderr, wait).await?;
        let port = ws_url
            .split("127.0.0.1:")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .and_then(|s| s.split(':').next())
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| anyhow!("could not parse debug port from {ws_url}"))?;

        Ok(Browser {
            http: http_client(),
            port,
            ws_url,
            child: Some(child),
            conn: None,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Attach to a running browser: `ws://…` or `host:port`.
    pub async fn connect(opts: ConnectOpts) -> Result<Browser> {
        let endpoint = opts.endpoint.clone();
        let (host_port, ws_url) = if endpoint.starts_with("ws://") || endpoint.starts_with("wss://")
        {
            (None, endpoint)
        } else {
            let hp = endpoint
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches('/')
                .to_string();
            let ver: Value = reqwest::get(format!("http://{hp}/json/version"))
                .await?
                .json()
                .await?;
            let ws = ver
                .get("webSocketDebuggerUrl")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("no webSocketDebuggerUrl"))?
                .to_string();
            (Some(hp), ws)
        };
        let port = host_port
            .as_deref()
            .and_then(|hp| hp.split(':').next_back())
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(0);
        Ok(Browser {
            http: http_client(),
            port,
            ws_url,
            child: None,
            conn: None,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Open a new page (own WebSocket connection) and initialise it.
    pub async fn new_page(
        &mut self,
        opts: super::page::PageOpts,
    ) -> Result<Arc<super::page::Page>> {
        // PUT /json/new (Chrome 111+); fall back to GET for older builds.
        // With `initial_url` the target is created already navigating — chrome
        // starts the load during boot, in parallel with our attach.
        let target = {
            let first = opts
                .initial_url
                .clone()
                .unwrap_or_else(|| "about:blank".into());
            let url = format!("{}/json/new?{}", self.http_base(), urlencode(&first));
            let mut res = self
                .http
                .put(&url)
                .send()
                .await
                .map_err(|e| anyhow!("/json/new: {e}"))?;
            if !res.status().is_success() {
                res = self
                    .http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| anyhow!("/json/new: {e}"))?;
            }
            let v: Value = res
                .json()
                .await
                .map_err(|e| anyhow!("/json/new json: {e}"))?;
            v
        };
        let ws = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("target has no webSocketDebuggerUrl: {target}"))?
            .to_string();
        let id = target
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let page = super::page::Page::connect(ws, id, opts).await?;
        Ok(page)
    }

    /// Existing targets (debuggability: list pages/frames/iframes).
    pub async fn targets(&self) -> Result<Vec<Value>> {
        let v: Value = self
            .http
            .get(format!("{}/json/list", self.http_base()))
            .send()
            .await?
            .json()
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Raw HTTP body fetch from the devtools http endpoint.
    pub async fn http_json(&self, path: &str) -> Result<Value> {
        Ok(self
            .http
            .get(format!("{}{}", self.http_base(), path))
            .send()
            .await?
            .json()
            .await?)
    }

    /// Browser-level CDP connection (Target.*, Browser.* domains) — created lazily.
    pub async fn conn(&mut self) -> Result<Arc<CdpConn>> {
        if self.conn.is_none() {
            self.conn = Some(CdpConn::connect(&self.ws_url).await?);
        }
        Ok(self.conn.clone().unwrap())
    }

    pub async fn close(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(conn) = self.conn.take() {
            conn.close().await;
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // best-effort: drop() is sync — kill_on_drop on the tokio child covers us,
        // and close() does the polite path when awaited.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client")
}

fn nix_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn looks_like_chrome(exe: &str) -> bool {
    // chrome-headless-shell always runs headless; full browsers need the switch
    !exe.contains("headless-shell")
}

/// Query-encode for /json/new?url=… (chrome wants the raw url here).
fn urlencode(u: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("x", u)
        .finish()
        .trim_start_matches("x=")
        .to_string()
}

fn mkdtemp_profile() -> String {
    let d = std::env::temp_dir().join(format!("kestrel-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.to_string_lossy().to_string()
}

/// Read the "DevTools listening on ws://…" line from chrome's stderr.
async fn read_ws_url(stderr: tokio::process::ChildStderr, timeout: Duration) -> Result<String> {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut line = String::new();
    let deadline = Instant::now() + timeout;
    loop {
        let n = tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await;
        match n {
            Ok(Ok(0)) => break,
            Ok(Ok(_)) => {
                if let Some(i) = line.find("DevTools listening on ") {
                    let url = line[i + "DevTools listening on ".len()..]
                        .trim()
                        .to_string();
                    if url.starts_with("ws://") {
                        return Ok(url);
                    }
                }
                line.clear();
            }
            Ok(Err(_)) => break,
            Err(_) => {
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    Err(anyhow!(
        "browser did not report a DevTools endpoint within {:?}",
        timeout
    ))
}
