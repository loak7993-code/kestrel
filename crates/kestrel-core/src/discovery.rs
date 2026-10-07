// kestrel :: discovery — find any installed Chromium-family browser.
// Ported from src/cdp/discovery.js (same preference order, same env override).
use anyhow::{Result, anyhow};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FoundBrowser {
    pub name: String,
    pub path: String,
}

fn exists(p: &str) -> bool {
    let path = Path::new(p);
    path.exists() && is_executable(path)
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Look in the usual install locations for each browser family.
pub fn discover() -> Vec<FoundBrowser> {
    let candidates: &[(&str, &[&str])] = &[
        ("chrome-headless-shell", &["chrome-headless-shell"]),
        ("google-chrome", &["google-chrome", "google-chrome-stable"]),
        ("chromium", &["chromium", "chromium-browser"]),
        (
            "microsoft-edge",
            &["microsoft-edge", "microsoft-edge-stable"],
        ),
        ("brave-browser", &["brave-browser", "brave"]),
        ("vivaldi", &["vivaldi", "vivaldi-stable"]),
        ("opera", &["opera"]),
    ];
    let mut dirs: Vec<String> = vec![];
    for d in [
        "/usr/bin",
        "/usr/local/bin",
        "/snap/bin",
        "/opt/google/chrome",
        "/opt/microsoft/msedge",
    ] {
        dirs.push(d.to_string());
    }
    if let Ok(home) = std::env::var("HOME") {
        for extra in [
            "/.local/bin",
            "/google/chrome",
            "/microsoft/msedge",
            "/brave-browser/brave-browser",
            "/vivaldi",
        ] {
            dirs.push(format!("{home}{extra}"));
        }
    }
    let mut out: Vec<FoundBrowser> = vec![];
    for (name, bins) in candidates {
        for bin in *bins {
            for dir in &dirs {
                let p = format!("{dir}/{bin}");
                if exists(&p) && !out.iter().any(|f| f.path == p) {
                    out.push(FoundBrowser {
                        name: name.to_string(),
                        path: p,
                    });
                }
            }
        }
    }
    // $PATH fallback (covers custom installs)
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            for (name, bins) in candidates {
                for bin in *bins {
                    let p = format!("{dir}/{bin}");
                    if exists(&p) && !out.iter().any(|f| f.path == p) {
                        out.push(FoundBrowser {
                            name: name.to_string(),
                            path: p,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Resolve a preference (name, path, or "auto") to an executable path.
/// $VELOX_BROWSER is the default, exactly like the JS package.
pub fn find_browser(pref: Option<&str>) -> Result<String> {
    let env_browser = std::env::var("VELOX_BROWSER")
        .ok()
        .filter(|s| !s.is_empty());
    let choice = pref
        .filter(|p| !p.is_empty() && *p != "auto")
        .map(|s| s.to_string())
        .or(env_browser);
    if let Some(choice) = choice {
        if choice.contains('/') || choice.contains('\\') {
            if exists(&choice) {
                return Ok(choice);
            }
            return Err(anyhow!("Browser not found: {choice}"));
        }
        let all = discover();
        let hit = all
            .iter()
            .find(|b| b.name == choice)
            .or_else(|| all.iter().find(|b| b.name.contains(&choice)));
        return hit.map(|b| b.path.clone()).ok_or_else(|| {
            anyhow!(
                "Browser \"{choice}\" not found. Available: {}",
                all.iter()
                    .map(|b| b.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    let all = discover();
    const ORDER: [&str; 7] = [
        "google-chrome",
        "chromium",
        "microsoft-edge",
        "brave-browser",
        "vivaldi",
        "opera",
        "chrome-headless-shell",
    ];
    for want in ORDER {
        if let Some(hit) = all.iter().find(|b| b.name.contains(want)) {
            return Ok(hit.path.clone());
        }
    }
    if let Some(first) = all.first() {
        return Ok(first.path.clone());
    }
    Err(anyhow!(
        "No Chromium-family browser found. Install Chrome, Chromium, Edge, Brave, Vivaldi or Opera, or set VELOX_BROWSER=/path/to/browser"
    ))
}
