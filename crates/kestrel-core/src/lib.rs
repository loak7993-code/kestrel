// kestrel-core — the engine: any installed Chromium-family browser over CDP,
// or no browser at all (keep-alive HTTP + DOM). Scriptable from the embedded
// QuickJS runtime (`ksl run script.js`) or from Rust directly.
pub mod cdp;
pub mod devices;
pub mod discovery;
pub mod http;
pub mod human;
pub mod js;
pub mod needs_js;
pub mod pool;
pub mod testsite;

use anyhow::Result;
use cdp::browser::Browser;
use cdp::page::Page;
use http::html::HtmlDoc;
use std::sync::Arc;
use std::time::Duration;

pub use cdp::challenge::ChallengeInfo;
pub use cdp::page::{Cookie, Nav, RequestEntry, WaitUntil};
pub use discovery::{FoundBrowser, discover, find_browser};
pub use http::{Link, LiteResponse};

#[derive(Debug, Clone, Default)]
pub struct OpenOpts {
    /// auto (default) | lite (never escalate) | cdp (always browser)
    pub engine: Option<String>,
    pub browser: Option<String>,
    pub timeout: Duration,
    pub headers: Vec<(String, String)>,
    pub user_agent: Option<String>,
    pub proxy: Option<String>,
    pub viewport: Option<(u32, u32, f64)>,
    pub device: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub block_urls: Vec<String>,
    pub stealth: Option<cdp::stealth::StealthOpts>,
    pub dialog_action: Option<cdp::page::DialogAction>,
}

impl OpenOpts {
    pub fn lite_opts(&self) -> http::LiteOpts {
        http::LiteOpts {
            timeout: self.timeout,
            headers: self.headers.clone(),
            user_agent: self.user_agent.clone(),
            proxy: self.proxy.clone(),
            max_redirects: 10,
        }
    }
    pub fn page_opts(&self, initial_url: Option<&str>) -> cdp::page::PageOpts {
        cdp::page::PageOpts {
            initial_url: initial_url.map(|u| u.to_string()),
            viewport: self.viewport,
            ua: self.user_agent.clone(),
            device: self.device.clone(),
            locale: self.locale.clone(),
            timezone: self.timezone.clone(),
            block_urls: self.block_urls.clone(),
            intercept: true,
            stealth: self.stealth.clone(),
            dialog_action: self.dialog_action.clone(),
        }
    }
    pub fn launch_opts(&self) -> cdp::browser::LaunchOpts {
        cdp::browser::LaunchOpts {
            browser: self.browser.clone(),
            headless: true,
            proxy: self.proxy.clone(),
            args: vec![],
            timeout: self.timeout,
        }
    }
}

/// What `open()` returns: a lite snapshot or a live CDP page — one surface.
/// (The lite variant is a snapshot value; the cdp variant owns a browser.)
#[allow(clippy::large_enum_variant)]
pub enum Session {
    Lite(LiteSession),
    Cdp(CdpSession),
}

pub struct LiteSession {
    pub res: LiteResponse,
    pub doc: HtmlDoc,
    pub opts: OpenOpts,
}

pub struct CdpSession {
    pub browser: Box<Browser>,
    pub page: Arc<Page>,
    pub opts: OpenOpts,
}

impl Session {
    /// Open a URL. engine: auto → lite first, escalate when the page needs JS
    /// (cookie state carries over).
    pub async fn open(url: &str, opts: OpenOpts) -> Result<Session> {
        let engine = opts.engine.clone().unwrap_or_else(|| "auto".to_string());
        match engine.as_str() {
            "lite" => Ok(Session::Lite(LiteSession::fetch(url, opts).await?)),
            "cdp" => Ok(Session::Cdp(CdpSession::goto(url, opts).await?)),
            _ => {
                let lite = LiteSession::fetch(url, opts.clone()).await?;
                let server = lite.res.header("server").map(|s| s.to_string());
                let cf = lite.res.headers.keys().any(|k| k == "cf-mitigated");
                if needs_js::needs_js(lite.res.status, server.as_deref(), cf, &lite.res.text()) {
                    let cdp = CdpSession::goto_with_jar(url, opts, Some(&lite)).await?;
                    Ok(Session::Cdp(cdp))
                } else {
                    Ok(Session::Lite(lite))
                }
            }
        }
    }

