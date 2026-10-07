// kestrel :: challenge — bot-management awareness: detect which captcha /
// challenge widget is on the page, wait for it, and report its state. Ported
// from src/field.js (widget detection) with the window-object authoritative
// signals and noisy-iframe exclusions.
use crate::cdp::page::Page;
use anyhow::{Result, anyhow};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Widget {
    pub kind: &'static str,
    pub script: Option<&'static str>,
    pub iframe: Option<&'static str>,
    pub win: Option<&'static str>,
    pub token_fields: &'static [&'static str],
    pub markers: &'static [&'static str],
}

pub const WIDGETS: &[Widget] = &[
    Widget {
        kind: "turnstile",
        script: Some(r"challenges\.cloudflare\.com/turnstile"),
        iframe: Some(r"challenges\.cloudflare\.com"),
        win: Some("turnstile"),
        token_fields: &[
            "input[name=\"cf-turnstile-response\"]",
            "textarea[name=\"cf-turnstile-response\"]",
        ],
        markers: &[
            "#turnstile-container",
            ".cf-turnstile",
            "#turnstile-wrapper",
            "#cf-turnstile",
        ],
    },
    Widget {
        kind: "recaptcha",
        script: Some(r"recaptcha/(api|releases)"),
        iframe: Some(r"recaptcha/api2"),
        win: Some("grecaptcha"),
        token_fields: &[
            "textarea[name=\"g-recaptcha-response\"]",
            "#g-recaptcha-response",
        ],
        markers: &[".g-recaptcha", "#g-recaptcha", "#recaptcha-demo"],
    },
    Widget {
        kind: "hcaptcha",
        script: Some(r"hcaptcha\.com"),
        iframe: Some(r"hcaptcha\.com"),
        win: Some("hcaptcha"),
        token_fields: &[
            "textarea[name=\"h-captcha-response\"]",
            "input[name=\"h-captcha-response\"]",
        ],
        markers: &[".h-captcha", "#hcaptcha-demo", "#hcaptcha-container"],
    },
    Widget {
        kind: "arkose",
        script: Some(r"arkoselabs\.com"),
        iframe: Some(r"arkoselabs\.com|funcaptcha"),
        win: Some("Arkose"),
        token_fields: &[
            "input[name=\"fc-token\"]",
            "input[name=\"arkose-token\"]",
            "input[name=\"verification-token\"]",
        ],
        markers: &["#arkose", "#arkose-iframe", ".arkose-challenge"],
    },
    Widget {
        kind: "awswaf-grid",
        script: Some(r"awswaf|aws-waf"),
        iframe: Some(r"awswaf"),
        win: Some("awsWafCookieDomainList"),
        token_fields: &["input[name=\"aws-waf-token\"]", "#aws-waf-token"],
        markers: &[
            "#amzn-captcha-verify-button",
            "#captcha-container",
            "#awswaf-iframe",
        ],
    },
    Widget {
        kind: "px",
        script: Some(r"px-cloud|perimeterx"),
        iframe: Some(r"px-cloud|perimeterx"),
        win: Some("_pxAppId"),
        token_fields: &["input[name=\"_px3\"]", "input[name=\"px-token\"]"],
        markers: &["#px-captcha", ".px-captcha"],
    },
];

/// Iframes that ship on normal pages and are NOT challenges — evidence noise.
static NOISY_IFRAMES: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)userway|accessi(be|bility)|enablea11y|recaptchaaccessibility|audioeye|essentialaccessibility").unwrap()
});

