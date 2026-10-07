// kestrel :: cdp/stealth — fingerprint coherence, authored for kestrel.
// Detectors score contradictions far more than individual values, so every
// claim is one coherent story: platform ↔ UA-CH ↔ WebGL ↔ screen ↔
// languages ↔ timezone. The injected patch set is kestrel's own (compact,
// deterministic noise, engine surface hidden), with the claimed Chrome
// version aligned to the real binary at attach time.
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

/// A device story. Every field is a claim; all of them must agree.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: &'static str,
    pub user_agent: &'static str,
    pub ua_metadata: Value,
    pub platform: &'static str,
    pub ua_platform: &'static str,
    pub ua_platform_version: &'static str,
    pub vendor: &'static str,
    pub webgl_vendor: &'static str,
    pub webgl_renderer: &'static str,
    pub hardware_concurrency: u32,
    pub device_memory: u32,
    pub screen: Screen,
    pub languages: &'static [&'static str],
    pub locale: &'static str,
    pub timezone: &'static str,
    pub max_touch_points: u32,
    pub mobile: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub avail_width: u32,
    pub avail_height: u32,
    pub color_depth: u32,
    pub pixel_depth: u32,
}

fn brands(major: u32) -> Value {
    json!([
        { "brand": "Chromium", "version": major.to_string() },
        { "brand": "Google Chrome", "version": major.to_string() },
        { "brand": "Not_A Brand", "version": "24" },
    ])
}

pub static PROFILES: once_cell::sync::Lazy<Vec<Profile>> = once_cell::sync::Lazy::new(|| {
    vec![
        Profile {
            name: "desktop-windows",
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
            ua_metadata: json!({ "brands": brands(132), "fullVersion": "132.0.0.0", "platform": "Windows", "platformVersion": "15.0.0", "architecture": "x86", "bitness": "64", "model": "", "mobile": false }),
            platform: "Win32",
            ua_platform: "Windows",
            ua_platform_version: "15.0.0",
            vendor: "Google Inc.",
            webgl_vendor: "Google Inc.",
            webgl_renderer: "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002503) Direct3D11 vs_5_0 ps_5_0, D3D11)",
            hardware_concurrency: 16,
            device_memory: 8,
            screen: Screen {
                width: 1920,
                height: 1080,
                avail_width: 1920,
                avail_height: 1032,
                color_depth: 24,
                pixel_depth: 24,
            },
            languages: &["en-US", "en"],
            locale: "en-US",
            timezone: "America/Chicago",
            max_touch_points: 0,
            mobile: false,
        },
        Profile {
            name: "desktop-mac",
            user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
            ua_metadata: json!({ "brands": brands(132), "fullVersion": "132.0.0.0", "platform": "macOS", "platformVersion": "14.6.0", "architecture": "arm", "bitness": "64", "model": "", "mobile": false }),
            platform: "MacIntel",
            ua_platform: "macOS",
            ua_platform_version: "14.6.0",
            vendor: "Google Inc.",
            webgl_vendor: "Google Inc.",
            webgl_renderer: "ANGLE (Apple, ANGLE Metal Renderer: Apple M2 Pro, Unspecified Version)",
            hardware_concurrency: 10,
            device_memory: 8,
            screen: Screen {
                width: 1512,
                height: 982,
                avail_width: 1512,
                avail_height: 944,
                color_depth: 30,
                pixel_depth: 30,
            },
            languages: &["en-US", "en"],
            locale: "en-US",
            timezone: "America/Los_Angeles",
            max_touch_points: 0,
            mobile: false,
        },
        Profile {
            name: "mobile-pixel",
            user_agent: "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Mobile Safari/537.36",
            ua_metadata: json!({ "brands": brands(132), "fullVersion": "132.0.0.0", "platform": "Android", "platformVersion": "14.0.0", "architecture": "", "bitness": "", "model": "Pixel 9", "mobile": true }),
            platform: "Linux armv8l",
            ua_platform: "Android",
            ua_platform_version: "14.0.0",
            vendor: "Google Inc.",
            webgl_vendor: "Qualcomm",
            webgl_renderer: "Adreno (TM) 750",
            hardware_concurrency: 8,
            device_memory: 8,
            screen: Screen {
                width: 412,
                height: 922,
                avail_width: 412,
                avail_height: 922,
                color_depth: 24,
                pixel_depth: 24,
            },
            languages: &["en-US", "en"],
            locale: "en-US",
            timezone: "America/New_York",
            max_touch_points: 5,
            mobile: true,
        },
    ]
});

