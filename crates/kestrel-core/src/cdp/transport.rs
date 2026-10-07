// kestrel :: cdp/transport — a CDP WebSocket connection with request/response
// correlation and an event bus. One instance per target (and one for the
// browser-level endpoint). Writer + reader run as separate tasks.
use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

enum ToWire {
    Msg(Message),
    Close,
}

pub struct CdpConn {
    tx: mpsc::Sender<ToWire>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>,
    events: broadcast::Sender<Value>, // full event objects: {method, params, sessionId?}
    next_id: AtomicU64,
    closed_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

impl CdpConn {
    pub async fn connect(url: &str) -> Result<Arc<CdpConn>> {
        let (ws, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| anyhow!("CDP websocket connect {url}: {e}"))?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel(4096);
        let (wire_tx, wire_rx) = mpsc::channel::<ToWire>(1024);
        let (closed_tx, closed_rx) = oneshot::channel::<()>();

        let (mut sink, mut stream) = ws.split();

        // writer task (owns the sink half)
        tokio::spawn(async move {
            let mut wire_rx = wire_rx;
            while let Some(item) = wire_rx.recv().await {
                match item {
                    ToWire::Msg(m) => {
                        if sink.send(m).await.is_err() {
                            break;
                        }
                    }
                    ToWire::Close => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        });

        // reader task
        let pending_reader = pending.clone();
        let events_for_reader = events_tx.clone();
        let wire_tx_reader = wire_tx.clone();
        tokio::spawn(async move {
            let mut closed = Some(closed_tx);
            while let Some(msg) = stream.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => break,
                };
                match msg {
                    Message::Text(txt) => {
                        let v: Value = match serde_json::from_slice(txt.as_bytes()) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if let Some(id) = v.get("id").and_then(Value::as_u64) {
                            let responder = pending_reader.lock().await.remove(&id);
                            if let Some(tx) = responder {
                                if let Some(err) = v.get("error") {
                                    let _ = tx.send(Err(anyhow!(
                                        "CDP {}: {}",
                                        err.get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("error"),
                                        err.get("data").and_then(Value::as_str).unwrap_or("")
                                    )));
                                } else {
                                    let _ =
                                        tx.send(Ok(v.get("result").cloned().unwrap_or(json!({}))));
                                }
                            }
                        } else if v.get("method").is_some() {
                            let _ = events_for_reader.send(v); // event
                        }
                    }
                    Message::Ping(data) => {
                        let _ = wire_tx_reader.send(ToWire::Msg(Message::Pong(data))).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            if let Some(tx) = closed.take() {
                let _ = tx.send(());
            }
            // fail every pending request
            let mut map = pending_reader.lock().await;
            for (_, tx) in map.drain() {
                let _ = tx.send(Err(anyhow!("connection closed")));
            }
        });

        Ok(Arc::new(CdpConn {
            tx: wire_tx,
            pending,
            events: events_tx,
            next_id: AtomicU64::new(1),
            closed_rx: Mutex::new(Some(closed_rx)),
        }))
    }

    /// Send a method, await its response (with timeout).
    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        self.send_timeout(method, params, Duration::from_secs(30))
            .await
    }

    pub async fn send_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "id": id, "method": method, "params": params });
        let text = serde_json::to_string(&msg)?;
        self.tx
            .send(ToWire::Msg(Message::Text(text.into())))
            .await
            .map_err(|_| anyhow!("connection closed (send {method})"))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(anyhow!("{method}: responder dropped")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!("{method}: timed out after {:?}", timeout))
            }
        }
    }

    /// Fire-and-forget (never blocks on a response, never waits on the socket).
    pub fn fire(&self, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "id": id, "method": method, "params": params });
        if let Ok(text) = serde_json::to_string(&msg) {
            let _ = self.tx.try_send(ToWire::Msg(Message::Text(text.into())));
        }
    }

    /// Subscribe to the event bus (receives full event objects).
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Wait for the next event with a given method name.
    pub async fn wait_event(
        &self,
        method: &str,
        mut predicate: impl FnMut(&Value) -> bool,
        timeout: Duration,
    ) -> Result<Value> {
        let mut rx = self.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!("timeout waiting for event {method}"));
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.get("method").and_then(Value::as_str) == Some(method) && predicate(&ev) {
                        return Ok(ev);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => {
                    return Err(anyhow!("timeout waiting for event {method}"));
                }
            }
        }
    }

    pub async fn close(&self) {
        let _ = self.tx.send(ToWire::Close).await;
        // give the socket a moment to drain
        if let Some(rx) = self.closed_rx.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_millis(500), rx).await;
        }
    }
}
