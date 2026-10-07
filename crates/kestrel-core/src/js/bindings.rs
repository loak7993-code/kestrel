// kestrel :: js/bindings — the `k` API scripts program against.
//
// DESIGN NOTE: sessions live in a VM-thread-local map. `Session` is
// deliberately not Send/Sync (Browser owns a child process); QuickJS is
// single-threaded, so nothing here crosses threads. The clippy lints about
// non-Send Arcs below are expected and allowed at module level.
#![allow(clippy::arc_with_non_send_sync)]
//
// Every host call enters tokio and blocks the script: sequential automation
// reads exactly like it runs. `Promise.all` over host calls serialises (the
// VM is single-threaded) — that is a documented v1 property.
use crate::cdp::stealth::StealthOpts;
use crate::{OpenOpts, Session};
use rquickjs::prelude::*;
use rquickjs::{Ctx, Object, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Shared state the host bindings operate on.
pub struct HostState {
    pub rt: tokio::runtime::Handle,
    /// session id → live slot (taken while a call owns the session)
    pub sessions: Mutex<HashMap<String, Arc<Mutex<Option<Session>>>>>,
    pub opts: GlobalOpts,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalOpts {
    pub timeout_ms: Option<u64>,
    pub proxy: Option<String>,
    pub stealth: bool,
    pub profile: Option<String>,
    pub retries: Option<u32>,
}

impl GlobalOpts {
    fn open_opts(&self) -> OpenOpts {
        OpenOpts {
            engine: None,
            browser: None,
            timeout: Duration::from_millis(self.timeout_ms.unwrap_or(20000)),
            headers: vec![],
            user_agent: None,
            proxy: self.proxy.clone(),
            viewport: None,
            device: None,
            locale: None,
            timezone: None,
            block_urls: vec![],
            retries: self.retries,
            stealth: if self.stealth {
                Some(StealthOpts {
                    profile: self.profile.clone(),
                    ..Default::default()
                })
            } else {
                None
            },
            dialog_action: None,
        }
    }
}

/// The session map is only touched from the VM thread (QuickJS is
/// single-threaded); `Session` is deliberately not Send (Browser owns a child
/// process), so the compiler sees a non-Send Arc here by design.
#[allow(clippy::arc_with_non_send_sync)]
type Shared = Arc<HostState>;

/// Throw a JS Error carrying a host failure and hand back a dummy value.
fn throw_host<'js>(ctx: &Ctx<'js>, msg: String) -> Value<'js> {
    if let Ok(ex) = rquickjs::Exception::from_message(ctx.clone(), &msg) {
        let _ = ctx.throw(ex.into_value());
    }
    Value::new_undefined(ctx.clone())
}

