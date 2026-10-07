// kestrel :: cdp/page — one page = one WebSocket.
/// kestrel's in-page engine: one evaluate wires `window.__ks` with the selector
/// primitives every framework call needs (css / id= / text= matching, point,
/// focus, wait). Registered once via addScriptToEvaluateOnNewDocument.
pub const KESTREL_ENGINE: &str = r#"(function () {
  if (window.__ks) return;
  var K = { visible: function (el) {
    if (!el || !el.isConnected) return false;
    var st = getComputedStyle(el);
    if (st.display === 'none' || st.visibility === 'hidden' || st.opacity === '0') return false;
    var r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  } };
  function match(sel, root) {
    root = root || document;
    if (sel.startsWith('id=')) return Array.prototype.slice.call(root.querySelectorAll('#' + CSS.escape(sel.slice(3))));
    if (sel.startsWith('tag=')) return Array.prototype.slice.call(root.getElementsByTagName(sel.slice(4)));
    if (sel.startsWith('text=')) {
      var want = sel.slice(5).toLowerCase();
      return Array.prototype.filter.call(root.querySelectorAll('*'), function (el) {
        return el.children.length === 0 && (el.textContent || '').trim().toLowerCase().includes(want);
      });
    }
    try { return Array.prototype.slice.call(root.querySelectorAll(sel)); } catch (e) { return []; }
  }
  K.match = match;
  K.one = function (sel, root) { var m = match(sel, root); return m.length ? m[0] : null; };
  K.count = function (sel, root) { return match(sel, root).length; };
  K.text = function (sel, root) { var el = K.one(sel, root); return el ? (el.textContent || '').trim() : null; };
  K.attr = function (sel, name, root) { var el = K.one(sel, root); return el ? el.getAttribute(name) : null; };
  K.rect = function (sel, root) {
    var el = K.one(sel, root); if (!el) return null;
    var r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  };
  K.point = function (sel, root) {
    var m = match(sel, root);
    for (var i = 0; i < m.length; i++) {
      if (!K.visible(m[i])) continue;
      var r = m[i].getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      if (r.top < 0 || r.bottom > innerHeight || r.left < 0 || r.right > innerWidth) {
        m[i].scrollIntoView({ block: 'center', behavior: 'instant' });
        r = m[i].getBoundingClientRect();
      }
      return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
    }
    return null;
  };
  K.focus = function (sel, root) { var el = K.one(sel, root); if (el && el.focus) el.focus(); return !!el; };
  K.extract = function (sel, spec, root) {
    spec = spec || {};
    var out = [];
    var m = match(sel, root);
    for (var i = 0; i < m.length; i++) {
      var el = m[i];
      var o = {};
      if (spec.text !== false || spec.text) o.text = (el.textContent || '').trim();
      if (spec.html) o.html = el.innerHTML;
      if (spec.tag) o.tag = el.tagName.toLowerCase();
      if (spec.attrs) for (var a = 0; a < spec.attrs.length; a++) o[spec.attrs[a]] = el.getAttribute(spec.attrs[a]);
      if (spec.limit && out.length >= spec.limit) break;
      out.push(o);
    }
    return out;
  };
  K.wait = function (sel, timeout, state, root) {
    state = state || 'visible';
    return new Promise(function (resolve) {
      var ok = function () {
        var el = K.one(sel, root);
        if (!el) return false;
        if (state === 'attached') return true;
        if (state === 'hidden') return !K.visible(el);
        return K.visible(el);
      };
      if (ok()) return resolve(true);
      var done = false, t0 = Date.now();
      var finish = function (v) { if (done) return; done = true; mo.disconnect(); clearInterval(poll); resolve(v === true); };
      var mo = new MutationObserver(function () { if (ok()) finish(true); });
      try { mo.observe(document.documentElement || document, { childList: true, subtree: true, attributes: true }); } catch (e) {}
      var poll = setInterval(function () { if (ok()) return finish(true); if (Date.now() - t0 > (timeout || 10000)) finish(false); }, 60);
    });
  };
  K.waitExpr = function (expr, timeout) {
    return new Promise(function (resolve, reject) {
      var t0 = Date.now();
      (function check() {
        var v;
        try { v = Function('"use strict"; return (' + expr + ');')(); } catch (e) { return reject(new Error(e.message)); }
        if (v) return resolve(v);
        if (Date.now() - t0 > (timeout || 10000)) return reject(new Error('waitExpr timeout: ' + expr));
        setTimeout(check, 50);
      })();
    });
  };
  window.__ks = K;
})();"#;

use super::{b64_decode, transport::CdpConn};
use crate::devices;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    None,
    /// DOMContentLoaded (kestrel's default "interactive")
    Interactive,
    Load,
    NetworkIdle,
}