/// The in-page probe (port of the JS DETECT_PROBE): scripts, iframes, candidate
/// nodes, token inputs, and the authoritative window-object signals.
const PROBE: &str = r#"(function(){
  var deep = function(sel){
    var out = Array.prototype.slice.call(document.querySelectorAll(sel));
    var walk = document.createTreeWalker(document, NodeFilter.SHOW_ELEMENT);
    var el;
    while ((el = walk.nextNode())) {
      if (el.shadowRoot) out = out.concat(Array.prototype.slice.call(el.shadowRoot.querySelectorAll(sel)));
      if (el.tagName === 'IFRAME') { try { if (el.contentDocument) out = out.concat(Array.prototype.slice.call(el.contentDocument.querySelectorAll(sel))); } catch (e) {} }
    }
    return out;
  };
  var scripts = Array.prototype.map.call(document.scripts, function(s){ return s.src || ''; }).filter(Boolean);
  var iframes = deep('iframe').map(function(f){ return f.src || ''; }).filter(Boolean);
  var nodes = deep('[id*="captcha"],[class*="captcha"],[id^="turnstile"],[class*="turnstile"],[id^="g-recaptcha"],[id^="hcaptcha"],[id^="arkose"],[id^="px-"],[data-pkey],[data-sitekey]').map(function(e){
    var r = e.getBoundingClientRect();
    return { id: e.id || null, cls: (e.className && String(e.className)) || '', tag: e.tagName.toLowerCase(),
             key: (e.getAttribute && (e.getAttribute('data-sitekey') || e.getAttribute('data-pkey'))) || null,
             w: Math.round(r.width), h: Math.round(r.height), visible: r.width > 0 && r.height > 0 };
  });
  var inputs = Array.prototype.map.call(document.querySelectorAll('input,textarea'), function(i){ return i.name || i.id || ''; });
  var win = {};
  ['turnstile','grecaptcha','hcaptcha','Arkose','_pxAppId'].forEach(function(k){
    try { win[k] = typeof window[k] !== 'undefined'; } catch (e) { win[k] = false; }
  });
  try { win.awsWafCookieDomainList = !!window.awsWafCookieDomainList; } catch (e) {}
  return { scripts: scripts, iframes: iframes, nodes: nodes, inputs: inputs, win: win, html: document.documentElement.outerHTML.length };
})()"#;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChallengeInfo {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sitekey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iframe_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_loaded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markers: Option<Vec<String>>,
    /// how long wait-mode polled before the widget appeared (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waited: Option<u128>,
}

/// One detection candidate: score, kind, markers, sitekey, iframe, container,
/// visibility, scriptLoaded, window-signal.
#[allow(clippy::type_complexity)]
type Scored = (
    f64,
    String,
    Vec<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    Option<String>,
);