/// Install every global on the context.
pub fn install<'js>(
    ctx: &Ctx<'js>,
    out: &mut String,
    args: Vec<String>,
    opts: GlobalOpts,
) -> rquickjs::Result<()> {
    #[allow(clippy::arc_with_non_send_sync)] // the VM is single-threaded by design
    let state: Shared = Arc::new(HostState {
        rt: tokio::runtime::Handle::current(),
        sessions: Mutex::new(HashMap::new()),
        opts,
    });

    let k = Object::new(ctx.clone())?;
    ctx.globals().set("k", k.clone())?;

    // ── logging / misc ──────────────────────────────────────────────────────
    {
        let o = Arc::new(Mutex::new(std::mem::take(out)));
        let o2 = o.clone();
        let ctx2 = ctx.clone();
        k.set(
            "log",
            Func::from(move |parts: rquickjs::function::Rest<Value<'js>>| {
                let strs: Vec<String> = parts
                    .0
                    .iter()
                    .map(|v| match v.as_string() {
                        Some(sv) => sv.to_string().unwrap_or_default(),
                        None => {
                            json_stringify(&ctx2, v).unwrap_or_else(|| String::from("undefined"))
                        }
                    })
                    .collect();
                o2.lock().unwrap().push_str(&strs.join(" "));
                o2.lock().unwrap().push('\n');
            }),
        )?;
        let o3 = o.clone();
        k.set(
            "flush",
            Func::from(move || -> String { std::mem::take(&mut *o3.lock().unwrap()) }),
        )?;
        k.set(
            "env",
            Func::from(move |name: String| std::env::var(name).ok()),
        )?;
        k.set(
            "exit",
            Func::from(move |code: i32| {
                std::process::exit(code);
                #[allow(unreachable_code)]
                ()
            }),
        )?;
    }

    // ── files ───────────────────────────────────────────────────────────────
    k.set(
        "save",
        Func::from(
            move |ctx: Ctx<'js>, path: String, data: Value<'js>| -> Value<'js> {
                let bytes = match to_bytes(&ctx, data) {
                    Ok(b) => b,
                    Err(e) => return throw_host(&ctx, e.to_string()),
                };
                if let Some(dir) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(&path, bytes) {
                    Ok(_) => Value::new_bool(ctx.clone(), true),
                    Err(e) => throw_host(&ctx, format!("save {path}: {e}")),
                }
            },
        ),
    )?;
    {
        k.set(
            "load",
            Func::from(move |ctx: Ctx<'js>, path: String| -> Value<'js> {
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        let arr = rquickjs::Array::new(ctx.clone()).unwrap();
                        for (i, b) in bytes.into_iter().enumerate() {
                            let _ = arr.set(i, b);
                        }
                        arr.into_value()
                    }
                    Err(e) => throw_host(&ctx, format!("load {path}: {e}")),
                }
            }),
        )?;
    }

    // ── http (no browser) ───────────────────────────────────────────────────
    {
        let st = state.clone();
        k.set(
            "fetch",
            Func::from(move |ctx: Ctx<'js>, url: String| -> Value<'js> {
                let st = st.clone();
                let out: anyhow::Result<serde_json::Value> = st.rt.clone().block_on(async move {
                    let opts = st.opts.open_opts();
                    let client = crate::http::build_client(&opts.lite_opts())?;
                    let res = crate::http::fetch(&client, &url, &opts.lite_opts()).await?;
                    anyhow::Ok(serde_json::json!({
                        "url": res.url, "status": res.status,
                        "headers": res.headers, "text": res.text(),
                    }))
                });
                finish(&ctx, out)
            }),
        )?;
    }

    // ── k.fetchAll(urls, opts) — parallel fetch + sequential processing ────
    // Returns [{ url, status, html, doc }]; `doc` is a parse handle (k.parse).
    // Network/render runs in parallel on the Rust side; the script iterates
    // the results on the VM thread (documented model).
    {
        // Send+Sync snapshot of the opts only — the session map (non-Sync by
        // design) must not cross into the pool's parallel futures
        let state = Arc::clone(&state);
        let snap: Arc<OpenOpts> = Arc::new(state.opts.open_opts());
        k.set(
            "fetchAll",
            Func::from(
                move |ctx: Ctx<'js>, urls: Value<'js>, opts: Opt<Value<'js>>| -> Value<'js> {
                    let state = Arc::clone(&state);
                    let Some(arr) = urls.as_array() else {
                        return throw_host(
                            &ctx,
                            "fetchAll: urls must be an array of strings".to_string(),
                        );
                    };
                    let url_list: Vec<String> = arr
                        .iter()
                        .filter_map(|v: rquickjs::Result<Value<'js>>| {
                            let v = match v {
                                Ok(v) => v,
                                Err(_) => return None,
                            };
                            v.as_string().and_then(|s| s.to_string().ok())
                        })
                        .collect();
                    let get_opt = |key: &str| -> Option<Value<'js>> {
                        opts.0
                            .as_ref()
                            .and_then(|o| o.as_object())
                            .and_then(|o| o.get(key).ok())
                    };
                    let concurrency = get_opt("concurrency")
                        .and_then(|v| v.as_number())
                        .map(|n| n as usize)
                        .unwrap_or(8)
                        .max(1);
                    let engine = get_opt("engine")
                        .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()));
                    let base_urls = url_list.clone();
                    let jv = state.rt.clone().block_on(fetch_all_pages(
                        snap.clone(),
                        url_list,
                        concurrency,
                        engine,
                    ));
                    match jv {
                        Ok(pages) => {
                            let arr = rquickjs::Array::new(ctx.clone());
                            let arr = match arr {
                                Ok(a) => a,
                                Err(e) => return throw_host(&ctx, e.to_string()),
                            };
                            for (i, page) in pages.into_iter().enumerate() {
                                let pobj = Object::new(ctx.clone());
                                let pobj = match pobj {
                                    Ok(o) => o,
                                    Err(e) => return throw_host(&ctx, e.to_string()),
                                };
                                if let Some(url) =
                                    page.get("url").and_then(serde_json::Value::as_str)
                                {
                                    let _ = pobj.set("url", url);
                                }
                                if let Some(status) =
                                    page.get("status").and_then(serde_json::Value::as_u64)
                                {
                                    let _ = pobj.set("status", status as u32);
                                }
                                if let Some(html) =
                                    page.get("html").and_then(serde_json::Value::as_str)
                                {
                                    let _ = pobj.set("html", html);
                                    if let Err(e) = install_doc(
                                        &ctx,
                                        &pobj,
                                        html,
                                        base_urls.get(i).map(String::as_str).unwrap_or(""),
                                    ) {
                                        return throw_host(&ctx, e.to_string());
                                    }
                                }
                                if let Some(err) =
                                    page.get("error").and_then(serde_json::Value::as_str)
                                {
                                    let _ = pobj.set("error", err);
                                }
                                let _ = arr.set(i, pobj.into_value());
                            }
                            arr.into_value()
                        }
                        Err(e) => throw_host(&ctx, e.to_string()),
                    }
                },
            ),
        )?;
    }

    // ── k.parse(html, base) — the DOM primitive over any HTML string ────────
    k.set(
        "parse",
        Func::from(
            move |ctx: Ctx<'js>, html: String, base: Opt<String>| -> Value<'js> {
                let base = base.0.unwrap_or_else(|| "http://localhost/".into());
                let obj = match Object::new(ctx.clone()) {
                    Ok(o) => o,
                    Err(e) => return throw_host(&ctx, e.to_string()),
                };
                match install_doc(&ctx, &obj, &html, &base) {
                    Ok(_) => obj.into_value(),
                    Err(e) => throw_host(&ctx, e.to_string()),
                }
            },
        ),
    )?;

    // ── open / detect / args ────────────────────────────────────────────────
    {
        let st = state.clone();
        k.set(
            "open",
            Func::from(
                move |ctx: Ctx<'js>, url: String, opts: Opt<Value<'js>>| -> Value<'js> {
                    let st = st.clone();
                    let jv = opts.0.as_ref().and_then(|v| json_of(&ctx, v));
                    let st_in = st.clone();
                    let out: anyhow::Result<String> = st.rt.block_on(async move {
                        let mut o = st_in.opts.open_opts();
                        if let Some(jv) = jv {
                            apply_open_opts(&mut o, &jv);
                        }
                        let session = Session::open(&url, o).await?;
                        let id = format!("s{}", st_in.sessions.lock().unwrap().len());
                        st_in
                            .sessions
                            .lock()
                            .unwrap()
                            .insert(id.clone(), Arc::new(Mutex::new(Some(session))));
                        Ok(id)
                    });
                    match out {
                        Ok(id) => {
                            let st2 = st.clone();
                            install_page(&ctx, st2, &id)
                                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
                        }
                        Err(e) => throw_host(&ctx, e.to_string()),
                    }
                },
            ),
        )?;
    }
    {
        k.set(
            "detect",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let rows: Vec<serde_json::Value> = crate::discover()
                    .into_iter()
                    .map(|b| serde_json::json!({ "name": b.name, "path": b.path }))
                    .collect();
                from_json(&ctx, serde_json::json!(rows))
                    .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
            }),
        )?;
    }
    {
        let mut map = HashMap::new();
        for a in &args {
            if let Some((k, v)) = a.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            } else {
                map.insert(a.to_string(), String::new());
            }
        }
        let args_obj = Object::new(ctx.clone())?;
        for (k, v) in map {
            args_obj.set(k, v)?;
        }
        k.set("args", args_obj)?;
    }

    Ok(())
}

