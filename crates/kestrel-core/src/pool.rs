// kestrel :: pool — parallel scraping across pre-warmed pages. N browsers,
// bounded concurrency, acquire/release or `map(items, fn)`.
use crate::cdp::browser::{Browser, LaunchOpts};
use crate::cdp::page::{GotoOpts, Nav, Page, PageOpts, WaitUntil};
use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub struct Pool {
    browsers: Vec<Arc<AsyncMutex<Browser>>>,
    next_browser: AtomicUsize,
    pages: AsyncMutex<Vec<Arc<Page>>>,
    page_opts: PageOpts,
    goto_timeout: Duration,
    semaphore: Arc<Semaphore>,
    pub metrics: Mutex<PoolMetrics>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PoolMetrics {
    pub created: usize,
    pub acquired: usize,
    pub released: usize,
}

#[derive(Debug, Clone)]
pub struct PoolOpts {
    pub browsers: usize,
    pub max_concurrency: usize,
    pub launch: LaunchOpts,
    pub page: PageOpts,
    pub goto_timeout: Duration,
}

impl Default for PoolOpts {
    fn default() -> Self {
        PoolOpts {
            browsers: 2,
            max_concurrency: 8,
            launch: LaunchOpts::default(),
            page: PageOpts::default(),
            goto_timeout: Duration::from_secs(45),
        }
    }
}

impl Pool {
    /// Launch the browsers up-front so `map()` never pays launch cost mid-run.
    pub async fn start(opts: PoolOpts) -> Result<Arc<Pool>> {
        let n = opts.browsers.max(1);
        let mut browser_list = Vec::with_capacity(n);
        for _ in 0..n {
            let mut b = Browser::launch(opts.launch.clone()).await?;
            let _ = b.new_page(opts.page.clone()).await?; // warm one
            browser_list.push(Arc::new(AsyncMutex::new(b)));
        }
        Ok(Arc::new(Pool {
            browsers: browser_list,
            next_browser: AtomicUsize::new(0),
            pages: AsyncMutex::new(vec![]),
            page_opts: opts.page.clone(),
            goto_timeout: opts.goto_timeout,
            semaphore: Arc::new(Semaphore::new(opts.max_concurrency.max(1))),
            metrics: Mutex::new(PoolMetrics {
                created: n,
                ..Default::default()
            }),
        }))
    }

    async fn acquire(&self) -> Result<Arc<Page>> {
        if let Some(p) = self.pages.lock().await.pop() {
            self.bump(|m| m.acquired += 1);
            return Ok(p);
        }
        // round-robin over browsers; Browser::new_page takes &mut so lock it
        let idx = self.next_browser.fetch_add(1, Ordering::Relaxed) % self.browsers.len();
        let mut b = self.browsers[idx].lock().await;
        let page = b.new_page(self.page_opts.clone()).await?;
        self.bump(|m| {
            m.created += 1;
            m.acquired += 1;
        });
        Ok(page)
    }

    fn release(&self, page: Arc<Page>) {
        self.bump(|m| m.released += 1);
        let pages = &self.pages;
        pages.try_lock().map(|mut p| p.push(page)).ok();
    }

    fn bump(&self, f: impl FnOnce(&mut PoolMetrics)) {
        f(&mut self.metrics.lock().unwrap());
    }

    /// Navigate a pooled page (interactive wait, the pool's timeout).
    pub async fn goto(&self, page: &Arc<Page>, url: &str) -> Result<Nav> {
        page.goto(
            url,
            GotoOpts {
                wait_until: Some(WaitUntil::Interactive),
                timeout: Some(self.goto_timeout),
                referer: None,
            },
        )
        .await
    }

    /// Run `f(url, page)` for every item with bounded concurrency. Pages are
    /// released back to the pool after each item, so later items stay fast.
    pub async fn map<T, F, Fut>(self: &Arc<Self>, urls: Vec<String>, f: F) -> Result<Vec<T>>
    where
        F: Fn(String, Arc<Page>) -> Fut + Send + Sync + 'static + Clone,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let results: Arc<AsyncMutex<Vec<T>>> =
            Arc::new(AsyncMutex::new(Vec::with_capacity(urls.len())));
        let mut handles = vec![];
        for url in urls {
            let permit = self.semaphore.clone().acquire_owned().await.unwrap();
            let page = self.acquire().await?;
            let f = f.clone();
            handles.push(tokio::spawn(async move {
                let out = f(url, page.clone()).await;
                drop(permit);
                (out, page)
            }));
        }
        for h in handles {
            if let Ok((out, page)) = h.await {
                if let Ok(v) = out {
                    results.lock().await.push(v);
                }
                self.release(page);
            }
        }
        Ok(results.lock().await.drain(..).collect())
    }

    pub async fn close(&self) {
        for b in &self.browsers {
            b.lock().await.close().await;
        }
        self.pages.lock().await.clear();
    }
}
