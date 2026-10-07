// kestrel :: needs_js — the escalation heuristics (ported from src/lite/engine.js).
use once_cell::sync::Lazy;
use regex::Regex;

static SPA_ROOT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)<div[^>]+id=["']?(root|app|__next|__nuxt|q-app|svelte|elm|vue-app)["']?[^>]*>\s*(</div>|<!--.*?-->)"#).unwrap()
});
static SSR_STATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"__NEXT_DATA__|__NUXT__|__INITIAL_STATE__|__APOLLO_STATE__|window\.__PRELOADED|application/ld\+json"#).unwrap()
});
static NOSCRIPT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<noscript[^>]*>[\s\S]{0,400}(enable|turn on|requires) javascript"#).unwrap()
});
static META_REFRESH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)<meta[^>]+http-equiv=["']?refresh["']?[^>]+url="#).unwrap());
static SCRIPT_SRC: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<script[^>]+src=").unwrap());
static TAGS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static BOT_SERVERS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)cloudflare|akamai|fastly").unwrap());

pub fn strip_tags(html: &str) -> String {
    TAGS.replace_all(html, "").to_string()
}

/// True when the page needs a real browser (escalate from the lite engine).
pub fn needs_js(status: u16, server: Option<&str>, cf_mitigated: bool, html: &str) -> bool {
    if status == 403 || status == 503 {
        let bot_server = server.map(|s| BOT_SERVERS.is_match(s)).unwrap_or(false);
        if bot_server || cf_mitigated {
            return true;
        }
    }
    if html.len() < 4000 && SPA_ROOT.is_match(html) && !SSR_STATE.is_match(html) {
        return true;
    }
    let body_text_len = strip_tags(html).trim().len();
    let scripts = SCRIPT_SRC.find_iter(html).count();
    if body_text_len < 250 && scripts >= 2 && !SSR_STATE.is_match(html) {
        return true;
    }
    if NOSCRIPT.is_match(html) && body_text_len < 500 {
        return true;
    }
    if META_REFRESH.is_match(html) && body_text_len < 300 {
        return true;
    }
    false
}