/// Install the page facade for a live session id.
///
/// One macro generates every method with the same proven closure shape:
/// captures (st2, sid2) once and only reads them (per-call clones), so every
/// generated closure is `Fn` as rquickjs requires.
fn install_page<'js>(ctx: &Ctx<'js>, st: Shared, id: &str) -> rquickjs::Result<Value<'js>> {
    let obj = Object::new(ctx.clone())?;
    let sid = id.to_string();

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "title",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on((|st: Shared, sid: String| async move {
                    let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                    let v = session.title().await?;
                    put_session(&st, &sid, session);
                    Ok(serde_json::json!(v))
                })(st.clone(), sid.clone()));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "url",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let rt = st.rt.clone();
                let out = rt.block_on(async move {
                    let v = session_url(&st, &sid);
                    anyhow::Ok(serde_json::json!(v))
                });
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set("goto", Func::from(move |ctx: Ctx<'js>, url: String| -> Value<'js> {
            let st = st2.clone();
            let sid = sid2.clone();
            let out = st.rt.block_on((|st: Shared, sid: String, url: String| async move {
                let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                let s = session.escalate_lite().await?;
                match s {
                    Session::Cdp(c) => {
                        let nav = c
                            .page
                            .goto(url.as_str(), crate::cdp::page::GotoOpts {
                                wait_until: Some(crate::cdp::page::WaitUntil::Interactive),
                                timeout: Some(st.opts.timeout_ms.map(Duration::from_millis).unwrap_or(Duration::from_secs(45))),
                                referer: None,
                            })
                            .await?;
                        put_session(&st, &sid, Session::Cdp(c));
                        Ok(serde_json::json!({ "url": nav.url, "status": nav.status, "ms": nav.ms }))
                    }
                    other => {
                        put_session(&st, &sid, other);
                        anyhow::bail!("goto needs the browser engine")
                    }
                }
            })(st.clone(), sid.clone(), url));
            finish(&ctx, out)
        }))?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "html",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on((|st: Shared, sid: String| async move {
                    let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                    let v = session.html().await?;
                    put_session(&st, &sid, session);
                    Ok(serde_json::json!(v))
                })(st.clone(), sid.clone()));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "readable",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on((|st: Shared, sid: String| async move {
                    let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                    let v = session.readable().await?;
                    put_session(&st, &sid, session);
                    Ok(serde_json::json!(v))
                })(st.clone(), sid.clone()));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "clickAt",
            Func::from(move |ctx: Ctx<'js>, x: f64, y: f64| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st
                    .rt
                    .block_on((|st: Shared, sid: String, x: f64, y: f64| async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        let r = match s {
                            Session::Cdp(c) => {
                                c.page.human().click_at(x, y).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                serde_json::json!(true)
                            }
                            _ => anyhow::bail!("clickAt needs the browser engine"),
                        };
                        anyhow::Ok(r)
                    })(st.clone(), sid.clone(), x, y));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "text",
            Func::from(move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st
                    .rt
                    .block_on((|st: Shared, sid: String, sel: String| async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let v = session.text(&sel).await?;
                        put_session(&st, &sid, session);
                        Ok(serde_json::json!(v))
                    })(st.clone(), sid.clone(), sel));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set(
                "count",
                Func::from(move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        let n = match s {
                            Session::Cdp(ref c) => c.page.count(&sel).await?,
                            _ => {
                                put_session(&st, &sid, s);
                                anyhow::bail!("count needs the browser engine")
                            }
                        };
                        put_session(&st, &sid, s);
                        anyhow::Ok(serde_json::json!(n))
                    });
                    finish(&ctx, out)
                }),
            )?;
        }

        obj.set(
            "extract",
            Func::from(move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st
                    .rt
                    .block_on((|st: Shared, sid: String, sel: String| async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;

                        match s {
                            Session::Cdp(c) => {
                                let rows = c
                                    .page
                                    .extract(&sel, serde_json::json!({ "text": true }))
                                    .await?;
                                put_session(&st, &sid, Session::Cdp(c)); // the slot keeps a live session
                                Ok(serde_json::json!(rows))
                            }
                            other => {
                                put_session(&st, &sid, other);
                                anyhow::bail!("extract needs the browser engine")
                            }
                        }
                    })(st.clone(), sid.clone(), sel));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "eval",
            Func::from(move |ctx: Ctx<'js>, js: String| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st
                    .rt
                    .block_on((|st: Shared, sid: String, js: String| async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let (s, v) = session.eval(&js).await?;
                        put_session(&st, &sid, s);
                        Ok(v)
                    })(st.clone(), sid.clone(), js));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "wait",
            Func::from(
                move |ctx: Ctx<'js>, sel: String, to_ms: f64| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let out = st.rt.block_on(
                        (|st: Shared, sid: String, sel: String, to_ms: f64| async move {
                            let to = Duration::from_millis(to_ms as u64);
                            let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                            let (s, ok) = session.wait_for(&sel, to).await?;
                            put_session(&st, &sid, s);
                            Ok(serde_json::json!(ok))
                        })(st.clone(), sid.clone(), sel, to_ms),
                    );
                    finish(&ctx, out)
                },
            ),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "screenshot",
            Func::from(move |ctx: Ctx<'js>, o: Opt<Value<'js>>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on(
                    (|st: Shared, sid: String, o: Opt<Value<'js>>| async move {
                        let obj = o.0.as_ref().and_then(|v| v.as_object());
                        let full = obj
                            .and_then(|o| o.get::<_, bool>("full").ok())
                            .unwrap_or(false);
                        let sel: Option<String> = obj.and_then(|o| o.get::<_, String>("sel").ok());
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let (s, png) = match sel {
                            Some(sel) => session.element_shot(&sel).await?,
                            None => session.screenshot(full).await?,
                        };
                        put_session(&st, &sid, s);
                        Ok(serde_json::json!({ "bytes": png.len(), "png": base64_encode(&png) }))
                    })(st.clone(), sid.clone(), o),
                );
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "pdf",
            Func::from(move |ctx: Ctx<'js>, o: Opt<Value<'js>>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on(
                    (|st: Shared, sid: String, o: Opt<Value<'js>>| async move {
                        let obj = o.0.as_ref().and_then(|v| v.as_object());
                        let format = obj.and_then(|o| o.get::<_, String>("format").ok());
                        let landscape = obj
                            .and_then(|o| o.get::<_, bool>("landscape").ok())
                            .unwrap_or(false);
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let (s, pdf) = session.pdf(format.as_deref(), landscape).await?;
                        put_session(&st, &sid, s);
                        Ok(serde_json::json!({ "bytes": pdf.len(), "pdf": base64_encode(&pdf) }))
                    })(st.clone(), sid.clone(), o),
                );
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        obj.set(
            "cookies",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on((|st: Shared, sid: String| async move {
                    let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                    let v = session.cookies().await?;
                    put_session(&st, &sid, session);
                    Ok(serde_json::json!(v))
                })(st.clone(), sid.clone()));
                finish(&ctx, out)
            }),
        )?;
    }

    {
        let st2 = st.clone();
        let sid2 = sid.clone();
        // ── human input (seeded bezier moves, jitter typing, tremor holds) ─────
        {
            let h_st = st.clone();
            let h_sid = sid.clone();
            let human = Object::new(ctx.clone())?;

            macro_rules! hset {
                ($name:expr, $closure:expr) => {
                    human.set($name, Func::from($closure))?;
                };
            }

            // move(x, y)
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("move", move |ctx: Ctx<'js>, x: f64, y: f64| -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page.human().move_to(x, y).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            // click(sel)
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("click", move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page.human().click(&sel).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            // type(sel, text)
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("type", move |ctx: Ctx<'js>,
                                    sel: String,
                                    text: String|
                      -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page.human().type_text(&sel, &text).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            // scroll(by, read)
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("scroll", move |ctx: Ctx<'js>,
                                      by: i64,
                                      read: Opt<bool>|
                      -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page
                                    .human()
                                    .scroll_by(by, read.0.unwrap_or(false))
                                    .await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            // warmup()
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("warmup", move |ctx: Ctx<'js>| -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page.human().warmup(3, 1, 600).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            // hold(sel, ms)
            {
                let hs = h_st.clone();
                let hs2 = h_sid.clone();
                hset!("hold", move |ctx: Ctx<'js>,
                                    sel: String,
                                    ms: f64|
                      -> Value<'js> {
                    let st = hs.clone();
                    let sid = hs2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.escalate_lite().await?;
                        match s {
                            Session::Cdp(c) => {
                                c.page.human().hold(&sel, ms as u64, 8).await?;
                                put_session(&st, &sid, Session::Cdp(c));
                                anyhow::Ok(serde_json::json!(true))
                            }
                            _ => anyhow::bail!("human input needs the browser engine"),
                        }
                    });
                    finish(&ctx, out)
                });
            }
            obj.set("human", human.into_value())?;
        }

        // ── introspection: console log, captured requests, response bodies ─────
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set("console", Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
            let st = st2.clone();
            let sid = sid2.clone();
            let rt = st.rt.clone();
                let out = rt.block_on(async move {
                let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                let v = match &session {
                    Session::Cdp(c) => {
                        let rows = c.page.console().await;
                        serde_json::json!(rows.iter().map(|m| serde_json::json!({ "type": m.kind, "text": m.text })).collect::<Vec<_>>())
                    }
                    _ => serde_json::json!([]),
                };
                put_session(&st, &sid, session);
                anyhow::Ok(v)
            });
            finish(&ctx, out)
        }))?;
        }
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set(
                "requests",
                Func::from(move |ctx: Ctx<'js>, filter: Opt<String>| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let (s, rows) = session.netlog(filter.0.as_deref()).await?;
                        let v =
                            serde_json::json!(rows.iter().map(|r| serde_json::json!({
                    "id": r.id, "url": r.url, "method": r.method,
                    "status": r.status, "type": r.resource_type, "failed": r.failed,
                })).collect::<Vec<_>>());
                        put_session(&st, &sid, s);
                        anyhow::Ok(v)
                    });
                    finish(&ctx, out)
                }),
            )?;
        }
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set(
                "body",
                Func::from(move |ctx: Ctx<'js>, id: String| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let v = match &session {
                            Session::Cdp(c) => c.page.body(&id).await?,
                            _ => anyhow::bail!("body needs the browser engine"),
                        };
                        put_session(&st, &sid, session);
                        anyhow::Ok(serde_json::json!(v))
                    });
                    finish(&ctx, out)
                }),
            )?;
        }

        // ── goto with backoff retry — flaky proxies are the normal case ────────
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set("gotoWithRetry", Func::from(move |ctx: Ctx<'js>, url: String, retries: Opt<f64>| -> Value<'js> {
            let st = st2.clone();
            let sid = sid2.clone();
            let rt = st.rt.clone();
            let out: anyhow::Result<serde_json::Value> = rt.block_on(async move {
                let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                let s = session.escalate_lite().await?;
                match s {
                    Session::Cdp(c) => {
                        let nav = c.page.goto_with_retry(&url, retries.0.unwrap_or(3.0) as usize, Duration::from_millis(st.opts.timeout_ms.unwrap_or(45000))).await?;
                        put_session(&st, &sid, Session::Cdp(c));
                        anyhow::Ok(serde_json::json!({ "url": nav.url, "status": nav.status, "ms": nav.ms, "attempts": nav.attempts }))
                    }
                    _ => anyhow::bail!("gotoWithRetry needs the browser engine"),
                }
            });
            finish(&ctx, out)
        }))?;
        }

        // ── session save/load (cookies + localStorage, Playwright format) ─────
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set(
                "saveSession",
                Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let (s, state) = session.save_session().await?;
                        put_session(&st, &sid, s);
                        anyhow::Ok(state)
                    });
                    finish(&ctx, out)
                }),
            )?;
        }
        {
            let st2 = st.clone();
            let sid2 = sid.clone();
            obj.set(
                "loadSession",
                Func::from(move |ctx: Ctx<'js>, state: Value<'js>| -> Value<'js> {
                    let st = st2.clone();
                    let sid = sid2.clone();
                    let Some(jv) = json_of(&ctx, &state) else {
                        return throw_host(
                            &ctx,
                            "loadSession: state must be an object".to_string(),
                        );
                    };
                    let rt = st.rt.clone();
                    let out = rt.block_on(async move {
                        let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                        let s = session.load_session(jv).await?;
                        put_session(&st, &sid, s);
                        anyhow::Ok(serde_json::json!(true))
                    });
                    finish(&ctx, out)
                }),
            )?;
        }

        obj.set(
            "close",
            Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
                let st = st2.clone();
                let sid = sid2.clone();
                let out = st.rt.block_on((|st: Shared, sid: String| async move {
                    let session = take_session(&st, &sid).map_err(anyhow::Error::msg)?;
                    session.close().await;
                    st.sessions
                        .lock()
                        .unwrap()
                        .insert(sid.clone(), Arc::new(Mutex::new(None)));
                    Ok(serde_json::json!(null))
                })(st.clone(), sid.clone()));
                finish(&ctx, out)
            }),
        )?;
    }

    Ok(obj.into_value())
}

