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
