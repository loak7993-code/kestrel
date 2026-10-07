// kestrel :: human — human-like interaction. Behaviour layers (Cloudflare,
// Akamai, PerimeterX) score *how* you move and type at least as much as
// fingerprints do: perfectly straight mouse paths and metronomic typing are
// classic automation tells. Ported from src/human.js; all randomness is seeded
// so runs are reproducible.
use crate::cdp::page::Page;
use anyhow::{Result, anyhow};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// xorshift32 — the same generator shape as the JS engine, seeded for
/// reproducibility.
pub struct Rng(Mutex<u32>);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(Mutex::new((seed as u32) | 1))
    }
    pub fn next_f64(&self) -> f64 {
        let mut s = self.0.lock().unwrap();
        let mut x = *s;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *s = x;
        (x as f64) / (u32::MAX as f64)
    }
    pub fn range(&self, min: f64, max: f64) -> f64 {
        min + self.next_f64() * (max - min)
    }
}

/// Ease-in-out — humans accelerate then settle.
fn ease(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

pub struct Human {
    page: Arc<Page>,
    rand: Rng,
    speed: f64,  // >1 = faster/less patient
    jitter: f64, // 0 = deterministic, 1 = human
    pos: Mutex<Option<(f64, f64)>>,
    /// monotonically increasing hook counter (unused by CDP; kept for API parity)
    _gestures: AtomicU64,
}

impl Human {
    pub fn new(page: Arc<Page>) -> Self {
        Self::with_config(page, std::time::SystemTime::now(), 1.0, 1.0)
    }

    pub fn with_config(
        page: Arc<Page>,
        seed: std::time::SystemTime,
        speed: f64,
        jitter: f64,
    ) -> Self {
        let ms = seed
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1);
        Human {
            page,
            rand: Rng::new(ms),
            speed: if speed > 0.0 { speed } else { 1.0 },
            jitter: jitter.max(0.0),
            pos: Mutex::new(None),
            _gestures: AtomicU64::new(0),
        }
    }

    fn r(&self, min: f64, max: f64) -> f64 {
        self.rand.range(min, max)
    }
    fn j(&self, v: f64) -> f64 {
        v * self.jitter
    }
    async fn pause(&self, min: f64, max: f64) {
        tokio::time::sleep(Duration::from_millis(
            (self.r(min, max) / self.speed).round() as u64,
        ))
        .await;
    }

    /// Where the cursor is (seeded inside the viewport when unknown).
    pub async fn position(&self) -> Result<(f64, f64)> {
        if let Some(p) = *self.pos.lock().unwrap() {
            return Ok(p);
        }
        let vp = self
            .page
            .eval("[innerWidth, innerHeight]")
            .await
            .unwrap_or(json!([1280, 720]));
        let w = vp[0].as_f64().unwrap_or(1280.0);
        let h = vp[1].as_f64().unwrap_or(720.0);
        let p = (w * self.r(0.2, 0.8), h * self.r(0.2, 0.8));
        *self.pos.lock().unwrap() = Some(p);
        Ok(p)
    }

    /// Move along a curved path (two control points + micro jitter, optional
    /// overshoot then correction) — the way hand/eye aiming actually works.
    pub async fn move_to(&self, x: f64, y: f64) -> Result<()> {
        self.move_to_opts(x, y, 0.25).await
    }

    pub async fn move_to_opts(&self, x: f64, y: f64, overshoot: f64) -> Result<()> {
        let (fx, fy) = self.position().await?;
        let dist = ((x - fx).powi(2) + (y - fy).powi(2)).sqrt();
        let steps = ((dist / self.r(6.0, 14.0)).round() as i64).clamp(8, 60);
        // two control points → a non-linear, non-symmetric curve
        let c1 = (
            fx + (x - fx) * self.r(0.2, 0.45) + self.j(self.r(-90.0, 90.0)),
            fy + (y - fy) * self.r(0.15, 0.4) + self.j(self.r(-70.0, 70.0)),
        );
        let c2 = (
            fx + (x - fx) * self.r(0.55, 0.85) + self.j(self.r(-60.0, 60.0)),
            fy + (y - fy) * self.r(0.5, 0.85) + self.j(self.r(-50.0, 50.0)),
        );
        let (tx, ty) = if self.rand.next_f64() < overshoot {
            (x + self.j(self.r(-8.0, 8.0)), y + self.j(self.r(-8.0, 8.0)))
        } else {
            (x, y)
        };
        // anchor: the first event is where the cursor already is (no teleporting)
        self.dispatch_move(fx, fy).await;
        for i in 1..=steps {
            let t = ease(i as f64 / steps as f64);
            let u = 1.0 - t;
            let px =
                u * u * u * fx + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * tx;
            let py =
                u * u * u * fy + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * ty;
            self.dispatch_move(
                px + self.j(self.r(-0.8, 0.8)),
                py + self.j(self.r(-0.8, 0.8)),
            )
            .await;
            // velocity profile: slower at the ends, quicker mid-flight
            let speed_factor = 0.6 + (0.5 - i as f64 / steps as f64).abs();
            tokio::time::sleep(Duration::from_millis(
                ((self.r(6.0, 18.0) * speed_factor) / self.speed)
                    .round()
                    .max(2.0) as u64,
            ))
            .await;
        }
        self.dispatch_move(x, y).await;
        *self.pos.lock().unwrap() = Some((x, y));
        self._gestures.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn dispatch_move(&self, x: f64, y: f64) {
        self.page.conn.fire(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none" }),
        );
        tokio::time::sleep(Duration::from_millis(2)).await; // let the wire breathe
    }

    async fn dispatch_button(&self, kind: &str, x: f64, y: f64, button: &str, clicks: i32) {
        self.page.conn.fire(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": button, "clickCount": clicks }),
        );
    }

    /// Click a selector the human way: move, dwell, press, hold briefly, release.
    pub async fn click(&self, sel: &str) -> Result<()> {
        self.click_opts(sel, 140.0, 220.0).await
    }

    /// `hold_ms`/`dwell_ms` are the upper bounds of the press/settle pauses.
    pub async fn click_opts(&self, sel: &str, hold_ms: f64, dwell_ms: f64) -> Result<()> {
        self.click_opts2(sel, hold_ms * 0.32, hold_ms, dwell_ms * 0.27, dwell_ms)
            .await
    }

    pub async fn click_opts2(
        &self,
        sel: &str,
        hold_min: f64,
        hold_max: f64,
        dwell_min: f64,
        dwell_max: f64,
    ) -> Result<()> {
        self.page.ensure_engine().await;
        let p = self
            .page
            .eval(&format!("__ks.point({})", json_str(sel)))
            .await?;
        let x = p
            .get("x")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let y = p
            .get("y")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        if x == 0.0 && y == 0.0 {
            return Err(anyhow!("human.click: not clickable {sel}"));
        }
        self.move_to(x, y).await?;
        self.pause(dwell_min, dwell_max).await; // settle before pressing
        self.dispatch_button("mousePressed", x, y, "left", 1).await;
        self.pause(hold_min, hold_max).await; // humans hold the button
        self.dispatch_button("mouseReleased", x, y, "left", 1).await;
        Ok(())
    }

    /// Type with per-key cadence, word-boundary pauses and occasional corrections.
    pub async fn type_text(&self, sel: &str, text: &str) -> Result<()> {
        self.type_opts(sel, text, 0.02).await
    }

    pub async fn type_opts(&self, sel: &str, text: &str, mistakes: f64) -> Result<()> {
        self.page.ensure_engine().await;
        let _ = self
            .page
            .eval(&format!("__ks.focus({})", json_str(sel)))
            .await;
        for ch in text.chars() {
            // occasional typo then backspace, like a real person
            if self.rand.next_f64() < self.j(mistakes) && ch.is_ascii_alphabetic() {
                let wrong = char::from(b'a' + (self.rand.next_f64() * 26.0) as u8);
                self.send_char(wrong).await;
                self.pause(40.0, 160.0).await;
                self.press_key("Backspace").await;
                self.pause(30.0, 120.0).await;
            }
            let is_space = ch == ' ';
            match ch {
                ' ' => self.press_key("Space").await,
                '\n' => self.press_key("Enter").await,
                c => self.send_char(c).await,
            }
            if is_space {
                self.pause(60.0, 220.0).await; // between words
            } else {
                self.pause(28.0, 145.0).await; // between keys
            }
            if self.rand.next_f64() < 0.03 * self.jitter {
                self.pause(250.0, 900.0).await; // hesitation
            }
        }
        Ok(())
    }

    async fn send_char(&self, ch: char) {
        let text = ch.to_string();
        self.page
            .conn
            .fire("Input.insertText", json!({ "text": text }));
        tokio::time::sleep(Duration::from_millis(4)).await;
    }

    async fn press_key(&self, key: &str) {
        self.page.conn.fire(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyDown", "key": key,
                "code": key_code(key), "windowsVirtualKeyCode": vk_code(key),
            }),
        );
        tokio::time::sleep(Duration::from_millis(12)).await;
        self.page.conn.fire(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp", "key": key,
                "code": key_code(key), "windowsVirtualKeyCode": vk_code(key),
            }),
        );
        tokio::time::sleep(Duration::from_millis(8)).await;
    }

    /// Scroll with wheel momentum and optional reading pauses.
    pub async fn scroll_by(&self, by: i64, read: bool) -> Result<()> {
        let dir = if by < 0 { -1.0 } else { 1.0 };
        let mut left = by.abs() as f64;
        while left > 0.0 {
            let chunk = left.min(self.r(120.0, 380.0));
            let ticks = self.r(4.0, 9.0).round() as i64;
            for i in 0..ticks {
                let decay = 1.0 - i as f64 / ticks as f64;
                self.page.conn.fire(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": 0, "y": 0, "deltaX": 0, "deltaY": dir * ((chunk / ticks as f64) * decay).round() }),
                );
                tokio::time::sleep(Duration::from_millis(
                    (self.r(8.0, 26.0) / self.speed).round() as u64,
                ))
                .await;
            }
            left -= chunk;
            if read {
                self.pause(180.0, 800.0).await; // "reading"
            }
        }
        Ok(())
    }

    /// Idle like a person: small drift movements, no clicks.
    pub async fn idle(&self, ms: u64) -> Result<()> {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        let (fx, fy) = self.position().await?;
        while tokio::time::Instant::now() < until {
            let x = (fx + self.j(self.r(-40.0, 40.0))).max(5.0);
            let y = (fy + self.j(self.r(-30.0, 30.0))).max(5.0);
            self.dispatch_move(x, y).await;
            *self.pos.lock().unwrap() = Some((x, y));
            tokio::time::sleep(Duration::from_millis(
                (self.r(220.0, 900.0) / self.speed).round() as u64,
            ))
            .await;
        }
        Ok(())
    }

    /// Make a fresh session look alive: a few natural cursor moves, an optional
    /// scroll, then an idle dwell.
    pub async fn warmup(&self, mouse: usize, scroll: usize, dwell_ms: u64) -> Result<()> {
        let (w, h) = self.viewport().await?;
        for _ in 0..mouse {
            self.move_to(w * self.r(0.15, 0.85), h * self.r(0.15, 0.8))
                .await?;
            self.pause(120.0, 480.0).await;
        }
        for _ in 0..scroll {
            self.scroll_by(self.r(120.0, 400.0).round() as i64, false)
                .await?;
            self.pause(200.0, 700.0).await;
        }
        if dwell_ms > 0 {
            self.idle(dwell_ms).await?;
        }
        Ok(())
    }

    /// Press and hold with tremor — for canvas/hold challenges (PerimeterX style).
    /// A perfectly regular hold fails behavioural validation, so duration, tremor
    /// and micro-movements are all randomised.
    pub async fn hold(&self, sel: &str, ms: u64, micro_moves: usize) -> Result<()> {
        self.page.ensure_engine().await;
        let p = self
            .page
            .eval(&format!("__ks.point({})", json_str(sel)))
            .await?;
        let x = p
            .get("x")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let y = p
            .get("y")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        if x == 0.0 && y == 0.0 {
            return Err(anyhow!("human.hold: target not found {sel}"));
        }
        self.move_to(x, y).await?;
        self.dispatch_button("mousePressed", x, y, "left", 1).await;
        let jitter_ms = (ms as f64 * self.r(-0.12, 0.12)) as i64;
        let end = tokio::time::Instant::now()
            + Duration::from_millis((ms as i64 + jitter_ms).max(0) as u64);
        let mut i = 0usize;
        while tokio::time::Instant::now() < end {
            i += 1;
            let jx = x + (self.rand.next_f64() - 0.5) * 2.2;
            let jy = y + (self.rand.next_f64() - 0.5) * 2.2;
            self.page.conn.fire(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": jx, "y": jy, "button": "left" }),
            );
            tokio::time::sleep(Duration::from_millis(
                (90.0 + self.rand.next_f64() * 120.0) as u64,
            ))
            .await;
            if i >= micro_moves.max(1) * 12 {
                break; // bound the loop the way the JS helper does via microMoves
            }
        }
        self.dispatch_button("mouseReleased", x, y, "left", 1).await;
        Ok(())
    }

    async fn viewport(&self) -> Result<(f64, f64)> {
        let vp = self
            .page
            .eval("[innerWidth, innerHeight]")
            .await
            .unwrap_or(json!([1280, 720]));
        Ok((
            vp[0].as_f64().unwrap_or(1280.0),
            vp[1].as_f64().unwrap_or(720.0),
        ))
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

fn key_code(key: &str) -> &'static str {
    match key {
        "Backspace" => "Backspace",
        "Enter" => "Enter",
        "Space" => "Space",
        "Tab" => "Tab",
        "Escape" => "Escape",
        _ => "",
    }
}

fn vk_code(key: &str) -> i32 {
    match key {
        "Backspace" => 8,
        "Enter" => 13,
        "Space" => 32,
        "Tab" => 9,
        "Escape" => 27,
        _ => 0,
    }
}