fn session_url(st: &Shared, sid: &str) -> serde_json::Value {
    match st.sessions.lock().unwrap().get(sid) {
        Some(slot) => match slot.lock().unwrap().as_ref() {
            Some(s) => serde_json::json!(s.url()),
            None => serde_json::json!(null),
        },
        None => serde_json::json!(null),
    }
}

fn take_session(state: &Shared, id: &str) -> Result<Session, String> {
    let map = state.sessions.lock().unwrap();
    let slot = map.get(id);
    match slot {
        Some(slot) => {
            let taken = slot.lock().unwrap().take();
            match taken {
                Some(s) => Ok(s),
                None => Err(format!("session {id} was closed (slot empty)")),
            }
        }
        None => Err(format!(
            "session {id} was closed (no slot; keys: {:?})",
            map.keys().collect::<Vec<_>>()
        )),
    }
}

fn put_session(state: &Shared, id: &str, s: Session) {
    let mut map = state.sessions.lock().unwrap();
    let slot = map
        .entry(id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(None)));
    *slot.lock().unwrap() = Some(s);
}

/// Convert an anyhow result into a JS value, throwing on error.
fn finish<'js>(ctx: &Ctx<'js>, out: anyhow::Result<serde_json::Value>) -> Value<'js> {
    match out {
        Ok(json) => from_json(ctx, json).unwrap_or_else(|_| Value::new_undefined(ctx.clone())),
        Err(e) => throw_host(ctx, e.to_string()),
    }
}