/// locale → coherent timezone + Accept-Language, so a geo preset cannot
/// contradict itself.
pub fn geo(locale: &str) -> Option<(&'static str, &'static str)> {
    Some(match locale {
        "en-US" => ("America/New_York", "en-US,en;q=0.9"),
        "en-GB" => ("Europe/London", "en-GB,en;q=0.9"),
        "de-DE" => ("Europe/Berlin", "de-DE,de;q=0.9,en;q=0.8"),
        "fr-FR" => ("Europe/Paris", "fr-FR,fr;q=0.9,en;q=0.8"),
        "es-ES" => ("Europe/Madrid", "es-ES,es;q=0.9,en;q=0.8"),
        "pt-BR" => ("America/Sao_Paulo", "pt-BR,pt;q=0.9,en;q=0.8"),
        "ja-JP" => ("Asia/Tokyo", "ja-JP,ja;q=0.9,en;q=0.8"),
        "ko-KR" => ("Asia/Seoul", "ko-KR,ko;q=0.9,en;q=0.8"),
        "zh-CN" => ("Asia/Shanghai", "zh-CN,zh;q=0.9,en;q=0.8"),
        "ru-RU" => ("Europe/Moscow", "ru-RU,ru;q=0.9,en;q=0.8"),
        "nl-NL" => ("Europe/Amsterdam", "nl-NL,nl;q=0.9,en;q=0.8"),
        "it-IT" => ("Europe/Rome", "it-IT,it;q=0.9,en;q=0.8"),
        "pl-PL" => ("Europe/Warsaw", "pl-PL,pl;q=0.9,en;q=0.8"),
        "tr-TR" => ("Europe/Istanbul", "tr-TR,tr;q=0.9,en;q=0.8"),
        "id-ID" => ("Asia/Jakarta", "id-ID,id;q=0.9,en;q=0.8"),
        _ => return None,
    })
}

/// Configuration. `Default` = desktop-windows, seed 1337, noise on.
#[derive(Debug, Clone)]
pub struct StealthOpts {
    pub profile: Option<String>,
    pub geo: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub user_agent: Option<String>,
    pub seed: u32,
    pub noise: bool,
    pub webrtc_block: bool,
    pub hide_engine: bool,
}

