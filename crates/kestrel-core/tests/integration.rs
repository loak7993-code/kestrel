// kestrel integration tests — run against the built-in test site (pure Rust,
// no node dependency). CDP tests need a browser: VELOX_BROWSER=/path cargo test
use kestrel_core::{OpenOpts, Session};
use std::time::Duration;

fn opts() -> OpenOpts {
    OpenOpts {
        timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

#[tokio::test]
async fn lite_fetch_and_dom() {
    let site = kestrel_core::testsite::start();
    let s = Session::open(&format!("{}/", site.url), opts())
        .await
        .unwrap();
    assert_eq!(s.engine(), "lite");
    assert_eq!(s.title().await.unwrap(), "Kestrel Test Site");
    assert_eq!(s.text("h1").await.unwrap().unwrap(), "Welcome to Kestrel");
    let links = s.links().await.unwrap();
    assert!(links.iter().any(|l| l.href.contains("/page2.html")));
    let tables = s.tables().await.unwrap();
    assert_eq!(tables[0]["rows"][0][0], serde_json::json!("7"));
    s.close().await;
}

#[tokio::test]
async fn needs_js_escalates() {
    if std::env::var("VELOX_BROWSER")
        .unwrap_or_default()
        .is_empty()
    {
        eprintln!("skipping escalation test (VELOX_BROWSER not set)");
        return;
    }
    let site = kestrel_core::testsite::start();
    let s = Session::open(&site.url, opts()).await.unwrap();
    assert_eq!(s.engine(), "lite");
    s.close().await;
    let s = Session::open(&format!("{}/spa", site.url), opts())
        .await
        .unwrap();
    assert_eq!(s.engine(), "cdp", "spa shell escalates");
    let (s, _) = s
        .wait_for("#js-done", Duration::from_secs(5))
        .await
        .unwrap();
    let text = s.text("#js-done").await.unwrap().unwrap();
    assert_eq!(text, "spa content ready");
    s.close().await;
}

#[tokio::test]
async fn cdp_eval_and_screenshot() {
    let site = kestrel_core::testsite::start();
    let exe = std::env::var("VELOX_BROWSER").unwrap_or_default();
    if exe.is_empty() {
        eprintln!("skipping CDP test (VELOX_BROWSER not set)");
        return;
    }
    let s = Session::open(&format!("{}/", site.url), opts())
        .await
        .unwrap();
    let (s, v) = s
        .eval("document.querySelectorAll('li.item').length")
        .await
        .unwrap();
    assert_eq!(v.as_u64(), Some(2));
    let (s, png) = s.screenshot(false).await.unwrap();
    assert!(png.len() > 1000);
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let (s, pdf) = s.pdf(Some("A4"), false).await.unwrap();
    assert_eq!(&pdf[..4], b"%PDF");
    s.close().await;
}

#[tokio::test]
async fn cookies_roundtrip() {
    let site = kestrel_core::testsite::start();
    let exe = std::env::var("VELOX_BROWSER").unwrap_or_default();
    if exe.is_empty() {
        eprintln!("skipping cookie test (VELOX_BROWSER not set)");
        return;
    }
    let o = OpenOpts {
        engine: Some("cdp".into()),
        ..opts()
    };
    let s = Session::open(&format!("{}/setcookie", site.url), o)
        .await
        .unwrap();
    let cookies = s.cookies().await.unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c.name == "fromkestrel" && c.value == "yes")
    );
    s.close().await;
}

#[test]
fn discovery_finds_a_browser() {
    if std::env::var("VELOX_BROWSER")
        .unwrap_or_default()
        .is_empty()
        && std::env::var("PATH").is_ok()
        && cfg!(not(target_os = "linux"))
    {
        // on macOS/Windows runners no Chromium is preinstalled and the env is
        // not set — discovery legitimately finds nothing there
        eprintln!("skipping discovery test (no browser on this runner)");
        return;
    }
    let _ = kestrel_core::find_browser(None).expect("a browser resolves");
}

