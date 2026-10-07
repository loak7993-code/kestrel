// kestrel :: js — the scripting layer. User scripts run in an embedded
// QuickJS VM against the `k` API; host calls enter tokio and block the script
// (sequential automation reads naturally, and `Promise.all` of host calls
// serialises — documented).
pub mod bindings;

pub use bindings::{GlobalOpts, HostState};
use rquickjs::{Context, Runtime};

/// Run a script file to completion. `args` become `k.args` (k=v strings).
pub fn run_script(
    path: &str,
    args: Vec<String>,
    opts: bindings::GlobalOpts,
) -> Result<i32, anyhow::Error> {
    let src = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
    let (code, output) = run_source(&src, args, opts, Some(path.to_string()))?;
    print!("{output}");
    if code != 0 {
        std::process::exit(code);
    }
    Ok(code)
}

/// Evaluate a source string. Returns (exit code, collected log output).
pub fn run_source(
    src: &str,
    args: Vec<String>,
    opts: bindings::GlobalOpts,
    path: Option<String>,
) -> Result<(i32, String), anyhow::Error> {
    let rt = Runtime::new()?;
    let ctx = Context::full(&rt)?;
    let mut out = String::new();

    let result: Result<i32, String> = ctx.with(|ctx| {
        if let Err(e) = bindings::install(&ctx, &mut out, args, opts) {
            return Err(format!("init: {e}"));
        }
        let full = match &path {
            // the script sees its own path for relative file ops
            Some(p) => format!(
                "globalThis.k = globalThis.k || {{}}; globalThis.k.__file = {}; \n{}",
                serde_json::to_string(p).unwrap(),
                src
            ),
            None => src.to_string(),
        };
        let result = match ctx.eval::<rquickjs::Value, _>(full.as_str()) {
            Ok(_) => Ok(0),
            Err(err) => Err(render_error(&ctx, err)),
        };
        // flush buffered k.log() output into the caller's string — AFTER the
        // script, on both paths (the pre-script flush above was the bug: it
        // drained the buffer before the script ever wrote to it)
        if let Ok(logs) = ctx.eval::<String, _>("k.flush()") {
            out.push_str(&logs);
        }
        result
    });
    // teardown policy: leak the VM on the way out — ALWAYS, success or error.
    // The CLI exits immediately after, and freeing QuickJS with live script
    // objects trips a libc assertion (measured). Nothing outlives the process.
    std::mem::forget(ctx);
    std::mem::forget(rt);
    let code = result.map_err(anyhow::Error::msg)?;
    Ok((code, out))
}

/// Install the k API on a bare context (used by the REPL).
pub fn install_globals(
    ctx: &rquickjs::Ctx<'_>,
    out: &mut String,
    args: Vec<String>,
    opts: bindings::GlobalOpts,
) -> Result<(), anyhow::Error> {
    bindings::install(ctx, out, args, opts).map_err(|e| anyhow::anyhow!("{e}"))
}

/// REPL line evaluation (each line shares one runtime via the caller's context).
pub fn eval_line(
    ctx: &rquickjs::Ctx<'_>,
    line: &str,
    out: &mut String,
) -> Result<bool, anyhow::Error> {
    // `var`/`const` re-declarations must survive across lines: wrap non-var input
    let result: Result<rquickjs::Value<'_>, _> = if line.trim_start().starts_with("var ")
        || line.trim_start().starts_with("const ")
        || line.trim_start().starts_with("let ")
        || line.trim_start().starts_with("function ")
    {
        ctx.eval(line.to_string())
    } else {
        ctx.eval(format!("globalThis.__ks_line = (function(){{ try {{ return ({line}) }} catch (e) {{ return {{ __stmt: 1 }} }} }})(); (function(){{ var __v = globalThis.__ks_line; if (typeof __v === 'object' && __v && __v.__stmt) {{ return eval({}); }} return __v; }})()",
            serde_json::to_string(line).unwrap()))
    };
    match result {
        Ok(v) => {
            let s = match v.as_string() {
                Some(sv) => sv.to_string().unwrap_or_default(),
                None => {
                    use rquickjs::CatchResultExt;
                    let g = ctx.globals();
                    let json: rquickjs::Object =
                        g.get("JSON").map_err(|e| anyhow::anyhow!("{e}"))?;
                    let stringify: rquickjs::Function =
                        json.get("stringify").map_err(|e| anyhow::anyhow!("{e}"))?;
                    stringify
                        .call::<_, String>((v.clone(),))
                        .catch(ctx)
                        .unwrap_or_else(|_| "undefined".to_string())
                }
            };
            if !s.is_empty() {
                out.push_str(&s);
                out.push('\n');
            }
            Ok(true)
        }
        Err(err) => {
            out.push_str(&render_error(ctx, err));
            out.push('\n');
            Ok(false)
        }
    }
}

pub fn render_error(ctx: &rquickjs::Ctx<'_>, err: rquickjs::Error) -> String {
    match err {
        rquickjs::Error::Exception => {
            // the pending exception is retrievable via ctx.catch()
            let v: rquickjs::Value = ctx.catch();
            match rquickjs::Exception::from_value(v) {
                Ok(ex) => {
                    let msg = ex.message().unwrap_or_else(|| "error".to_string());
                    match ex.stack() {
                        Some(s) => format!("error: {msg}\n{s}"),
                        None => format!("error: {msg}"),
                    }
                }
                Err(_) => "error: exception".to_string(),
            }
        }
        other => format!("error: {other}"),
    }
}