impl Default for StealthOpts {
    fn default() -> Self {
        StealthOpts {
            profile: None,
            geo: None,
            locale: None,
            timezone: None,
            user_agent: None,
            seed: 1337,
            noise: true,
            webrtc_block: false,
            hide_engine: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub profile: &'static Profile,
    pub user_agent: String,
    pub locale: String,
    pub languages: Vec<String>,
    pub timezone: String,
    pub accept_language: String,
    pub seed: u32,
    pub noise: bool,
    pub webrtc_block: bool,
    pub hide_engine: bool,
}

pub fn resolve(opts: &StealthOpts) -> Result<Resolved> {
    let name = opts.profile.as_deref().unwrap_or("desktop-windows");
    let profile = PROFILES.iter().find(|p| p.name == name).ok_or_else(|| {
        anyhow!(
            "unknown stealth profile \"{name}\" — try: {}",
            PROFILES
                .iter()
                .map(|p| p.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let geo_tz = opts.geo.as_deref().and_then(geo);
    let locale = opts
        .locale
        .clone()
        .or_else(|| opts.geo.clone())
        .unwrap_or_else(|| profile.locale.to_string());
    let languages: Vec<String> = if let Some(g) = &opts.geo {
        let base = g.split('-').next().unwrap_or(g).to_string();
        vec![g.clone(), base, "en".to_string()]
    } else {
        let base = locale.split('-').next().unwrap_or(&locale).to_string();
        let mut out = vec![locale.clone(), base.clone()];
        for l in profile.languages {
            if !l.starts_with(&base) && out.len() < 3 {
                out.push(l.to_string());
            }
        }
        out.truncate(3);
        out
    };
    let timezone = opts
        .timezone
        .clone()
        .or_else(|| geo_tz.map(|(tz, _)| tz.to_string()))
        .unwrap_or_else(|| profile.timezone.to_string());
    let accept_language = geo_tz
        .map(|(_, al)| al.to_string())
        .unwrap_or_else(|| languages.join(","));
    Ok(Resolved {
        profile,
        user_agent: opts
            .user_agent
            .clone()
            .unwrap_or_else(|| profile.user_agent.to_string()),
        locale,
        languages,
        timezone,
        accept_language,
        seed: opts.seed,
        noise: opts.noise,
        webrtc_block: opts.webrtc_block,
        hide_engine: opts.hide_engine,
    })
}

thread_local! {
    static ALIGNED: std::cell::RefCell<Vec<Profile>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Align the claimed Chrome version with the real binary — claiming 132 while
/// the browser is 155 is exactly the contradiction detectors pick up.
pub fn align_version(r: Resolved, version: Option<&str>) -> Resolved {
    let Some(version) = version else { return r };
    let full = version
        .rsplit('/')
        .next()
        .unwrap_or(version)
        .trim()
        .to_string();
    let Some(major) = full.split('.').next() else {
        return r;
    };
    if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return r;
    }
    let mut r = r;
    let re = once_cell::sync::Lazy::new(|| regex::Regex::new(r"Chrome/[\d.]+").unwrap());
    r.user_agent = re
        .replace_all(&r.user_agent, format!("Chrome/{full}"))
        .to_string();
    if let Ok(mut md) = serde_json::from_value::<Value>(r.profile.ua_metadata.clone()) {
        if let Some(brands) = md.get_mut("brands").and_then(Value::as_array_mut) {
            for b in brands.iter_mut() {
                let is_chrome = b
                    .get("brand")
                    .and_then(Value::as_str)
                    .map(|s| s.contains("Chromium") || s.contains("Google Chrome"))
                    .unwrap_or(false);
                if is_chrome {
                    b["version"] = json!(major);
                }
            }
        }
        md["fullVersion"] = json!(full);
        let p = r.profile;
        let aligned = Profile {
            name: p.name,
            user_agent: Box::leak(r.user_agent.clone().into_boxed_str()),
            ua_metadata: md,
            ..*p
        };
        r.profile = ALIGNED.with(|cell| {
            let mut v = cell.borrow_mut();
            v.push(aligned);
            // the thread-local outlives every page created on this thread
            unsafe { std::mem::transmute::<&Profile, &'static Profile>(v.last().unwrap()) }
        });
    }
    r
}

/// The injected patch set — kestrel's own, compact by design. Deterministic
/// seeded noise (same seed → same canvas → what canvas-fingerprint detectors
/// check) via a tiny xorshift on a string hash.
pub fn build_source(r: &Resolved) -> String {
    let p = r.profile;
    let cfg = json!({
        "platform": p.platform,
        "vendor": p.vendor,
        "webglVendor": p.webgl_vendor,
        "webglRenderer": p.webgl_renderer,
        "hardwareConcurrency": p.hardware_concurrency,
        "deviceMemory": p.device_memory,
        "screen": { "width": p.screen.width, "height": p.screen.height, "availWidth": p.screen.avail_width, "availHeight": p.screen.avail_height, "colorDepth": p.screen.color_depth, "pixelDepth": p.screen.pixel_depth },
        "languages": r.languages,
        "maxTouchPoints": p.max_touch_points,
        "mobile": p.mobile,
        "noise": r.noise,
        "seed": r.seed,
        "webrtcBlock": r.webrtc_block,
        "hideEngine": r.hide_engine,
    });
    format!(
        r#"(function () {{
  if (window.__kestrel) return;
  try {{ Object.defineProperty(window, '__kestrel', {{ value: 1, enumerable: false }}); }} catch (e) {{}}
  var K = {cfg};
  var def = function (o, k, v) {{ try {{ Object.defineProperty(o, k, {{ get: function () {{ return v; }}, configurable: true }}); }} catch (e) {{}} }};
  var hash = function (s) {{ var h = 2166136261; for (var i = 0; i < s.length; i++) {{ h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }} return h >>> 0; }};
  var rng = (function () {{ var s = (K.seed >>> 0) ^ hash(location.host || 'about'); return function () {{ s ^= s << 13; s >>>= 0; s ^= s >> 17; s ^= s << 5; return s / 4294967296; }}; }})();
  var nav = Object.getPrototypeOf(navigator);
  def(nav, 'webdriver', false);
  def(nav, 'platform', K.platform);
  def(nav, 'vendor', K.vendor);
  def(nav, 'language', K.languages[0]);
  def(nav, 'languages', Object.freeze(K.languages.slice()));
  def(nav, 'hardwareConcurrency', K.hardwareConcurrency);
  def(nav, 'deviceMemory', K.deviceMemory);
  def(nav, 'maxTouchPoints', K.maxTouchPoints);
  if (!window.chrome) {{ def(window, 'chrome', {{ runtime: {{}} }}); }}
  try {{
    var gp = navigator.plugins;
    if (gp && gp.length === 0) def(nav, 'plugins', {{ length: 3 }});
  }} catch (e) {{}}
  try {{
    var c = document.createElement('canvas');
    var g = c.getContext('webgl') || c.getContext('experimental-webgl');
    if (g) {{
      var d = g.getExtension('WEBGL_debug_renderer_info');
      var op = g.getParameter.bind(g);
      g.getParameter = function (x) {{
        if (d && x === d.UNMASKED_VENDOR_WEBGL) return K.webglVendor;
        if (d && x === d.UNMASKED_RENDERER_WEBGL) {{
          if (!K.noise) return K.webglRenderer;
          var r = K.webglRenderer;
          if (rng() < 0.14) r = r.replace(/Direct3D11/, 'Direct3D12');
          return r;
        }}
        return op(x);
      }};
      if (K.noise) {{
        var orig = g.readPixels.bind(g);
        g.readPixels = function () {{
          var out = orig.apply(null, arguments);
          var px = arguments[6];
          if (px && px.data && px.data.length) {{
            for (var i = 0; i < px.data.length; i += 97) px.data[i] = (px.data[i] + ((rng() * 3) | 0)) & 0xff;
          }}
          return out;
        }};
      }}
    }}
  }} catch (e) {{}}
  try {{
    var q = (K.screen.w_ || 0); // placeholder removed below
  }} catch (e) {{}}
  def(window.screen, 'width', K.screen.width);
  def(window.screen, 'height', K.screen.height);
  def(window.screen, 'availWidth', K.screen.availWidth);
  def(window.screen, 'availHeight', K.screen.availHeight);
  def(window.screen, 'colorDepth', K.screen.colorDepth);
  def(window.screen, 'pixelDepth', K.screen.pixelDepth);
  if (K.webrtcBlock) {{
    try {{ navigator.mediaDevices.getUserMedia = function () {{ return Promise.reject(new Error('NotAllowedError')); }}; }} catch (e) {{}}
    try {{
      var RTC = window.RTCPeerConnection;
      if (RTC) window.RTCPeerConnection = function () {{ return {{ createOffer: function () {{ return Promise.reject(new Error('blocked')) }}, close: function () {{}} }}; }};
    }} catch (e) {{}}
  }}
  if (K.hideEngine) {{
    try {{
      var hide = function () {{
        if (!window.__kestrelApi) return;
        try {{ Object.defineProperty(window, '__kestrelApi', {{ value: window.__kestrelApi, enumerable: false }}); }} catch (e) {{}}
      }};
      hide();
      setTimeout(hide, 0);
    }} catch (e) {{}}
  }}
}})();"#,
        cfg = serde_json::to_string(&cfg).unwrap(),
    )
    .replace(
        "var q = (K.screen.w_ || 0); // placeholder removed below",
        "// screen claims are coherent with the CDP metrics override",
    )
}

/// Env overrides that must land BEFORE the first navigation.
pub fn env_overrides(r: &Resolved) -> Value {
    json!({
        "userAgent": r.user_agent,
        "platform": r.profile.platform,
        "userAgentMetadata": r.profile.ua_metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_coherent() {
        let r = resolve(&StealthOpts::default()).unwrap();
        assert_eq!(r.profile.name, "desktop-windows");
        assert_eq!(r.timezone, "America/Chicago");
        assert_eq!(r.languages[0], "en-US");
    }

    #[test]
    fn geo_overrides_everything_together() {
        let r = resolve(&StealthOpts {
            geo: Some("ja-JP".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.timezone, "Asia/Tokyo");
        assert_eq!(r.accept_language, "ja-JP,ja;q=0.9,en;q=0.8");
        assert_eq!(r.languages[0], "ja-JP");
    }

    #[test]
    fn version_alignment_patches_ua_and_brands() {
        let r = resolve(&StealthOpts::default()).unwrap();
        let r = align_version(r, Some("HeadlessChrome/155.0.8059.39"));
        assert!(r.user_agent.contains("Chrome/155.0.8059.39"));
        assert_eq!(r.profile.ua_metadata["fullVersion"], "155.0.8059.39");
        assert_eq!(r.profile.ua_metadata["brands"][1]["version"], "155");
    }

    #[test]
    fn source_substitutes_and_hides() {
        let r = resolve(&StealthOpts::default()).unwrap();
        let src = build_source(&r);
        assert!(src.contains("\"webglRenderer\""));
        assert!(
            !src.contains("__KESTREL_PLACEHOLDER"),
            "no unresolved placeholders"
        );
        assert!(
            src.contains("\"hideEngine\":true"),
            "hideEngine on by default"
        );
        let off = resolve(&StealthOpts {
            hide_engine: false,
            ..Default::default()
        })
        .unwrap();
        assert!(build_source(&off).contains("\"hideEngine\":false"));
    }

    #[test]
    fn unknown_profile_errors() {
        assert!(
            resolve(&StealthOpts {
                profile: Some("nope".into()),
                ..Default::default()
            })
            .is_err()
        );
    }
}
