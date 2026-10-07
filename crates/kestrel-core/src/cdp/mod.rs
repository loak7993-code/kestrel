// kestrel :: cdp — Chrome DevTools Protocol driver.
//
// Transport: one WebSocket per target (Chrome serves `devtools/page/<id>`
// endpoints), a writer task + a reader task per connection, request/response
// correlated by id, events fanned out on a broadcast channel. No flat-protocol
// session multiplexing needed — the HTTP /json endpoints do target lifecycle.
pub mod browser;
pub mod challenge;
pub mod page;
pub mod stealth;
pub mod transport;

pub use browser::{Browser, ConnectOpts, LaunchOpts};
pub use page::{Cookie, GotoOpts, Nav, Page, PageOpts, RequestEntry, WaitUntil};

use anyhow::{Result, anyhow};

/// Base64 helpers for binary CDP payloads (screenshots, PDFs).
pub(crate) fn b64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| anyhow!("base64 decode: {e}"))
}