/// One-shot structured detection of whatever anti-bot widget is on the page.
pub async fn detect_once(page: &Arc<Page>) -> Result<ChallengeInfo> {
    let probe = page.eval(PROBE).await.unwrap_or_else(
        |_| json!({ "scripts": [], "iframes": [], "nodes": [], "inputs": [], "win": {} }),
    );

    let scripts: Vec<String> = probe["scripts"]
        .as_array()
        .map(|a| str_list(a))
        .unwrap_or_default();
    let mut iframes: Vec<String> = probe["iframes"]
        .as_array()
        .map(|a| str_list(a))
        .unwrap_or_default();
    iframes.retain(|u| !NOISY_IFRAMES.is_match(u));
    let nodes = probe["nodes"].as_array().cloned().unwrap_or_default();
    let inputs: Vec<String> = probe["inputs"]
        .as_array()
        .map(|a| str_list(a))
        .unwrap_or_default();
    let win = probe["win"].clone();

    #[allow(clippy::type_complexity)]
    let mut scored: Vec<Scored> = vec![];
    for w in WIDGETS {
        let mut score = 0.0f64;
        let mut markers: Vec<String> = vec![];
        let script_re = w.script.map(|p| Regex::new(&format!("(?i){p}")).unwrap());
        let iframe_re = w.iframe.map(|p| Regex::new(&format!("(?i){p}")).unwrap());
        let hit_script = script_re
            .as_ref()
            .and_then(|re| scripts.iter().find(|s| re.is_match(s)).cloned());
        if let Some(s) = &hit_script {
            score += 5.0;
            markers.push(format!("script:{}", &s[..s.len().min(70)]));
        }
        // window-object signals are authoritative; a src scan races hydration
        let hit_window = w
            .win
            .and_then(|k| win.get(k).and_then(Value::as_bool))
            .unwrap_or(false);
        if hit_window {
            score += 4.0;
            markers.push(format!("window:{}", w.win.unwrap()));
        }
        let hit_frame = iframe_re
            .as_ref()
            .and_then(|re| iframes.iter().find(|s| re.is_match(s)).cloned());
        if let Some(f) = &hit_frame {
            score += 4.0;
            markers.push(format!("iframe:{}", &f[..f.len().min(70)]));
        }
        for n in &nodes {
            let id = n
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let cls = n
                .get("cls")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let id_hit = w.markers.iter().any(|m| {
                m.starts_with('#')
                    && id.starts_with(&m[1..].split('-').next().unwrap_or(m).to_lowercase())
            });
            let cls_hit = w.markers.iter().any(|m| {
                m.starts_with('.')
                    && cls.contains(&m[1..].split('-').next().unwrap_or(m).to_lowercase())
            });
            if id_hit || cls_hit {
                score += 2.0;
                markers.push(format!("node:{}", if id.is_empty() { &cls } else { &id }));
            }
        }
        let token_hit = inputs.iter().any(|n| {
            w.token_fields.iter().any(|t| {
                // `input[name="x"]` → match the element's name; `#x` → match the id.
                // An empty extraction must NEVER count (it would match everything).
                if let Some(i) = t.find("name=\"") {
                    let name = &t[i + 6..];
                    let name = name.split('"').next().unwrap_or("");
                    !name.is_empty() && n.to_lowercase().contains(&name.to_lowercase())
                } else if let Some(rest) = t.strip_prefix('#') {
                    !rest.is_empty() && n.to_lowercase().contains(&rest.to_lowercase())
                } else {
                    false
                }
            })
        });
        if token_hit {
            score += 1.0;
            markers.push("token-field".to_string());
        }
        if score > 0.0 {
            let node = nodes
                .iter()
                .find(|n| n.get("visible").and_then(Value::as_bool).unwrap_or(false))
                .or_else(|| nodes.first())
                .cloned()
                .unwrap_or(json!({}));
            // sitekey from the widget's iframe url when the container has none
            let key_from_url = hit_frame.as_deref().and_then(|u| extract_key(u, w.kind));
            scored.push((
                score,
                w.kind.to_string(),
                markers,
                node.get("key")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
                    .or(key_from_url),
                hit_frame,
                node.get("id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
                node.get("visible")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                hit_window || hit_script.is_some(),
                hit_window.then(|| w.win.unwrap().to_string()),
            ));
        }
    }
    if scored.is_empty() {
        return Ok(ChallengeInfo {
            kind: None,
            sitekey: None,
            iframe_url: None,
            container_id: None,
            visible: None,
            token_present: None,
            token_length: None,
            script_loaded: None,
            window_signal: None,
            markers: None,
            waited: None,
        });
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let best = &scored[0];
    let kind = best.1.clone();
    // token length for the winner
    let token_len = token_length(page, &kind).await.unwrap_or(0);
    Ok(ChallengeInfo {
        kind: Some(kind),
        sitekey: best.3.clone(),
        iframe_url: best.4.clone(),
        container_id: best.5.clone(),
        visible: Some(best.6),
        token_present: Some(token_len > 8),
        token_length: Some(token_len),
        script_loaded: Some(best.7),
        window_signal: best.8.clone(),
        markers: Some(best.2.clone()),
        waited: None,
    })
}

/// SPAs hydrate the widget seconds after first paint — a bare snapshot reads
/// "no captcha here" on a page that has one. `{ wait }` polls until it mounts.
pub async fn detect(
    page: &Arc<Page>,
    wait: bool,
    timeout: Duration,
    poll: Duration,
) -> Result<ChallengeInfo> {
    let t0 = Instant::now();
    let mut info = detect_once(page).await?;
    if wait && info.kind.is_none() {
        while info.kind.is_none() && t0.elapsed() < timeout {
            tokio::time::sleep(poll).await;
            info = detect_once(page).await?;
        }
    }
    if wait {
        info.waited = Some(t0.elapsed().as_millis());
    }
    Ok(info)
}

/// Wait for a captcha token field to populate. On timeout it reports WHICH state
/// it died in instead of a silent null.
pub async fn wait_for_token(
    page: &Arc<Page>,
    selector: Option<&str>,
    timeout: Duration,
    poll: Duration,
    detect_timeout: Duration,
) -> Result<String> {
    let sel: String = match selector {
        Some(s) => s.to_string(),
        None => {
            let info = detect(page, true, detect_timeout, Duration::from_millis(500)).await?;
            let kind = info.kind.as_deref();
            let widget = match kind.and_then(|k| WIDGETS.iter().find(|w| w.kind == k)) {
                Some(w) => Some(w),
                None => last_resort(page).await,
            };
            match widget {
                Some(w) => w.token_fields.join(","),
                None => {
                    // nothing captcha-like at all → keep polling detect until timeout
                    let deadline = Instant::now() + timeout;
                    while Instant::now() < deadline {
                        tokio::time::sleep(poll).await;
                        let again = detect_once(page).await?;
                        if let Some(k) = &again.kind
                            && let Some(w) = WIDGETS.iter().find(|w| w.kind == k)
                        {
                            return wait_on_selectors(
                                page,
                                &w.token_fields.join(","),
                                deadline,
                                poll,
                            )
                            .await;
                        }
                    }
                    return Err(anyhow!(
                        "waitForCaptchaToken: timed out after {:?} — state: no captcha-like element ever appeared",
                        timeout
                    ));
                }
            }
        }
    };
    wait_on_selectors(page, &sel, Instant::now() + timeout, poll).await
}

async fn wait_on_selectors(
    page: &Arc<Page>,
    joined: &str,
    deadline: Instant,
    poll: Duration,
) -> Result<String> {
    let selectors: Vec<&str> = joined.split(',').collect();
    let first = selectors[0];
    let t0 = Instant::now();
    let mut last = json!({ "found": false, "tokenLength": 0, "widget": 0, "scriptLoaded": false });
    while Instant::now() < deadline {
        let expr = format!(
            r#"(function(){{
                var any = null;
                var sels = {};
                for (var i = 0; i < sels.length; i++) {{ var e = document.querySelector(sels[i]); if (e) {{ any = e; break; }} }}
                var nodes = document.querySelectorAll('iframe, [class*="captcha"], [id^="turnstile"], [id^="g-recaptcha"], [id^="hcaptcha"], [id^="arkose"], [id^="awswaf"], [id^="px-"]');
                return {{
                    found: !!any,
                    tokenLength: any ? String(any.value || '').length : 0,
                    widget: nodes.length,
                    scriptLoaded: Array.prototype.some.call(document.scripts, function(s){{ return /captcha|turnstile|recaptcha|hcaptcha|arkose|waf|px-cloud|arkoselabs/i.test(s.src || ''); }}),
                }};
            }})()"#,
            serde_json::to_string(&selectors).unwrap()
        );
        let state = page.eval(&expr).await.unwrap_or_else(|_| last.clone());
        last = state.clone();
        let found = state.get("found").and_then(Value::as_bool).unwrap_or(false);
        let len = state
            .get("tokenLength")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if found && len > 8 {
            let value = page
                .eval(&format!(
                    "(function(){{var e=document.querySelector({}); return e ? e.value : null}})()",
                    serde_json::to_string(first).unwrap()
                ))
                .await?;
            if let Some(v) = value.as_str() {
                return Ok(v.to_string());
            }
        }
        tokio::time::sleep(poll).await;
    }
    let found = last.get("found").and_then(Value::as_bool).unwrap_or(false);
    let len = last.get("tokenLength").and_then(Value::as_u64).unwrap_or(0);
    let widget = last.get("widget").and_then(Value::as_u64).unwrap_or(0);
    let loaded = last
        .get("scriptLoaded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reason = if found {
        if len > 0 {
            "token field present but stayed empty/too short (challenge not solved)"
        } else {
            "token field present but never populated"
        }
    } else if widget > 0 {
        "widget present but the token field is missing (wrong selector?)"
    } else if loaded {
        "captcha script loaded but the widget never rendered"
    } else {
        "captcha script never loaded (blocked, wrong domain, or never requested)"
    };
    Err(anyhow!(
        "waitForCaptchaToken: timed out after {:?} — state: {}",
        t0.elapsed(),
        reason
    ))
}

