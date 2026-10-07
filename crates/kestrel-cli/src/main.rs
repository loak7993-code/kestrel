// ksl — the kestrel CLI: sugar commands plus the script runner + REPL.
use anyhow::Result;
use clap::{Parser, Subcommand};
use kestrel_core::js::{self, bindings::GlobalOpts};
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "ksl",
    version,
    about = "ksl — kestrel: browser automation at native speed."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a kestrel script (k API in embedded QuickJS)
    Run {
        script: String,
        /// script args as k=v pairs (available as k.args.name)
        #[arg(last = true)]
        args: Vec<String>,
        /// request timeout in ms
        #[arg(long)]
        timeout: Option<u64>,
        /// proxy (http://… or socks5://…)
        #[arg(long)]
        proxy: Option<String>,
        /// apply coherent anti-fingerprint patches by default
        #[arg(long)]
        stealth: bool,
    },
    /// Interactive REPL over the k API
    Repl {
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Open a URL and print readable text (auto engine)
    Open {
        url: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        html: bool,
        #[arg(long)]
        sel: Option<String>,
        #[arg(long)]
        engine: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
        #[arg(long)]
        stealth: bool,
        #[arg(short, long)]
        out: Option<String>,
        #[arg(short, long)]
        quiet: bool,
        /// print per-phase timings
        #[arg(long)]
        profile: bool,
    },
    /// Screenshot → out.png
    Shot {
        url: String,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        sel: Option<String>,
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
        #[arg(long)]
        profile: bool,
    },
    /// Save the page as PDF
    Pdf {
        url: String,
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        landscape: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// List links
    Links {
        url: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Structured scrape (--recipe text|links|images|tables|meta|jsonld|all)
    Scrape {
        url: String,
        #[arg(long, default_value = "all")]
        recipe: String,
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Evaluate JS in the page
    Eval {
        url: String,
        expr: String,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Print cookies
    Cookies {
        url: String,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// What anti-bot widget is on this page?
    Challenge {
        url: String,
        #[arg(long, default_value = "true")]
        wait: bool,
        #[arg(long, default_value = "20000")]
        timeout: u64,
        /// click/solve the behavioural part of the widget, then wait for clearance
        #[arg(long)]
        engage: bool,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Captured network log
    Net {
        url: String,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// HAR 1.2 export
    Har {
        url: String,
        #[arg(long)]
        bodies: bool,
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Capture cookies + web storage to a JSON file
    SaveSession {
        url: String,
        file: String,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Open URL with a saved session restored first
    LoadSession {
        url: String,
        file: String,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        proxy: Option<String>,
    },
    /// List Chromium-family browsers found on this machine
    Detect,
    /// (internal) serve the integration-test site
    #[command(hide = true)]
    TestSite { port: u16 },
    /// Time lite vs browser on the same URL
    Bench {
        url: String,
        #[arg(long, default_value = "3")]
        iters: usize,
    },
}

fn opts(timeout: Option<u64>, proxy: Option<String>, stealth: bool) -> kestrel_core::OpenOpts {
    kestrel_core::OpenOpts {
        timeout: std::time::Duration::from_millis(timeout.unwrap_or(20000)),
        proxy,
        stealth: stealth.then(kestrel_core::cdp::stealth::StealthOpts::default),
        ..Default::default()
    }
}

fn emit(text: &str, out: &Option<String>) {
    match out {
        None => println!("{text}"),
        Some(path) => {
            if let Some(dir) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(path, text).expect("write out");
            eprintln!("→ {path}  {:.1} KB", text.len() as f64 / 1024.0);
        }
    }
}

fn main() {
    // the script runner must NOT sit inside a tokio worker: host calls in
    // scripts block on their own runtime, and nested block_on panics.
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run {
            script,
            args,
            timeout,
            proxy,
            stealth,
        } => {
            let gopts = GlobalOpts {
                timeout_ms: timeout,
                proxy: proxy.clone(),
                stealth,
                ..Default::default()
            };
            let result = std::thread::spawn(move || -> Result<i32, anyhow::Error> {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let _guard = rt.enter(); // bindings capture this handle
                js::run_script(&script, args, gopts)
            })
            .join();
            match result {
                Ok(Ok(code)) => std::process::exit(code),
                Ok(Err(e)) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
                Err(_) => {
                    eprintln!("error: script thread panicked");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Repl { timeout, proxy } => {
            let gopts = GlobalOpts {
                timeout_ms: timeout,
                proxy,
                ..Default::default()
            };
            let result = std::thread::spawn(move || -> Result<(), anyhow::Error> {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let _guard = rt.enter();
                repl(gopts)
            })
            .join();
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
                Err(_) => {
                    eprintln!("error: repl thread panicked");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            if let Err(e) = rt.block_on(run_async(cli)) {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_async(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Run { .. } | Cmd::Repl { .. } => unreachable!("sync arms are handled in main"),
        Cmd::Open {
            url,
            json,
            html,
            sel,
            engine: _,
            timeout,
            proxy,
            stealth,
            out,
            quiet: _,
            profile,
        } => {
            let t0 = Instant::now();
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, stealth)).await?;
            if profile {
                eprintln!("[profile] open: {}ms", t0.elapsed().as_millis());
            }
            let t1 = Instant::now();
            if json {
                let payload = serde_json::json!({
                    "url": s.url(), "engine": s.engine(),
                    "title": s.title().await?, "meta": s.meta().await?,
                });
                emit(&serde_json::to_string_pretty(&payload)?, &out);
            } else if html {
                emit(&s.html().await?, &out);
            } else if let Some(sel) = &sel {
                emit(&s.text(sel).await?.unwrap_or_default(), &out);
            } else {
                emit(&s.readable().await?, &out);
            }
            if profile {
                eprintln!("[profile] render: {}ms", t1.elapsed().as_millis());
            }
            s.close().await;
        }
        Cmd::Shot {
            url,
            full,
            sel,
            out,
            timeout,
            proxy,
            profile,
        } => {
            let out = out.unwrap_or_else(|| "shot.png".to_string());
            let t0 = Instant::now();
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let (s, png) = match &sel {
                Some(sel) => s.element_shot(sel).await?,
                None => s.screenshot(full).await?,
            };
            if profile {
                eprintln!("[profile] shot: {}ms", t0.elapsed().as_millis());
            }
            std::fs::write(&out, &png)?;
            println!("{out}  {:.1} KB", png.len() as f64 / 1024.0);
            s.close().await;
        }
        Cmd::Pdf {
            url,
            out,
            format,
            landscape,
            timeout,
            proxy,
        } => {
            let out = out.unwrap_or_else(|| "page.pdf".to_string());
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let (s, pdf) = s.pdf(format.as_deref(), landscape).await?;
            std::fs::write(&out, &pdf)?;
            println!("{out}  {:.1} KB", pdf.len() as f64 / 1024.0);
            s.close().await;
        }
        Cmd::Links {
            url,
            json,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let links = s.links().await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &links
                            .iter()
                            .map(|l| serde_json::json!({ "text": l.text, "href": l.href }))
                            .collect::<Vec<_>>()
                    )?
                );
            } else {
                for l in links {
                    println!(
                        "{:<62} {}",
                        l.text.chars().take(60).collect::<String>(),
                        l.href
                    );
                }
            }
            s.close().await;
        }
        Cmd::Scrape {
            url,
            recipe,
            out,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let data = match recipe.as_str() {
                "text" => serde_json::json!(s.readable().await?),
                "links" => serde_json::json!(s.links().await?),
                "images" => serde_json::json!(s.images().await?),
                "tables" => serde_json::json!(s.tables().await?),
                "meta" => serde_json::json!(s.meta().await?),
                "jsonld" => serde_json::json!(s.jsonld().await?),
                _ => serde_json::json!({
                    "url": s.url(),
                    "meta": s.meta().await?,
                    "text": s.readable().await?,
                    "links": s.links().await?,
                    "images": s.images().await?,
                    "tables": s.tables().await?,
                    "jsonld": s.jsonld().await?,
                }),
            };
            emit(&serde_json::to_string_pretty(&data)?, &out);
            s.close().await;
        }
        Cmd::Eval {
            url,
            expr,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let (s, v) = s.eval(&expr).await?;
            println!(
                "{}",
                match &v {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string_pretty(other)?,
                }
            );
            s.close().await;
        }
        Cmd::Cookies {
            url,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let cookies = s.cookies().await?;
            println!("{}", serde_json::to_string_pretty(&cookies)?);
            s.close().await;
        }
        Cmd::Challenge {
            url,
            wait,
            timeout,
            engage,
            proxy,
        } => {
            let s =
                kestrel_core::Session::open(&url, opts(timeout_nav(timeout), proxy, false)).await?;
            if engage {
                let (s, res) = s
                    .engage_challenge(std::time::Duration::from_millis(timeout), true)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
                s.close().await;
            } else {
                let (s, info) = s
                    .detect_challenge(wait, std::time::Duration::from_millis(timeout))
                    .await?;
                println!("{}", serde_json::to_string_pretty(&info)?);
                s.close().await;
            }
        }
        Cmd::Net {
            url,
            filter,
            json,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let (s, rows) = s.netlog(filter.as_deref()).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                for r in rows {
                    println!(
                        "{:<7} {:>3}  {}",
                        r.method,
                        r.status
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "-".into()),
                        r.url
                    );
                }
            }
            s.close().await;
        }
        Cmd::Har {
            url,
            bodies,
            out,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false)).await?;
            let (s, har) = s.har(bodies).await?;
            emit(&serde_json::to_string_pretty(&har)?, &out);
            s.close().await;
        }
        Cmd::SaveSession {
            url,
            file,
            timeout,
            proxy,
        } => {
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false))
                .await?
                .escalate()
                .await?;
            let (s, state) = s.save_session().await?;
            emit(&serde_json::to_string_pretty(&state)?, &Some(file.clone()));
            eprintln!("saved → {file}");
            s.close().await;
        }
        Cmd::LoadSession {
            url,
            file,
            timeout,
            proxy,
        } => {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::Error::msg(format!("read {file}: {e}")))?;
            let state: serde_json::Value = serde_json::from_str(&raw)?;
            let s = kestrel_core::Session::open(&url, opts(timeout, proxy, false))
                .await?
                .escalate()
                .await?;
            let s = s.load_session(state).await?;
            if let kestrel_core::Session::Cdp(c) = &s {
                let _ = c.page.cookies().await?; // session is live with the jar applied
            }
            let title = s.title().await.unwrap_or_default();
            println!("title: {title}");
            s.close().await;
        }
        Cmd::Detect => {
            let found = kestrel_core::discover();
            if found.is_empty() {
                println!(
                    "no Chromium-family browsers found — set VELOX_BROWSER or install Chrome/Chromium/Edge/Brave/Vivaldi/Opera"
                );
            } else {
                for b in found {
                    println!("{:<28} {}", b.name, b.path);
                }
            }
        }
        Cmd::TestSite { port } => {
            kestrel_core::testsite::serve_forever(port);
        }
        Cmd::Bench { url, iters } => {
            let mut lite = vec![];
            for _ in 0..iters {
                let t0 = Instant::now();
                let s = kestrel_core::Session::open(&url, opts(None, None, false)).await?;
                let _ = s.readable().await?;
                lite.push(t0.elapsed().as_millis());
                s.close().await;
            }
            let mut cdp = vec![];
            for _ in 0..iters {
                let t0 = Instant::now();
                let s = kestrel_core::Session::open(&url, opts(None, None, false))
                    .await?
                    .escalate()
                    .await?;
                let _ = s.readable().await?;
                cdp.push(t0.elapsed().as_millis());
                s.close().await;
            }
            let med = |mut v: Vec<u128>| {
                v.sort();
                v[v.len() / 2]
            };
            println!("lite open:   {}ms (median of {iters})", med(lite));
            println!("cdp open:    {}ms", med(cdp));
        }
    }
    Ok(())
}

fn timeout_nav(t: u64) -> Option<u64> {
    Some(t + 5000)
}

fn repl(gopts: GlobalOpts) -> Result<()> {
    println!("kestrel REPL — the k API. .exit to leave, k.open(url) to begin.");
    let rt = rquickjs::Runtime::new()?;
    let ctx = rquickjs::Context::full(&rt)?;
    let mut sink = String::new();
    ctx.with(|ctx| kestrel_core::js::install_globals(&ctx, &mut sink, vec![], gopts))?;
    let mut buffer = String::new();
    let stdin = std::io::stdin();
    loop {
        let prompt = if buffer.is_empty() { "ksl> " } else { "...> " };
        use std::io::BufRead;
        print!("{prompt}");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == ".exit" || trimmed == ".quit" {
            break;
        }
        if buffer.is_empty()
            && (trimmed.starts_with("var ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("function "))
        {
            buffer.push_str(line.trim_end());
            buffer.push('\n');
            continue;
        }
        buffer.push_str(line.trim_end());
        let src = std::mem::take(&mut buffer);
        match ctx.with(|ctx| {
            let mut out = String::new();
            let ok = js::eval_line(&ctx, &src, &mut out)?;
            print!("{out}");
            Ok::<_, anyhow::Error>(ok)
        }) {
            Ok(_) => {}
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    Ok(())
}
