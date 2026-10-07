# kestrel

Browser automation at native speed. A Rust framework with the browser engine
(CDP), a pure-HTTP engine, coherent stealth profiles, human-like input,
challenge/captcha detection — and an **embedded QuickJS runtime**, so scripts
program against a `k` API with no Node, no Python, no external runtime.

```bash
cargo install --path .          # or grab a release binary
ksl run scrape.js               # scripts
ksl open https://example.com    # sugar commands, same output as the JS tools
```

## Why

Three lessons from running velox (JS) and velox-rs (Rust port) against real
targets, baked in from line one:

1. **Startup dominates.** A native binary starts in ~2 ms where a Node CLI
   pays ~100 ms before its first CDP message. Every defect that ever made the
   JS tool slow (spawn-heavy discovery, race timers holding the process open,
   double engine injection) is structurally absent here.
2. **The in-page logic is JavaScript regardless.** Selectors, waits, stealth
   patches execute inside Chrome as JS. kestrel ships its own compact in-page
   engine (`__ks`) and its own coherent stealth patch set — authored here, not
   extracted from anywhere.
3. **Extensibility needs closures, and closures need a scripting VM.** A pure
   Rust framework can't take user callbacks. QuickJS is embedded instead:
   scripts get a real language; the host stays native; nothing else to install.

## The `k` API

```js
// scrape.js — host calls BLOCK the script: sequential automation reads
// exactly like it runs. (Promise.all over host calls serialises — the VM
// is single-threaded; that is deliberate.)
const page = k.open(k.args.site, { engine: "cdp" });   // auto|lite|cdp
k.log("title:", page.title());
const rows = page.extract(".story", { attrs: ["href", "data-id"] });
const shot = page.screenshot({ full: true });
k.save("page.png", shot);

const nav = page.goto(page.url() + "/page2");          // { url, status, ms }
const retry = page.gotoWithRetry(page.url() + "/flaky", 3);  // backoff retry; { attempts: [...] }
const tok = page.waitForCaptchaToken(null, 60000);     // diagnoses on timeout
const challenge = page.detectChallenge({ wait: true });

page.close();
```

```js
// hybrid: pure HTTP first, browser only when JS is needed
const res = k.fetch(url);            // keep-alive, gzip/brotli, cookie jar
if (k.needsJS(res)) {
  const page = k.open(url);          // escalates, cookies carry over
}
```

Full surface: `k.open/fetch/save/load/log/env/args/detect/exit`,
page `title/url/goto/html/readable/text/extract/count/eval/wait/screenshot/
pdf/cookies/clickAt/close`, human input — `page.human.move/click/type/scroll/
warmup/hold` (seeded, reproducible) — and the challenge loop:

```js
const res = page.detectChallenge();          // { type, sitekey, visible, … }
const engaged = page.engageChallenge(30000); // click the checkbox / hold /
                                             // wait for clearance + token
if (engaged.cleared) page.goto(page.url());  // reload → the real page
```

Cookie carry-over is real: a lite `k.fetch` sees `set-cookie` headers, and the
escalated browser session starts with that jar applied before its first
navigation.

**Parallel fetching** — the network runs in parallel (pool for cdp, shared
keep-alive client for lite), the script processes results sequentially:

```js
const pages = k.fetchAll(urls, { concurrency: 8, engine: "lite" });
for (const p of pages) {
  k.log(p.status, p.url, "| h1:", p.select("h1"), "| links:", p.links().length);
}

// any HTML string, parsed locally — no browser, no round-trip
const doc = k.parse(html, base);
doc.select("h1"); doc.links(); doc.tables(); doc.extract("li");
```

Page introspection is scriptable too: `page.console()`, `page.requests(/filter/)`,
`page.body(requestId)`, `page.saveSession()` / `page.loadSession(state)`
(cookies + localStorage, Playwright format — the CLI `save-session`/
`load-session` commands ride the same path).

The CLI's `--engine auto|lite|cdp` flag is live on every open-based command
(it was silently ignored in the first cut — caught by profiling `open --engine
cdp` at 21 ms, which is a lite-engine time, not a browser one). `--retries N`
retries transient failures (timeouts, resets, 5xx) with linear backoff at the
session level — the same semantics as `gotoWithRetry` but around the whole
open.

## CLI

```bash
ksl run script.js -- k=v …    # scripts (embedded QuickJS)
ksl repl                      # interactive, same k API
ksl open URL [--json|--html|--sel] [-o FILE] [--stealth] [--engine auto|lite|cdp] [--retries N]
ksl shot URL [--full|--sel]   |  ksl pdf URL [--format A4] [--landscape]
ksl links/scrape/eval/cookies/challenge/net/har URL …
ksl bench URL                 # lite vs browser timings, in-process
ksl detect                    # browsers found on this machine
```

`--profile` on `open`/`shot` prints per-phase timings.

## Architecture

```
crates/kestrel-core/
  cdp/transport.rs    one WebSocket per target; writer+reader tasks; id-
                      correlated requests; broadcast event bus
  cdp/browser.rs      launch (stderr ws-url capture) / connect; /json/new?url=
                      fast path (chrome starts loading during boot)
  cdp/page.rs         goto with lifecycle waits (late-attach safe), bare-call
                      eval with self-heal, extraction via the in-page engine,
                      screenshots (clip), PDFs, cookies (dot-domain fix),
                      network capture + HAR
  cdp/stealth.rs      3 coherent profiles + 15 geo presets + UA-CH alignment
                      with the real binary; deterministic seeded noise
  cdp/challenge.rs    turnstile/recaptcha/hcaptcha/arkose/awswaf/px detection,
                      window-object authoritative signals, {wait} mode
  human.rs            seeded bezier mouse, jitter typing, warmup, press-hold
  http/               reqwest engine + scraper DOM + readability
  needs_js.rs         escalation heuristics
  pool.rs             N browsers × bounded concurrency
  js/                 QuickJS runtime, k-API bindings, REPL support
crates/kestrel-cli/   ksl binary
tests/                integration tests against the built-in Rust test site
```

## Test

```bash
cargo test --release                        # unit + integration (no browser)
VELOX_BROWSER=/path/to/chrome cargo test    # + CDP tests
```

## Status (honest gaps)

Shipped: dual engine (auto/lite/cdp + escalation), navigation with lifecycle
waits and the boot-load fast path, in-page engine (`css/id=/tag=/text=`,
shadow-`wait`, `waitExpr`), extraction, eval, screenshots (viewport/full/
element), PDFs, cookies, human input, stealth profiles, challenge detection +
token waiting, network capture + HAR, pool, CLI + scripts + REPL.

Not in v1: drag&drop/file upload/workers/screencast video/virtual clock,
proxy auth forwarder, accounts/identity tooling, plugin loader for scripts
(host closures are Rust-side for now). Challenge engage covers the
behavioural layer (checkbox/press-hold/verify-button + clearance wait); IP
reputation and image-grid puzzles are out of scope in every framework. The
framework is standalone — it shares no code with velox; where the two
overlap, the semantics were written fresh (stealth profiles, in-page engine)
or ported with the same lessons applied (transport, discovery).

MIT.