#[test]
fn stealth_profile_coherence() {
    use kestrel_core::cdp::stealth::{StealthOpts, resolve};
    let r = resolve(&StealthOpts::default()).unwrap();
    assert_eq!(r.profile.name, "desktop-windows");
    let r = resolve(&StealthOpts {
        geo: Some("de-DE".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(r.timezone, "Europe/Berlin");
    assert_eq!(r.accept_language, "de-DE,de;q=0.9,en;q=0.8");
}

#[test]
fn quickjs_scripts_end_to_end() {
    // needs a browser (k.open cdp) and /tmp paths
    if std::env::var("VELOX_BROWSER")
        .unwrap_or_default()
        .is_empty()
    {
        eprintln!("skipping quickjs script test (VELOX_BROWSER not set)");
        return;
    }
    // spawn a test site on a thread, run a kestrel script against it
    std::thread::spawn(|| {
        kestrel_core::testsite::serve_forever(47890);
    });
    // wait for the port
    std::thread::sleep(std::time::Duration::from_millis(150));
    let dir = std::env::temp_dir().join("kestrel-test-script.js");
    std::fs::write(
        &dir,
        r#"
const page = k.open("http://127.0.0.1:47890/", { engine: "cdp" });
k.log("title:", page.title());
const n = page.count("li.item");
if (n !== 2) throw new Error("expected 2 items, got " + n);
const shot = page.screenshot({ full: true });
if (shot.bytes < 1000) throw new Error("screenshot too small");
await_save_and_check();
function await_save_and_check() {
  k.save("/tmp/kestrel-js-shot.png", shot);
}
page.close();
k.log("script ok");
"#,
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _g = rt.enter();
    let (code, out) = kestrel_core::js::run_source(
        &std::fs::read_to_string(&dir).unwrap(),
        vec![],
        kestrel_core::js::GlobalOpts {
            timeout_ms: Some(20000),
            ..Default::default()
        },
        Some(dir.to_string_lossy().to_string()),
    )
    .expect("script runs");
    assert_eq!(code, 0);
    assert!(out.contains("title: Kestrel Test Site"), "out: {out}");
    assert!(out.contains("script ok"), "out: {out}");
    assert!(std::path::Path::new("/tmp/kestrel-js-shot.png").exists());
}

#[tokio::test]
async fn challenge_engage_solves_and_clears() {
    if std::env::var("VELOX_BROWSER")
        .unwrap_or_default()
        .is_empty()
    {
        eprintln!("skipping engage test (VELOX_BROWSER not set)");
        return;
    }
    let site = kestrel_core::testsite::start();
    let o = OpenOpts {
        engine: Some("cdp".into()),
        ..opts()
    };
    let s = Session::open(&format!("{}/challenge", site.url), o)
        .await
        .unwrap();
    let (s, res) = s
        .engage_challenge(Duration::from_secs(15), true)
        .await
        .unwrap();
    assert_eq!(res.kind.as_deref(), Some("turnstile"), "widget detected");
    assert!(
        res.acted.iter().any(|a| a.contains("click")),
        "acted: {:?}",
        res.acted
    );
    assert!(res.cleared || res.token_present, "solved: {:?}", res);
    assert!(!res.timeout, "engaged within 15s: {:?}", res);
    // clearance cookie landed in the jar
    let cookies = s.cookies().await.unwrap();
    assert!(
        cookies.iter().any(|c| c.name == "cf_clearance"),
        "cookies: {:?}",
        cookies.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    );
    s.close().await;
}

#[tokio::test]
async fn pool_maps_urls() {
    if std::env::var("VELOX_BROWSER")
        .unwrap_or_default()
        .is_empty()
    {
        eprintln!("skipping pool test (VELOX_BROWSER not set)");
        return;
    }
    let site = kestrel_core::testsite::start();
    let pool = kestrel_core::pool::Pool::start(kestrel_core::pool::PoolOpts {
        browsers: 2,
        max_concurrency: 4,
        ..Default::default()
    })
    .await
    .unwrap();
    let urls: Vec<String> = ["/", "/page2.html", "/", "/page2.html"]
        .iter()
        .map(|p| format!("{}{p}", site.url))
        .collect();
    let titles = pool
        .map(urls, |url, page| async move {
            page.goto(
                &url,
                kestrel_core::cdp::page::GotoOpts {
                    wait_until: Some(kestrel_core::cdp::page::WaitUntil::Interactive),
                    timeout: Some(Duration::from_secs(20)),
                    referer: None,
                },
            )
            .await?;
            page.title().await
        })
        .await
        .unwrap();
    assert_eq!(titles.len(), 4, "all items processed: {:?}", titles);
    let ok = titles
        .iter()
        .all(|t| t == "Kestrel Test Site" || t == "Page Two");
    assert!(ok, "titles: {:?}", titles);
    pool.close().await;
}

#[tokio::test]
async fn cookie_carry_over_lite_to_cdp() {
    let site = kestrel_core::testsite::start();
    // lite fetch hits /setcookie → the cookie must land in the escalated
    // browser session BEFORE its first navigation
    let s = Session::open(&format!("{}/setcookie", site.url), opts())
        .await
        .unwrap();
    assert_eq!(s.engine(), "lite");
    let (s, _) = s.wait_for("body", Duration::from_secs(5)).await.unwrap();
    let cookies = s.cookies().await.unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c.name == "fromkestrel" && c.value == "yes"),
        "lite jar carried over: {:?}",
        cookies.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    );
    s.close().await;
}