    pub fn engine(&self) -> &'static str {
        match self {
            Session::Lite(_) => "lite",
            Session::Cdp(_) => "cdp",
        }
    }

    pub fn url(&self) -> String {
        match self {
            Session::Lite(s) => s.res.url.clone(),
            Session::Cdp(s) => s.page.url(),
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Session::Lite(s) => Some(s.res.status),
            Session::Cdp(_) => None,
        }
    }

    pub async fn title(&self) -> Result<String> {
        match self {
            Session::Lite(s) => Ok(s.doc.title().unwrap_or_default()),
            Session::Cdp(s) => s.page.title().await,
        }
    }

    pub async fn readable(&self) -> Result<String> {
        match self {
            Session::Lite(s) => Ok(s.doc.readable()),
            Session::Cdp(s) => {
                Ok(CdpSession::readable_html(&s.page, &s.page.content().await?).await)
            }
        }
    }

    pub async fn html(&self) -> Result<String> {
        match self {
            Session::Lite(s) => Ok(s.res.text()),
            Session::Cdp(s) => s.page.content().await,
        }
    }

    pub async fn text(&self, sel: &str) -> Result<Option<String>> {
        match self {
            Session::Lite(s) => Ok(s.doc.text_of(sel)),
            Session::Cdp(s) => s.page.text(sel).await,
        }
    }

    pub async fn links(&self) -> Result<Vec<Link>> {
        match self {
            Session::Lite(s) => Ok(s.doc.links()),
            Session::Cdp(s) => {
                let rows = s
                    .page
                    .extract(
                        "a[href]",
                        serde_json::json!({ "text": true, "attrs": ["href"] }),
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| Link {
                        text: r
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        href: r
                            .get("href")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect())
            }
        }
    }

    pub async fn images(&self) -> Result<Vec<serde_json::Value>> {
        match self {
            Session::Lite(s) => Ok(s.doc.images()),
            Session::Cdp(s) => {
                let rows = s
                    .page
                    .extract("img", serde_json::json!({ "attrs": ["src", "alt"] }))
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "src": r.get("src").cloned().unwrap_or(serde_json::Value::Null),
                            "alt": r.get("alt").cloned().unwrap_or(serde_json::Value::Null),
                        })
                    })
                    .collect())
            }
        }
    }

    pub async fn tables(&self) -> Result<Vec<serde_json::Value>> {
        match self {
            Session::Lite(s) => Ok(s.doc.tables()),
            Session::Cdp(s) => Ok(s
                .page
                .eval("Array.from(document.querySelectorAll('table')).map(t => ({ headers: [...t.querySelectorAll('thead th, thead td')].map(c => c.textContent.trim()), rows: [...t.querySelectorAll('tbody tr')].map(r => [...r.querySelectorAll('td')].map(c => c.textContent.trim())) }))")
                .await?
                .as_array()
                .cloned()
                .unwrap_or_default()),
        }
    }

    pub async fn meta(&self) -> Result<std::collections::HashMap<String, String>> {
        match self {
            Session::Lite(s) => Ok(s.doc.meta()),
            Session::Cdp(s) => {
                let v = s
                    .page
                    .eval("(function(){const o={}; const t=document.querySelector('title'); if (t) o.title=t.textContent; document.querySelectorAll('meta').forEach(m => { const n=m.getAttribute('name')||m.getAttribute('property'); const c=m.getAttribute('content'); if (n&&c) o[n]=c; }); return o;})()")
                    .await?;
                let mut out = std::collections::HashMap::new();
                if let serde_json::Value::Object(map) = v {
                    for (k, val) in map {
                        if let Some(s) = val.as_str() {
                            out.insert(k, s.to_string());
                        }
                    }
                }
                Ok(out)
            }
        }
    }

    pub async fn jsonld(&self) -> Result<Vec<serde_json::Value>> {
        match self {
            Session::Lite(s) => Ok(s.doc.jsonld()),
            Session::Cdp(s) => Ok(s
                .page
                .eval("Array.from(document.querySelectorAll('script[type=\"application/ld+json\"]')).map(s => { try { return JSON.parse(s.textContent) } catch (e) { return null } }).filter(Boolean)")
                .await?
                .as_array()
                .cloned()
                .unwrap_or_default()),
        }
    }

    /// Save session state (cookies + localStorage of the current origin).
    pub async fn save_session(self) -> Result<(Session, serde_json::Value)> {
        match self {
            Session::Lite(l) => Ok((
                Session::Lite(LiteSession {
                    res: LiteResponse {
                        url: l.res.url.clone(),
                        start_url: l.res.start_url.clone(),
                        status: l.res.status,
                        headers: l.res.headers.clone(),
                        set_cookies: l.res.set_cookies.clone(),
                        body: l.res.body.clone(),
                        ms: l.res.ms,
                    },
                    doc: l.doc,
                    opts: l.opts.clone(),
                }),
                serde_json::json!({ "cookies": [], "origins": [] }),
            )),
            Session::Cdp(s) => {
                let cookies = s.page.cookies().await?;
                let ls = s.page.local_storage().await?;
                let origin = url::Url::parse(&s.page.url())
                    .ok()
                    .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
                    .unwrap_or_else(|| s.page.url());
                Ok((
                    Session::Cdp(s),
                    serde_json::json!({
                        "cookies": cookies,
                        "origins": [{ "origin": origin, "localStorage": ls.iter()
                            .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
                            .collect::<Vec<_>>() }],
                    }),
                ))
            }
        }
    }

    /// Restore session state (applies cookies + origins/localStorage).
    pub async fn load_session(self, state: serde_json::Value) -> Result<Session> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let page = s.page.clone();
                if let Some(cookies) = state.get("cookies").and_then(serde_json::Value::as_array) {
                    let list: Vec<Cookie> = cookies
                        .iter()
                        .filter_map(|c| {
                            Some(Cookie {
                                name: c.get("name")?.as_str()?.to_string(),
                                value: c
                                    .get("value")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                domain: c
                                    .get("domain")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                path: c
                                    .get("path")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("/")
                                    .to_string(),
                                expires: c.get("expires").and_then(serde_json::Value::as_f64),
                                http_only: c
                                    .get("httpOnly")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                                secure: c
                                    .get("secure")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                                same_site: c
                                    .get("sameSite")
                                    .and_then(serde_json::Value::as_str)
                                    .map(String::from),
                            })
                        })
                        .collect();
                    page.set_cookies(cdp::page::normalize_cookies(list)).await?;
                }
                page.apply_storage_state(&state).await?;
                Ok(Session::Cdp(s))
            }
            _ => unreachable!(),
        }
    }

    pub async fn cookies(&self) -> Result<Vec<Cookie>> {
        match self {
            Session::Lite(_) => Ok(vec![]),
            Session::Cdp(s) => s.page.cookies().await,
        }
    }

    /// Escalate a lite session to the browser (transparent upgrade).
    pub async fn escalate(self) -> Result<Session> {
        match self {
            Session::Cdp(_) => Ok(self),
            Session::Lite(lite) => {
                let opts = lite.opts.clone();
                let cdp = CdpSession::goto(&lite.res.url, opts).await?;
                Ok(Session::Cdp(cdp))
            }
        }
    }

    /// Interior helper: escalate only lite sessions (cdp passes through).
    async fn escalate_lite(self) -> Result<Session> {
        match self {
            Session::Lite(lite) => {
                let opts = lite.opts.clone();
                let cdp = CdpSession::goto(&lite.res.url, opts).await?;
                Ok(Session::Cdp(cdp))
            }
            other => Ok(other),
        }
    }

    pub async fn wait_for(self, sel: &str, timeout: Duration) -> Result<(Session, bool)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let ok = s.page.wait_for_selector(sel, timeout).await?;
                Ok((Session::Cdp(s), ok))
            }
            _ => unreachable!(),
        }
    }

    pub async fn screenshot(self, full: bool) -> Result<(Session, Vec<u8>)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let buf = s.page.screenshot(full).await?;
                Ok((Session::Cdp(s), buf))
            }
            _ => unreachable!(),
        }
    }

    pub async fn element_shot(self, sel: &str) -> Result<(Session, Vec<u8>)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let png = s.page.screenshot_element(sel).await?;
                Ok((Session::Cdp(s), png))
            }
            _ => unreachable!(),
        }
    }

    pub async fn pdf(self, format: Option<&str>, landscape: bool) -> Result<(Session, Vec<u8>)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let buf = s.page.pdf(format, landscape).await?;
                Ok((Session::Cdp(s), buf))
            }
            _ => unreachable!(),
        }
    }

    pub async fn eval(self, js: &str) -> Result<(Session, serde_json::Value)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let v = s.page.eval(js).await?;
                Ok((Session::Cdp(s), v))
            }
            _ => unreachable!(),
        }
    }

    pub async fn detect_challenge(
        self,
        wait: bool,
        timeout: Duration,
    ) -> Result<(Session, ChallengeInfo)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let info = s.page.detect_challenge(wait, timeout).await?;
                Ok((Session::Cdp(s), info))
            }
            _ => unreachable!(),
        }
    }

    pub async fn wait_for_captcha_token(
        self,
        selector: Option<&str>,
        timeout: Duration,
    ) -> Result<(Session, String)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let tok = s.page.wait_for_captcha_token(selector, timeout).await?;
                Ok((Session::Cdp(s), tok))
            }
            _ => unreachable!(),
        }
    }

    /// Engage an interactive challenge (escalates lite → cdp).
    pub async fn engage_challenge(
        self,
        timeout: Duration,
        human: bool,
    ) -> Result<(Session, cdp::challenge::EngageResult)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let r = cdp::challenge::engage(&s.page, timeout, human).await?;
                Ok((Session::Cdp(s), r))
            }
            _ => unreachable!(),
        }
    }

    /// Navigate through a challenge: goto → engage → reload on clearance.
    pub async fn goto_through(self, url: &str, timeout: Duration) -> Result<(Session, Nav)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let nav = cdp::challenge::goto_through(&s.page, url, timeout, true).await?;
                Ok((Session::Cdp(s), nav))
            }
            _ => unreachable!(),
        }
    }

    pub async fn netlog(self, filter: Option<&str>) -> Result<(Session, Vec<RequestEntry>)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let mut rows: Vec<RequestEntry> = s.page.requests().await;
                if let Some(f) = filter {
                    let re = if f.starts_with('/') && f.ends_with('/') && f.len() > 2 {
                        regex::Regex::new(&f[1..f.len() - 1]).ok()
                    } else {
                        regex::Regex::new(&regex::escape(f)).ok()
                    };
                    if let Some(re) = re {
                        rows.retain(|r| re.is_match(&r.url));
                    } else {
                        rows.retain(|r| r.url.contains(f));
                    }
                }
                Ok((Session::Cdp(s), rows))
            }
            _ => unreachable!(),
        }
    }

    pub async fn har(self, with_bodies: bool) -> Result<(Session, serde_json::Value)> {
        let s = self.escalate_lite().await?;
        match s {
            Session::Cdp(s) => {
                let har = s.page.har(with_bodies).await?;
                Ok((Session::Cdp(s), har))
            }
            _ => unreachable!(),
        }
    }

    pub async fn close(self) {
        match self {
            Session::Lite(_) => {}
            Session::Cdp(s) => {
                s.page.close().await;
                let mut b = s.browser;
                b.close().await;
            }
        }
    }
}