fn apply_open_opts(o: &mut OpenOpts, v: &serde_json::Value) {
    if let Some(e) = v.get("engine").and_then(serde_json::Value::as_str) {
        o.engine = Some(e.to_string());
    }
    if let Some(t) = v.get("timeout").and_then(serde_json::Value::as_u64) {
        o.timeout = Duration::from_millis(t);
    }
    if let Some(ua) = v.get("userAgent").and_then(serde_json::Value::as_str) {
        o.user_agent = Some(ua.to_string());
    }
    if let Some(p) = v.get("proxy").and_then(serde_json::Value::as_str) {
        o.proxy = Some(p.to_string());
    }
    if let Some(d) = v.get("device").and_then(serde_json::Value::as_str) {
        o.device = Some(d.to_string());
    }
    if let Some(l) = v.get("locale").and_then(serde_json::Value::as_str) {
        o.locale = Some(l.to_string());
    }
    if let Some(tz) = v.get("timezone").and_then(serde_json::Value::as_str) {
        o.timezone = Some(tz.to_string());
    }
    if let Some(arr) = v.get("block").and_then(serde_json::Value::as_array) {
        o.block_urls = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
    }
    if v.get("stealth")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        o.stealth = Some(StealthOpts {
            profile: v
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .map(String::from),
            ..Default::default()
        });
    }
    if let Some(vp) = v.get("viewport").and_then(serde_json::Value::as_str) {
        let mut it = vp.split('x');
        if let (Some(w), Some(h)) = (it.next(), it.next())
            && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
        {
            o.viewport = Some((w, h, 1.0));
        }
    }
}

