use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Async token-bucket rate limiter enforcing per-minute request and token limits.
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
        let (max_rpm, max_tpm) = {
            let inner = self.inner.lock().await;
            (inner.max_rpm, inner.max_tpm)
        };
        if max_rpm == 0 || max_tpm == 0 {
            return Err(anyhow::anyhow!("rate limits must be greater than zero"));
        }

        loop {
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
            let rpm_wait = if inner.request_times.len() >= inner.max_rpm {
                inner.request_times.front().and_then(|&oldest| {
                    let elapsed = now.duration_since(oldest);
                    if elapsed < window {
                        Some(window - elapsed)
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            // Check TPM
            let current_tokens: usize = inner.token_entries.iter().map(|&(_, t)| t).sum();
            let tpm_wait = if current_tokens + token_count > inner.max_tpm {
                inner.token_entries.front().and_then(|&(oldest, _)| {
                    let elapsed = now.duration_since(oldest);
                    if elapsed < window {
                        Some(window - elapsed)
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            // Determine the longest wait time
            let wait = match (rpm_wait, tpm_wait) {
                (Some(r), Some(t)) => Some(r.max(t)),
                (Some(r), None) => Some(r),
                (None, Some(t)) => Some(t),
                (None, None) => None,
            };

            if let Some(dur) = wait {
                // Release the lock before sleeping, then retry
                drop(inner);
                tokio::time::sleep(dur).await;
                continue;
            }

            // Both limits pass: record the request under the same lock
            inner.request_times.push_back(now);
            inner.token_entries.push_back((now, token_count));
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn acquire_passes_when_under_limits() {
        let limiter = RateLimiter::new(10, 1000, 60);
        limiter.acquire(10).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_waits_when_over_rpm() {
        let limiter = RateLimiter::new(1, 1000, 60);
        limiter.acquire(1).await.unwrap();

        let start = Instant::now();
        limiter.acquire(1).await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_waits_when_over_tpm() {
        let limiter = RateLimiter::new(10, 10, 60);
        limiter.acquire(5).await.unwrap();

        let start = Instant::now();
        limiter.acquire(6).await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_rejects_zero_rpm_limit() {
        let limiter = RateLimiter::new(0, 1000, 60);
        assert!(limiter.acquire(1).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_rejects_zero_tpm_limit() {
        let limiter = RateLimiter::new(10, 0, 60);
        assert!(limiter.acquire(1).await.is_err());
    }
}
