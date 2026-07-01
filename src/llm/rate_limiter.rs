use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct RateLimiter {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Request timestamps for RPM tracking
    request_times: VecDeque<Instant>,
    /// Token usage entries for TPM tracking
    token_entries: VecDeque<(Instant, usize)>,
    max_rpm: usize,
    max_tpm: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_rpm: usize, max_tpm: usize, window_seconds: u64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                request_times: VecDeque::new(),
                token_entries: VecDeque::new(),
                max_rpm,
                max_tpm,
                window: Duration::from_secs(window_seconds),
            }),
        }
    }

    /// Wait until both RPM and TPM limits allow a new request with `token_count` tokens.
    pub async fn acquire(&self, token_count: usize) -> anyhow::Result<()> {
        loop {
            let wait = {
                let mut inner = self.inner.lock().await;
                let now = Instant::now();
                let window = inner.window;

                // Prune expired entries
                while let Some(&t) = inner.request_times.front() {
                    if now.duration_since(t) > window {
                        inner.request_times.pop_front();
                    } else {
                        break;
                    }
                }
                while let Some(&(t, _)) = inner.token_entries.front() {
                    if now.duration_since(t) > window {
                        inner.token_entries.pop_front();
                    } else {
                        break;
                    }
                }

                // Check RPM
                if inner.request_times.len() >= inner.max_rpm {
                    if let Some(&oldest) = inner.request_times.front() {
                        let elapsed = now.duration_since(oldest);
                        if elapsed < window {
                            Some(window - elapsed)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(dur) = wait {
                tokio::time::sleep(dur).await;
                continue;
            }

            // Check TPM
            let wait = {
                let inner = self.inner.lock().await;
                let now = Instant::now();
                let window = inner.window;
                let current_tokens: usize = inner.token_entries.iter().map(|&(_, t)| t).sum();

                if current_tokens + token_count > inner.max_tpm {
                    if let Some(&(oldest, _)) = inner.token_entries.front() {
                        let elapsed = now.duration_since(oldest);
                        if elapsed < window {
                            Some(window - elapsed)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(dur) = wait {
                tokio::time::sleep(dur).await;
                continue;
            }

            // Record the request
            {
                let mut inner = self.inner.lock().await;
                inner.request_times.push_back(Instant::now());
                inner.token_entries.push_back((Instant::now(), token_count));
            }

            return Ok(());
        }
    }
}