/// JS value → serde_json via JSON.stringify (round-trips objects/arrays).
fn json_of<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> Option<serde_json::Value> {
    json_stringify(ctx, v).and_then(|s| serde_json::from_str(&s).ok())
}

fn json_stringify<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> Option<String> {
    let globals = ctx.globals();
    let json: Object = globals.get("JSON").ok()?;
    let stringify: rquickjs::Function = json.get("stringify").ok()?;
    stringify
        .call::<_, String>((v.clone(),))
        .ok()
        .or_else(|| Some("null".to_string()))
}

fn from_json<'js>(ctx: &Ctx<'js>, v: serde_json::Value) -> rquickjs::Result<Value<'js>> {
    match v {
        serde_json::Value::Null => Ok(Value::new_undefined(ctx.clone())),
        serde_json::Value::Bool(b) => Ok(Value::new_bool(ctx.clone(), b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::new_int(ctx.clone(), i as i32))
            } else {
                Ok(Value::new_float(ctx.clone(), n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::String(s) => {
            Ok(rquickjs::String::from_str(ctx.clone(), &s)?.into_value())
        }
        serde_json::Value::Array(items) => {
            let arr = rquickjs::Array::new(ctx.clone())?;
            for (i, item) in items.into_iter().enumerate() {
                arr.set(i, from_json(ctx, item)?)?;
            }
            Ok(arr.into_value())
        }
        serde_json::Value::Object(map) => {
            let obj = Object::new(ctx.clone())?;
            for (k, item) in map {
                obj.set(k, from_json(ctx, item)?)?;
            }
            Ok(obj.into_value())
        }
    }
}

fn to_bytes<'js>(ctx: &Ctx<'js>, v: Value<'js>) -> rquickjs::Result<Vec<u8>> {
    if let Some(s) = v.as_string() {
        return Ok(s.to_string()?.into_bytes());
    }
    if let Some(a) = v.as_array() {
        let len = a.len();
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let b: u32 = a.get(i)?;
            out.push(b as u8);
        }
        return Ok(out);
    }
    let s = json_stringify(ctx, &v).unwrap_or_default();
    Ok(s.into_bytes())
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Attach the lite-DOM surface to a JS object over an HTML string.
fn install_doc<'js>(
    _ctx: &Ctx<'js>,
    obj: &Object<'js>,
    html: &str,
    base: &str,
) -> rquickjs::Result<()> {
    let doc = Arc::new(crate::http::html::HtmlDoc::parse(html, base));

    // sync closures are fine here (no await needed)
    let d1 = doc.clone();
    obj.set(
        "text",
        Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
            let v = d1.readable();
            from_json(&ctx, serde_json::json!(v))
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        }),
    )?;
    let d2 = doc.clone();
    obj.set(
        "select",
        Func::from(move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
            let v = d2.text_of(&sel);
            from_json(&ctx, serde_json::json!(v))
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        }),
    )?;
    let d3 = doc.clone();
    obj.set(
        "attr",
        Func::from(
            move |ctx: Ctx<'js>, sel: String, name: String| -> Value<'js> {
                let v = d3.attr_of(&sel, &name);
                from_json(&ctx, serde_json::json!(v))
                    .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
            },
        ),
    )?;
    let d4 = doc.clone();
    obj.set(
        "links",
        Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
            let links = d4.links();
            from_json(
                &ctx,
                serde_json::json!(
                    links
                        .iter()
                        .map(|l| serde_json::json!({ "text": l.text, "href": l.href }))
                        .collect::<Vec<_>>()
                ),
            )
            .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        }),
    )?;
    let d5 = doc.clone();
    obj.set(
        "tables",
        Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
            from_json(&ctx, serde_json::json!(d5.tables()))
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        }),
    )?;
    let d6 = doc.clone();
    obj.set(
        "meta",
        Func::from(move |ctx: Ctx<'js>| -> Value<'js> {
            let m = d6.meta();
            let o = match Object::new(ctx.clone()) {
                Ok(o) => o,
                Err(e) => return throw_host(&ctx, e.to_string()),
            };
            for (k, v) in m {
                let _ = o.set(k, v);
            }
            o.into_value()
        }),
    )?;
    let d7 = doc.clone();
    obj.set(
        "extract",
        Func::from(move |ctx: Ctx<'js>, sel: String| -> Value<'js> {
            let rows: Vec<String> = Vec::new(); // placeholder replaced below
            let _ = rows;
            // text extraction over all matches: not in HtmlDoc yet — links-style map
            let texts = d7.extract_texts(&sel);
            from_json(&ctx, serde_json::json!(texts))
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        }),
    )?;
    Ok(())
}