impl LiteSession {
    pub async fn fetch(url: &str, opts: OpenOpts) -> Result<LiteSession> {
        let client = http::build_client(&opts.lite_opts())?;
        let res = http::fetch(&client, url, &opts.lite_opts()).await?;
        let doc = res.doc();
        Ok(LiteSession { res, doc, opts })
    }
}

impl CdpSession {
    pub async fn goto(url: &str, opts: OpenOpts) -> Result<CdpSession> {
        Self::goto_inner(url, opts, None).await
    }

    /// Goto with the lite response's cookies replayed before the first
    /// navigation (the hybrid-pipeline hand-off).
    pub async fn goto_with_jar(
        url: &str,
        opts: OpenOpts,
        lite: Option<&LiteSession>,
    ) -> Result<CdpSession> {
        Self::goto_inner(url, opts, lite).await
    }

    async fn goto_inner(
        url: &str,
        opts: OpenOpts,
        lite: Option<&LiteSession>,
    ) -> Result<CdpSession> {
        let mut browser = Box::new(Browser::launch(opts.launch_opts()).await?);
        // the initial-load fast path only applies when there is no cookie state
        // to replay first (the lite jar must land BEFORE the first navigation)
        let can_initial = lite.is_none();
        let page = browser
            .new_page(if can_initial {
                let mut po = opts.page_opts(Some(url));
                po.initial_url = Some(url.to_string());
                po
            } else {
                opts.page_opts(None)
            })
            .await?;
        if let Some(l) = lite {
            let cookies = cookies_from_headers(&l.res.url, &l.res.set_cookies);
            if !cookies.is_empty() {
                let _ = page.set_cookies(cookies).await;
            }
        }
        let _ = page
            .goto(
                url,
                cdp::page::GotoOpts {
                    wait_until: Some(WaitUntil::Interactive),
                    timeout: Some(opts.timeout),
                    referer: None,
                },
            )
            .await?;
        Ok(CdpSession {
            browser,
            page,
            opts,
        })
    }