impl WaitUntil {
    pub fn parse(s: &str) -> Self {
        match s {
            "none" => WaitUntil::None,
            "load" => WaitUntil::Load,
            "networkidle" => WaitUntil::NetworkIdle,
            _ => WaitUntil::Interactive,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GotoOpts {
    pub wait_until: Option<WaitUntil>,
    pub timeout: Option<Duration>,
    pub referer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Nav {
    pub url: String,
    pub status: Option<u16>,
    pub ms: u128,
    /// present on goto_with_retry results
    pub attempts: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct PageOpts {
    /// When set, the target is created already navigating to this URL —
    /// chrome starts loading during process boot, in parallel with our attach.
    pub initial_url: Option<String>,
    pub viewport: Option<(u32, u32, f64)>, // w, h, dsf
    pub ua: Option<String>,
    pub device: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub block_urls: Vec<String>,
    pub intercept: bool, // arm Network capture for requests()/bodies
    /// Coherent fingerprint hardening (see cdp::stealth).
    pub stealth: Option<super::stealth::StealthOpts>,
    /// How to answer dialogs: true → auto-accept (default), or prompt text.
    pub dialog_action: Option<DialogAction>,
}

#[derive(Debug, Clone)]
pub enum DialogAction {
    Accept,
    Dismiss,
    Prompt(String),
}

impl Default for DialogActionDefault {
    fn default() -> Self {
        DialogActionDefault
    }
}
pub struct DialogActionDefault;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Cookie {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "value")]
    pub value: String,
    #[serde(rename = "domain")]
    pub domain: String,
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "expires", skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    #[serde(rename = "secure")]
    pub secure: bool,
    #[serde(rename = "sameSite", skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RequestEntry {
    pub id: String,
    pub url: String,
    pub method: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub failed: Option<String>,
    pub resource_type: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleMessage {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

pub struct Page {
    pub conn: Arc<CdpConn>,
    pub target_id: String,
    pub opts: PageOpts,
    url: Mutex<String>,
    main_frame: Mutex<Option<String>>,
    requests: Mutex<HashMap<String, RequestEntry>>,
    console: Mutex<Vec<ConsoleMessage>>,
    errors: Mutex<Vec<ConsoleMessage>>,
    inflight: Mutex<i64>,
    last_doc_status: Mutex<Option<u16>>,
    init_done: AtomicU64,
    dialogs: Mutex<Vec<DialogEvent>>,
    dialog_action: Mutex<DialogAction>,
    stealth: Mutex<Option<super::stealth::Resolved>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DialogEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(rename = "defaultPrompt")]
    pub default_prompt: Option<String>,
}

impl Page {
    pub async fn connect(ws_url: String, target_id: String, opts: PageOpts) -> Result<Arc<Page>> {
        let conn = CdpConn::connect(&ws_url).await?;
        let page = Arc::new(Page {
            conn,
            target_id: target_id.clone(),
            opts,
            url: Mutex::new("about:blank".to_string()),
            main_frame: Mutex::new(None),
            requests: Mutex::new(HashMap::new()),
            console: Mutex::new(vec![]),
            errors: Mutex::new(vec![]),
            inflight: Mutex::new(0),
            last_doc_status: Mutex::new(None),
            init_done: AtomicU64::new(0),
            dialogs: Mutex::new(vec![]),
            dialog_action: Mutex::new(DialogAction::Accept),
            stealth: Mutex::new(None),
        });
        page.init().await?;
        Ok(page)
    }

    async fn init(self: &Arc<Self>) -> Result<()> {
        if self.init_done.swap(1, Ordering::SeqCst) == 1 {
            return Ok(());
        }
        let s = self.conn.clone();
        s.fire("Page.enable", json!({}));
        s.fire("Runtime.enable", json!({}));
        s.fire("Page.setLifecycleEventsEnabled", json!({ "enabled": true }));
        if self.opts.intercept || !self.opts.block_urls.is_empty() {
            s.fire("Network.enable", json!({ "maxPostDataSize": 65536 }));
        }
        if let Some(action) = self.opts.dialog_action.clone() {
            *self.dialog_action.lock().unwrap() = action;
        }
        self.wire_events().await?;
        self.apply_environment().await?;
        // the engine registers for every FUTURE document in one call — the old
        // path evaluated it on about:blank AND again after each navigation
        // (measured: ~10ms of V8 parse + a wire round-trip per goto, gone)
        let scripts: Vec<String> = vec![KESTREL_ENGINE.to_string()];
        // stealth: resolve → align with the real binary → env overrides + injected source
        if let Some(cfg) = self.opts.stealth.clone() {
            let version = s
                .send("Browser.getVersion", json!({}))
                .await
                .ok()
                .and_then(|v| {
                    v.get("product")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                });
            let resolved = super::stealth::resolve(&cfg)?;
            let aligned = super::stealth::align_version(resolved, version.as_deref());
            let _ = s
                .send(
                    "Network.setUserAgentOverride",
                    super::stealth::env_overrides(&aligned),
                )
                .await;
            let _ = s
                .send(
                    "Emulation.setLocaleOverride",
                    json!({ "locale": aligned.locale }),
                )
                .await;
            let _ = s
                .send(
                    "Emulation.setTimezoneOverride",
                    json!({ "timezoneId": aligned.timezone }),
                )
                .await;
            if aligned.profile.mobile {
                let _ = s
                    .send(
                        "Emulation.setTouchEmulationEnabled",
                        json!({ "enabled": true, "maxTouchPoints": aligned.profile.max_touch_points.max(5) }),
                    )
                    .await;
            }
            let _ = s
                .send(
                    "Network.setExtraHTTPHeaders",
                    json!({ "headers": { "accept-language": aligned.accept_language } }),
                )
                .await;
            let source = super::stealth::build_source(&aligned);
            let _ = s
                .send(
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": source }),
                )
                .await;
            let _ = s
                .send("Runtime.evaluate", json!({ "expression": source }))
                .await;
            *self.stealth.lock().unwrap() = Some(aligned);
        }
        for script in &scripts {
            let _ = s
                .send(
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": script }),
                )
                .await;
        }
        for script in &scripts {
            let _ = s
                .send("Runtime.evaluate", json!({ "expression": script }))
                .await;
        }
        Ok(())
    }

    async fn wire_events(self: &Arc<Self>) -> Result<()> {
        let page = self.clone();
        let mut rx = self.conn.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => page.handle_event(ev),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        Ok(())
    }

    fn handle_event(self: &Arc<Self>, ev: Value) {
        let method = ev.get("method").and_then(Value::as_str).unwrap_or("");
        let params = ev.get("params").cloned().unwrap_or(json!({}));
        match method {
            "Page.frameNavigated" => {
                if params
                    .get("frame")
                    .and_then(|f| f.get("parentId"))
                    .is_none()
                {
                    let url = params["frame"]["url"].as_str().unwrap_or("").to_string();
                    *self.url.lock().unwrap() = url;
                    *self.main_frame.lock().unwrap() =
                        params["frame"]["id"].as_str().map(|s| s.to_string());
                }
            }
            "Runtime.consoleAPICalled" => {
                let kind = params
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("log")
                    .to_string();
                let text = params["args"]
                    .as_array()
                    .map(|args| {
                        args.iter()
                            .map(|a| {
                                a.get("value")
                                    .map(|v| match v {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .or_else(|| {
                                        a.get("description")
                                            .and_then(Value::as_str)
                                            .map(|s| s.to_string())
                                    })
                                    .unwrap_or_default()
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let mut buf = self.console.lock().unwrap();
                buf.push(ConsoleMessage { kind, text });
                if buf.len() > 1000 {
                    buf.remove(0);
                }
            }
            "Runtime.exceptionThrown" => {
                let d = &params["exceptionDetails"];
                let text = d["exception"]["description"]
                    .as_str()
                    .or_else(|| d["text"].as_str())
                    .unwrap_or("error")
                    .to_string();
                self.errors.lock().unwrap().push(ConsoleMessage {
                    kind: "error".to_string(),
                    text,
                });
            }
            "Page.javascriptDialogOpening" => {
                let kind = params["type"].as_str().unwrap_or("alert").to_string();
                let message = params["message"].as_str().unwrap_or("").to_string();
                let default_prompt = params["defaultPrompt"].as_str().map(|s| s.to_string());
                let action = self.dialog_action.lock().unwrap().clone();
                let (response, text) = match action {
                    DialogAction::Accept => ("Accept", None),
                    DialogAction::Dismiss => ("Dismiss", None),
                    DialogAction::Prompt(t) => ("Accept", Some(t)),
                };
                self.conn.fire(
                    "Page.handleJavaScriptDialog",
                    json!({ "action": response, "promptText": text }),
                );
                self.dialogs.lock().unwrap().push(DialogEvent {
                    kind,
                    message,
                    default_prompt,
                });
            }
            "Network.requestWillBeSent" => {
                let id = params["requestId"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    return;
                }
                *self.inflight.lock().unwrap() += 1;
                let entry = RequestEntry {
                    id: id.clone(),
                    url: params["request"]["url"].as_str().unwrap_or("").to_string(),
                    method: params["request"]["method"]
                        .as_str()
                        .unwrap_or("GET")
                        .to_string(),
                    status: None,
                    content_type: None,
                    failed: None,
                    resource_type: params
                        .get("type")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string()),
                };
                self.requests.lock().unwrap().insert(id, entry);
            }
            "Network.responseReceived" => {
                let id = params["requestId"].as_str().unwrap_or("").to_string();
                let status = params["response"]["status"].as_u64().map(|s| s as u16);
                let ctype = {
                    let headers = &params["response"]["headers"];
                    headers
                        .get("content-type")
                        .or_else(|| headers.get("Content-Type"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                };
                if let Some(entry) = self.requests.lock().unwrap().get_mut(&id) {
                    entry.status = status;
                    entry.content_type = ctype;
                }
                if params["type"].as_str() == Some("Document") {
                    *self.last_doc_status.lock().unwrap() = status;
                }
            }
            "Network.loadingFinished" => {
                let mut inflight = self.inflight.lock().unwrap();
                *inflight -= 1;
                if *inflight < 0 {
                    *inflight = 0;
                }
            }
            "Network.loadingFailed" => {
                let id = params["requestId"].as_str().unwrap_or("").to_string();
                let mut inflight = self.inflight.lock().unwrap();
                *inflight -= 1;
                if *inflight < 0 {
                    *inflight = 0;
                }
                if let Some(entry) = self.requests.lock().unwrap().get_mut(&id) {
                    entry.failed =
                        Some(params["errorText"].as_str().unwrap_or("failed").to_string());
                }
            }
            _ => {}
        }
    }

    async fn apply_environment(self: &Arc<Self>) -> Result<()> {
        let s = self.conn.clone();
        if let Some(name) = self.opts.device.clone() {
            if let Some(d) = devices::get(&name) {
                let _ = s
                    .send("Emulation.setDeviceMetricsOverride", devices::metrics(d))
                    .await?;
                let _ = s
                    .send(
                        "Network.setUserAgentOverride",
                        json!({
                            "userAgent": d.ua,
                            "platform": d.platform,
                            "userAgentMetadata": {
                                "mobile": d.mobile,
                                "platform": d.platform,
                            }
                        }),
                    )
                    .await?;
            }
        } else {
            if let Some((w, h, dsf)) = self.opts.viewport {
                let _ = s
                    .send(
                        "Emulation.setDeviceMetricsOverride",
                        json!({ "width": w, "height": h, "deviceScaleFactor": dsf, "mobile": false }),
                    )
                    .await?;
            }
            if let Some(ua) = self.opts.ua.clone() {
                let _ = s
                    .send("Network.setUserAgentOverride", json!({ "userAgent": ua }))
                    .await?;
            }
        }
        if let Some(locale) = self.opts.locale.clone() {
            let _ = s
                .send("Emulation.setLocaleOverride", json!({ "locale": locale }))
                .await?;
        }
        if let Some(tz) = self.opts.timezone.clone() {
            let _ = s
                .send("Emulation.setTimezoneOverride", json!({ "timezoneId": tz }))
                .await?;
        }
        if !self.opts.block_urls.is_empty() {
            let _ = s
                .send(
                    "Network.setBlockedURLs",
                    json!({ "urls": self.opts.block_urls }),
                )
                .await?;
        }
        Ok(())
    }

    /// Navigate and wait the requested lifecycle state.
    pub async fn goto(self: &Arc<Self>, url: &str, opts: GotoOpts) -> Result<Nav> {
        let wait_until = opts.wait_until.unwrap_or(WaitUntil::Interactive);
        let timeout = opts.timeout.unwrap_or(Duration::from_secs(45));
        let t0 = Instant::now();

        // fast path: the target was created with this exact URL (initial_url) and
        // chrome already did the work during boot — don't navigate again
        if self.opts.initial_url.as_deref() == Some(url) {
            // about:blank is also readyState complete — confirm the document is
            // actually the target, or the boot navigation is still in flight
            let state = self
                .eval("(function(){ return [document.readyState, location.href]; })()")
                .await
                .unwrap_or(Value::Null);
            let ready = state[0].as_str().unwrap_or("");
            let href = state[1].as_str().unwrap_or("");
            let at_target = href == url || url.starts_with(href.trim_end_matches('/'));
            let ready_ok = (wait_until == WaitUntil::Interactive
                && (ready == "interactive" || ready == "complete"))
                || (wait_until == WaitUntil::Load && ready == "complete");
            if ready_ok && at_target {
                let status = self
                    .eval("performance.getEntriesByType('navigation')[0]?.responseStatus ?? null")
                    .await
                    .ok()
                    .and_then(|v| v.as_u64().map(|n| n as u16));
                return Ok(Nav {
                    url: self.url(),
                    status: status.or(*self.last_doc_status.lock().unwrap()),
                    ms: t0.elapsed().as_millis(),
                    attempts: vec![],
                });
            }
        }

        *self.last_doc_status.lock().unwrap() = None;

        let mut nav_params = json!({ "url": url });
        if let Some(referer) = &opts.referer {
            nav_params["referrer"] = json!(referer);
        }
        let res = self
            .conn
            .send_timeout("Page.navigate", nav_params, timeout)
            .await?;
        if let Some(err) = res.get("errorText").and_then(Value::as_str)
            && !err.is_empty()
        {
            return Err(anyhow!("goto {url}: {err}"));
        }

        match wait_until {
            WaitUntil::None => {}
            WaitUntil::Interactive => {
                self.wait_lifecycle("DOMContentLoaded", timeout).await?;
            }
            WaitUntil::Load => {
                self.wait_lifecycle("load", timeout).await?;
            }
            WaitUntil::NetworkIdle => {
                self.wait_lifecycle("load", timeout).await.ok();
                self.wait_network_idle(timeout).await.ok();
            }
        }
        // the engine self-heals in vlx() (inline guard) — no per-navigation
        // re-injection round-trip here

        Ok(Nav {
            url: self.url.lock().unwrap().clone(),
            status: *self.last_doc_status.lock().unwrap(),
            ms: t0.elapsed().as_millis(),
            attempts: vec![],
        })
    }

    async fn wait_lifecycle(self: &Arc<Self>, name: &str, timeout: Duration) -> Result<()> {
        // NOTE: no readyState pre-check here — after a fresh Page.navigate the
        // OLD document is still 'complete' and the check would return before
        // the new document's event. Late attach is handled by the goto fast
        // path's location.href confirmation instead.
        let main = self.main_frame.lock().unwrap().clone();
        match self
            .conn
            .wait_event(
                "Page.lifecycleEvent",
                |ev| {
                    ev["params"]["name"].as_str() == Some(name)
                        && (main.is_none() || ev["params"]["frameId"].as_str() == main.as_deref())
                },
                timeout,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(_) => Err(anyhow!("lifecycle {name} not reached within {timeout:?}")),
        }
    }

    async fn wait_network_idle(self: &Arc<Self>, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let inflight = *self.inflight.lock().unwrap();
            if inflight <= 0 {
                let before = inflight;
                tokio::time::sleep(Duration::from_millis(500)).await;
                let after = *self.inflight.lock().unwrap();
                if before == after && after <= 0 {
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                return Err(anyhow!("network idle not reached within {timeout:?}"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn ensure_engine(self: &Arc<Self>) {
        let live = self
            .conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": "typeof window.__ks === 'object'", "returnByValue": true }),
            )
            .await
            .ok()
            .and_then(|r| r["result"]["value"].as_bool())
            .unwrap_or(false);
        if !live {
            let _ = self
                .conn
                .send("Runtime.evaluate", json!({ "expression": KESTREL_ENGINE }))
                .await;
        }
    }

    /// Evaluate a JS expression (or function-as-string) and return a JSON value.
    pub async fn eval(self: &Arc<Self>, js: &str) -> Result<Value> {
        let wrapped = format!(
            "(async () => {{ let v = ({js}); if (typeof v === 'function') v = v(); if (v && typeof v.then === 'function') v = await v; return v; }})()"
        );
        let res = self
            .conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": wrapped, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = res.get("exceptionDetails") {
            // not an expression? try raw evaluation (statements keep their value)
            let raw = self
                .conn
                .send(
                    "Runtime.evaluate",
                    json!({ "expression": js, "returnByValue": true, "awaitPromise": true }),
                )
                .await;
            return match raw {
                Ok(r) if r.get("exceptionDetails").is_none() => Ok(r["result"]["value"].clone()),
                _ => {
                    let desc = ex["exception"]["description"]
                        .as_str()
                        .or_else(|| ex["text"].as_str())
                        .unwrap_or("eval error");
                    Err(anyhow!("eval error: {desc}"))
                }
            };
        }
        Ok(res["result"]["value"].clone())
    }

    /// Call the in-page engine: __ks.<fn>(args…). Fast path sends the bare
    /// call (the init script guarantees the engine on every document); on a
    /// missing-engine reference error it injects once and retries.
    async fn vlx(self: &Arc<Self>, call: String) -> Result<Value> {
        let res = self
            .conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": call, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if res.get("exceptionDetails").is_none() {
            return Ok(res["result"]["value"].clone());
        }
        let desc = res["exceptionDetails"]["exception"]["description"]
            .as_str()
            .or_else(|| res["exceptionDetails"]["text"].as_str())
            .unwrap_or("")
            .to_string();
        if !desc.contains("__ks") {
            return Err(anyhow!("engine error: {desc}"));
        }
        let _ = self
            .conn
            .send("Runtime.evaluate", json!({ "expression": KESTREL_ENGINE }))
            .await;
        let res = self
            .conn
            .send(
                "Runtime.evaluate",
                json!({ "expression": call, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = res.get("exceptionDetails") {
            let desc = ex["exception"]["description"]
                .as_str()
                .or_else(|| ex["text"].as_str())
                .unwrap_or("engine error");
            return Err(anyhow!("engine error: {desc}"));
        }
        Ok(res["result"]["value"].clone())
    }

    /// Raw trusted click at coordinates.
    pub async fn mouse_click(self: &Arc<Self>, x: f64, y: f64) -> Result<()> {
        self.conn
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.conn
            .send(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        Ok(())
    }

    pub fn url(&self) -> String {
        self.url.lock().unwrap().clone()
    }

    pub async fn title(self: &Arc<Self>) -> Result<String> {
        Ok(self
            .eval("document.title")
            .await?
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    pub async fn content(self: &Arc<Self>) -> Result<String> {
        Ok(self
            .eval("document.documentElement.outerHTML")
            .await?
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    pub async fn text(self: &Arc<Self>, sel: &str) -> Result<Option<String>> {
        Ok(self
            .vlx(format!("__ks.text({})", json_str(sel)))
            .await?
            .as_str()
            .map(|s| s.to_string()))
    }

    pub async fn attr(self: &Arc<Self>, sel: &str, name: &str) -> Result<Option<String>> {
        Ok(self
            .vlx(format!("__ks.attr({}, {})", json_str(sel), json_str(name)))
            .await?
            .as_str()
            .map(|s| s.to_string()))
    }

    pub async fn count(self: &Arc<Self>, sel: &str) -> Result<u64> {
        Ok(self
            .vlx(format!("__ks.count({})", json_str(sel)))
            .await?
            .as_u64()
            .unwrap_or(0))
    }

    /// Batch extraction: spec = {text, html, tag, attrs: [...], limit}
    pub async fn extract(self: &Arc<Self>, sel: &str, spec: Value) -> Result<Vec<Value>> {
        let v = self
            .vlx(format!(
                "__ks.extract({}, {})",
                json_str(sel),
                serde_json::to_string(&spec)?
            ))
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// In-page MutationObserver wait (ONE round-trip, no polling over the wire).
    pub async fn wait_for_selector(self: &Arc<Self>, sel: &str, timeout: Duration) -> Result<bool> {
        let v = self
            .vlx(format!(
                "__ks.wait({}, {}, 'visible')",
                json_str(sel),
                timeout.as_millis()
            ))
            .await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn dialogs(&self) -> Vec<DialogEvent> {
        self.dialogs.lock().unwrap().clone()
    }

    pub async fn console(&self) -> Vec<ConsoleMessage> {
        self.console.lock().unwrap().clone()
    }

    pub async fn errors(&self) -> Vec<ConsoleMessage> {
        self.errors.lock().unwrap().clone()
    }

    pub async fn requests(&self) -> Vec<RequestEntry> {
        self.requests.lock().unwrap().values().cloned().collect()
    }

    /// Body of a captured response (needs `intercept: true`).
    pub async fn body(self: &Arc<Self>, entry_id: &str) -> Result<Option<String>> {
        let res = self
            .conn
            .send("Network.getResponseBody", json!({ "requestId": entry_id }))
            .await;
        match res {
            Ok(r) => {
                let b64 = r
                    .get("base64Encoded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let body = r
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if b64 {
                    Ok(Some(
                        String::from_utf8_lossy(&b64_decode(&body)?).to_string(),
                    ))
                } else {
                    Ok(Some(body))
                }
            }
            Err(_) => Ok(None),
        }
    }

    /// Cookies for the current page URL (Storage fallback when empty).
    pub async fn cookies(self: &Arc<Self>) -> Result<Vec<Cookie>> {
        let url = self.url();
        let mut res = self
            .conn
            .send("Network.getCookies", json!({ "urls": [url] }))
            .await?;
        let mut list = res["cookies"].as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            res = self.conn.send("Storage.getCookies", json!({})).await?;
            list = res["cookies"].as_array().cloned().unwrap_or_default();
        }
        Ok(list.into_iter().filter_map(parse_cookie).collect())
    }

    pub async fn set_cookies(self: &Arc<Self>, cookies: Vec<Cookie>) -> Result<()> {
        let values: Vec<Value> = cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap())
            .collect();
        self.conn
            .send("Network.setCookies", json!({ "cookies": values }))
            .await?;
        Ok(())
    }

    pub async fn clear_cookies(self: &Arc<Self>) -> Result<()> {
        self.conn
            .send("Network.clearBrowserCookies", json!({}))
            .await?;
        Ok(())
    }

    /// localStorage of the current origin: { key: value }
    pub async fn local_storage(self: &Arc<Self>) -> Result<serde_json::Map<String, Value>> {
        let v = self
            .eval("(function(){const o={}; for (let i=0;i<localStorage.length;i++){const k=localStorage.key(i); o[k]=localStorage.getItem(k);} return o;})()")
            .await?;
        Ok(v.as_object().cloned().unwrap_or_default())
    }

    /// Apply a storage state before navigation (Playwright-format origins).
    pub async fn apply_storage_state(self: &Arc<Self>, state: &Value) -> Result<()> {
        if let Some(origins) = state.get("origins").and_then(Value::as_array) {
            for origin in origins {
                let o = origin.get("origin").and_then(Value::as_str).unwrap_or("");
                let items = origin.get("localStorage").and_then(Value::as_array);
                if o.is_empty() || items.is_none() {
                    continue;
                }
                for item in items.unwrap_or(&vec![]) {
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    let value = item.get("value").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let _ = self
                        .eval(&format!(
                            "localStorage.setItem({},{})",
                            json_str(name),
                            json_str(value)
                        ))
                        .await;
                }
            }
        }
        Ok(())
    }

    /// PNG screenshot of the viewport, or the full page when `full`.
    pub async fn screenshot(self: &Arc<Self>, full: bool) -> Result<Vec<u8>> {
        let mut params = json!({ "format": "png", "captureBeyondViewport": full });
        if full {
            let metrics = self.conn.send("Page.getLayoutMetrics", json!({})).await?;
            let size = &metrics["cssContentSize"];
            let w = size["width"].as_f64().unwrap_or(0.0).min(16384.0);
            let h = size["height"].as_f64().unwrap_or(0.0).min(16384.0);
            if w > 0.0 && h > 0.0 {
                params["clip"] = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
            }
        }
        let res = self.conn.send("Page.captureScreenshot", params).await?;
        let data = res
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("captureScreenshot returned no data"))?;
        b64_decode(data)
    }

    /// PDF (printToPDF); `format` e.g. "A4", "Letter".
    pub async fn pdf(self: &Arc<Self>, format: Option<&str>, landscape: bool) -> Result<Vec<u8>> {
        let mut params = json!({ "printBackground": true });
        if let Some(f) = format {
            params["paperWidth"] = json!(paper_size(f).0);
            params["paperHeight"] = json!(paper_size(f).1);
        }
        params["landscape"] = json!(landscape);
        let res = self.conn.send("Page.printToPDF", params).await?;
        let data = res
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("printToPDF returned no data"))?;
        b64_decode(data)
    }

    /// Human-like interaction handle (bezier moves, jitter typing, warmup, hold).
    pub fn human(self: &Arc<Self>) -> crate::human::Human {
        crate::human::Human::new(self.clone())
    }

    /// Structured detection of the anti-bot widget on this page.
    pub async fn detect_challenge(
        self: &Arc<Self>,
        wait: bool,
        timeout: Duration,
    ) -> Result<super::challenge::ChallengeInfo> {
        super::challenge::detect(self, wait, timeout, Duration::from_millis(500)).await
    }

    /// Wait for a captcha token field to populate; diagnoses its state on timeout.
    pub async fn wait_for_captcha_token(
        self: &Arc<Self>,
        selector: Option<&str>,
        timeout: Duration,
    ) -> Result<String> {
        super::challenge::wait_for_token(
            self,
            selector,
            timeout,
            Duration::from_millis(250),
            Duration::from_secs(15),
        )
        .await
    }

    /// Wait for an arbitrary JS expression to become truthy (polled in-page).
    pub async fn wait_for_function(self: &Arc<Self>, js: &str, timeout: Duration) -> Result<Value> {
        let expr = format!("__ks.waitExpr({}, {})", json_str(js), timeout.as_millis());
        self.vlx(expr).await
    }

    /// Navigate with backoff — flaky residential proxies are the normal case.
    pub async fn goto_with_retry(
        self: &Arc<Self>,
        url: &str,
        retries: usize,
        timeout: Duration,
    ) -> Result<Nav> {
        let mut delay = 400u64;
        let mut attempts: Vec<Value> = vec![];
        for i in 0..=retries {
            let t0 = Instant::now();
            match self
                .goto(
                    url,
                    GotoOpts {
                        wait_until: Some(WaitUntil::Interactive),
                        timeout: Some(timeout),
                        referer: None,
                    },
                )
                .await
            {
                Ok(nav) => {
                    attempts.push(json!({ "attempt": i + 1, "ms": t0.elapsed().as_millis(), "status": nav.status }));
                    let mut with_attempts = nav;
                    with_attempts.attempts = attempts;
                    return Ok(with_attempts);
                }
                Err(e) => {
                    attempts.push(json!({ "attempt": i + 1, "ms": t0.elapsed().as_millis(), "error": e.to_string() }));
                    let retryable = RETRYABLE_RE.is_match(&e.to_string());
                    if i == retries || !retryable {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    delay *= 2;
                }
            }
        }
        unreachable!()
    }

    /// Frames: flattened Page.getFrameTree (main frame first).
    pub async fn frames(self: &Arc<Self>) -> Result<Vec<Value>> {
        let res = self.conn.send("Page.getFrameTree", json!({})).await?;
        let mut out = vec![];
        walk_frames(&res["frameTree"], 0, &mut out);
        Ok(out)
    }

    /// Screenshot of one element (clip from its rect via the in-page engine).
    pub async fn screenshot_element(self: &Arc<Self>, sel: &str) -> Result<Vec<u8>> {
        self.ensure_engine().await;
        let rect = self.eval(&format!("__ks.rect({})", json_str(sel))).await?;
        let x = rect.get("x").and_then(Value::as_f64).unwrap_or(0.0);
        let y = rect.get("y").and_then(Value::as_f64).unwrap_or(0.0);
        let w = rect.get("width").and_then(Value::as_f64).unwrap_or(0.0);
        let h = rect.get("height").and_then(Value::as_f64).unwrap_or(0.0);
        if w <= 0.0 || h <= 0.0 {
            return Err(anyhow!("element not visible: {sel}"));
        }
        let params = json!({
            "format": "png",
            "clip": { "x": x, "y": y, "width": w.min(16384.0), "height": h.min(16384.0), "scale": 1 },
        });
        let res = self.conn.send("Page.captureScreenshot", params).await?;
        let data = res
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("captureScreenshot returned no data"))?;
        b64_decode(data)
    }

    /// HAR 1.2 export from the captured requests (bodies optional).
    pub async fn har(self: &Arc<Self>, with_bodies: bool) -> Result<Value> {
        let reqs = self.requests().await;
        let mut entries = vec![];
        for r in reqs {
            let mut body = json!(null);
            if with_bodies {
                body = json!(self.body(&r.id).await.unwrap_or(None));
            }
            entries.push(json!({
                "startedDateTime": "1970-01-01T00:00:00Z",
                "time": 0,
                "request": { "method": r.method, "url": r.url, "httpVersion": "http/1.1", "headers": [], "queryString": [], "headersSize": -1, "bodySize": -1 },
                "response": {
                    "status": r.status.unwrap_or(0), "statusText": "", "httpVersion": "http/1.1",
                    "headers": [{ "name": "content-type", "value": r.content_type.clone().unwrap_or_default() }],
                    "content": { "size": -1, "mimeType": r.content_type.clone().unwrap_or_default(), "text": body },
                    "headersSize": -1, "bodySize": -1, "redirectURL": "",
                },
                "cache": {},
                "timings": { "send": 0, "wait": 0, "receive": 0 },
                "_resourceType": r.resource_type,
                "_failed": r.failed,
            }));
        }
        Ok(json!({
            "log": {
                "version": "1.2",
                "creator": { "name": "kestrel", "version": env!("CARGO_PKG_VERSION") },
                "pages": [],
                "entries": entries,
            }
        }))
    }

    pub async fn close(self: Arc<Self>) {
        self.conn.close().await;
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

static RETRYABLE_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"(?i)timeout|ERR_|ECONN|closed|Tunnel|SSL|reset|502|503|504").unwrap()
});

fn walk_frames(node: &Value, depth: usize, out: &mut Vec<Value>) {
    let f = &node["frame"];
    if !f.is_null() {
        out.push(json!({
            "id": f["id"], "url": f["url"], "name": f.get("name").cloned().unwrap_or(Value::Null),
            "parentId": f.get("parentId").cloned().unwrap_or(Value::Null), "depth": depth,
        }));
    }
    if let Some(children) = node.get("childFrames").and_then(Value::as_array) {
        for c in children {
            walk_frames(c, depth + 1, out);
        }
    }
}

fn parse_cookie(v: Value) -> Option<Cookie> {
    Some(Cookie {
        name: v.get("name")?.as_str()?.to_string(),
        value: v
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        domain: v
            .get("domain")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        path: v
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/")
            .to_string(),
        expires: v.get("expires").and_then(Value::as_f64),
        http_only: v.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
        secure: v.get("secure").and_then(Value::as_bool).unwrap_or(false),
        same_site: v
            .get("sameSite")
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
    })
}

/// printToPDF paper sizes in inches.
fn paper_size(name: &str) -> (f64, f64) {
    match name.to_ascii_uppercase().as_str() {
        "LETTER" => (8.5, 11.0),
        "LEGAL" => (8.5, 14.0),
        "TABLOID" => (11.0, 17.0),
        "A3" => (11.69, 16.54),
        "A5" => (5.83, 8.27),
        _ => (8.27, 11.69), // A4
    }
}

/// Normalise cookies the way kestrel does: dot the domain, default the path,
/// de-duplicate (later wins). Ported from src/field.js normalizeCookies.
pub fn normalize_cookies(mut list: Vec<Cookie>) -> Vec<Cookie> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Cookie> = vec![];
    for c in list.drain(..) {
        let mut c = c;
        let bare = c.domain.trim_start_matches('.').to_lowercase();
        let is_ip =
            bare.split('.').count() == 4 && bare.split('.').all(|p| p.parse::<u8>().is_ok());
        if !bare.is_empty() {
            if !is_ip && !bare.starts_with('.') {
                c.domain = format!(".{bare}");
            } else {
                c.domain = bare;
            }
        }
        if c.path.is_empty() {
            c.path = "/".to_string();
        }
        let key = format!("{}|{}|{}", c.name, c.domain, c.path);
        if let Some(i) = seen.get(&key) {
            out[*i] = c;
        } else {
            seen.insert(key, out.len());
            out.push(c);
        }
    }
    out
}