/// Fetch many pages in parallel (pool for cdp, shared client for lite).
async fn fetch_all_pages(
    snap: Arc<OpenOpts>,
    url_list: Vec<String>,
    concurrency: usize,
    engine: Option<String>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    match engine.as_deref() {
        Some("cdp") => {
            let pool = crate::pool::Pool::start(crate::pool::PoolOpts {
                browsers: (concurrency / 4).max(1),
                max_concurrency: concurrency,
                launch: snap.launch_opts(),
                page: snap.page_opts(None),
                goto_timeout: snap.timeout,
            })
            .await?;
            let snap_for_map = snap.clone();
            let pages = pool
                .map(url_list.clone(), move |url, page| {
                    let snap_for_map = snap_for_map.clone();
                    async move {
                        let nav = page
                            .goto(
                                &url,
                                crate::cdp::page::GotoOpts {
                                    wait_until: Some(crate::cdp::page::WaitUntil::Interactive),
                                    timeout: Some(snap_for_map.timeout),
                                    referer: None,
                                },
                            )
                            .await?;
                        let html = page.content().await?;
                        anyhow::Ok(serde_json::json!({
                            "url": nav.url, "status": nav.status, "html": html,
                        }))
                    }
                })
                .await?;
            pool.close().await;
            Ok(pages)
        }
        _ => {
            // lite: true parallelism via a shared client
            let client = crate::http::build_client(&snap.lite_opts())?;
            let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
            let mut handles = vec![];
            for url in &url_list {
                let permit = sem.clone().acquire_owned().await.unwrap();
                let client = client.clone();
                let url = url.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = permit;
                    let res = crate::http::fetch(&client, &url, &crate::http::default_opts()).await;
                    match res {
                        Ok(r) => anyhow::Ok(serde_json::json!({
                            "url": r.url, "status": r.status, "html": r.text(),
                        })),
                        Err(e) => anyhow::Ok(serde_json::json!({
                            "url": url, "status": 0, "error": e.to_string(),
                        })),
                    }
                }));
            }
            let mut pages = vec![];
            for h in handles {
                if let Ok(r) = h.await
                    && let Ok(j) = r
                {
                    pages.push(j);
                }
            }
            Ok(pages)
        }
    }
}