    /// Readability for browser pages: same rendering rules as the lite path,
    /// over the DOM the engine sees.
    pub async fn readable_html(page: &Arc<Page>, html: &str) -> String {
        let doc = HtmlDoc::parse(html, &page.url());
        doc.readable()
    }
}

/// set-cookie values from the lite fetch → cookies for the browser jar.
/// The hybrid-pipeline hand-off: the lite engine's cookie state carries over
/// to the escalated browser session BEFORE its first navigation.
fn cookies_from_headers(url: &str, set_cookies: &[String]) -> Vec<Cookie> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    let mut out = vec![];
    for raw in set_cookies {
        let mut parts = raw.split(';');
        let (Some(pair), attrs) = (parts.next(), parts) else {
            continue;
        };
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let mut c = Cookie {
            name: name.to_string(),
            value: value.trim().to_string(),
            domain: if host.is_empty() {
                String::new()
            } else {
                format!(".{host}")
            },
            path: "/".to_string(),
            expires: None,
            http_only: false,
            secure: false,
            same_site: None,
        };
        for a in attrs {
            let a = a.trim();
            let lower = a.to_ascii_lowercase();
            if let Some(p) = lower.strip_prefix("path=") {
                if !p.is_empty() {
                    c.path = a[5..].to_string();
                }
            } else if lower == "httponly" {
                c.http_only = true;
            } else if lower == "secure" {
                c.secure = true;
            } else if let Some(_exp) = lower.strip_prefix("expires=") {
                // best-effort: httpdate parse
                if let Ok(t) = httpdate::parse_http_date(a[8..].trim()) {
                    let secs = t
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as f64)
                        .unwrap_or(0.0);
                    c.expires = Some(secs);
                }
            }
        }
        out.push(c);
    }
    out
}