/// No widget detected at all: use recaptcha's field only when the page shows
/// any captcha-ish node — never guess a wrong token field.
async fn last_resort(page: &Arc<Page>) -> Option<&'static Widget> {
    let n = page
        .eval(r#"(function(){
            var n = document.querySelectorAll('[class*="captcha"],[id*="captcha"],[class*="challenge"],[id^="turnstile"],[id^="g-recaptcha"],[id^="hcaptcha"],[id^="arkose"],[id^="px-"],iframe[src*="captcha"],iframe[src*="turnstile"],iframe[src*="recaptcha"],iframe[src*="hcaptcha"],iframe[src*="arkose"],iframe[src*="awswaf"]');
            return n.length;
        })()"#)
        .await
        .ok()
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if n > 0 { Some(&WIDGETS[0]) } else { None }
}

async fn token_length(page: &Arc<Page>, kind: &str) -> Result<u64> {
    let fields = WIDGETS
        .iter()
        .find(|w| w.kind == kind)
        .map(|w| w.token_fields)
        .unwrap_or(&[]);
    let expr = format!(
        r#"(function(){{
            var sels = {};
            for (var i = 0; i < sels.length; i++) {{ var e = document.querySelector(sels[i]); if (e) return String(e.value || '').length; }}
            return 0;
        }})()"#,
        serde_json::to_string(&fields).unwrap()
    );
    let v = page.eval(&expr).await.unwrap_or(Value::Null);
    Ok(v.as_u64().unwrap_or(0))
}

fn str_list(a: &[Value]) -> Vec<String> {
    a.iter()
        .filter_map(Value::as_str)
        .map(|s| s.to_string())
        .collect()
}

fn extract_key(url: &str, kind: &str) -> Option<String> {
    let key_re: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"[?&](?:sitekey|k|pk|pkey)=([\w-]+)").unwrap());
    match kind {
        "recaptcha" | "hcaptcha" | "arkose" => key_re.captures(url).map(|c| c[1].to_string()),
        _ => None,
    }
}
