//! Shared exponential-backoff-with-full-jitter retry policy for **idempotent**
//! remote operations (Cloudflare D1, object storage, git-over-HTTPS discovery).
//!
//! This exists to align with Lore's `SlowDown` handling: when a cloud backend
//! answers `429 Too Many Requests` or `503 Service Unavailable` (optionally with
//! a `Retry-After` header), the client should back off rather than hammer the
//! endpoint. All retries are bounded three ways — a maximum retry count, a
//! per-sleep `max_delay` cap (which also clamps a hostile `Retry-After`), and a
//! `total_deadline` wall-clock budget — so tail latency can never grow
//! unbounded.
//!
//! Only operations that are safe to repeat may use this: reads, existence
//! probes, content-addressed PUTs, and requests the server provably did not act
//! on (HTTP 429/503, or a connection that never completed). Non-idempotent
//! writes must instead rely on natural idempotency (SQL `UPSERT` / content hash)
//! or an explicit idempotency key — see `docs/development/gap/lore.md` §0.2.

use std::{future::Future, time::Duration};

use crate::utils::error::emit_warning;

/// Exponential backoff with full jitter and hard caps.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retries after the first attempt
    /// (total attempts = `max_retries + 1`).
    pub max_retries: u32,
    /// Base delay seeding the exponential window for the first retry.
    pub base_delay: Duration,
    /// Upper bound on any single sleep. A server `Retry-After` larger than this
    /// is clamped (with a warning) so a hostile/misconfigured server cannot pin
    /// the client for an unbounded time.
    pub max_delay: Duration,
    /// Total wall-clock budget across all sleeps. Once the next sleep would
    /// exceed it, retrying stops and the last error is returned.
    pub total_deadline: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(10),
            total_deadline: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// Full-jitter backoff for a 0-based retry attempt: a uniform random delay
    /// in `[0, min(max_delay, base_delay * 2^attempt)]`. Full jitter (rather
    /// than equal jitter) minimises thundering-herd retry collisions.
    pub fn jittered_backoff(&self, attempt: u32) -> Duration {
        let base_ms = self.base_delay.as_millis().min(u128::from(u64::MAX)) as u64;
        let cap_ms = self.max_delay.as_millis().min(u128::from(u64::MAX)) as u64;
        // Cap the shift so `1 << shift` never overflows; by attempt 63 the
        // window has long since saturated `cap_ms` anyway.
        let shift = attempt.min(63);
        let window = base_ms.saturating_mul(1u64 << shift).min(cap_ms).max(1);
        Duration::from_millis(fastrand::u64(0..=window))
    }

    /// Clamp a server-provided `Retry-After` to `max_delay`, warning when the
    /// server asked to wait longer than we are willing to.
    pub fn clamp_retry_after(&self, retry_after: Duration) -> Duration {
        if retry_after > self.max_delay {
            emit_warning(format!(
                "server requested Retry-After of {}s; clamping to {}s",
                retry_after.as_secs(),
                self.max_delay.as_secs()
            ));
            self.max_delay
        } else {
            retry_after
        }
    }
}

/// Outcome of a single attempt inside [`retry_idempotent`].
pub enum RetryOutcome<T, E> {
    /// Terminal — return this result as-is (success or a non-retryable error).
    Done(Result<T, E>),
    /// Retryable failure. `retry_after` is an optional server hint (e.g. from a
    /// `Retry-After` header); `last_err` is returned if retries are exhausted.
    Retry {
        retry_after: Option<Duration>,
        last_err: E,
    },
}

/// Run an idempotent async operation with exponential backoff + full jitter.
///
/// `op` receives the 0-based attempt index and returns a [`RetryOutcome`]. The
/// loop stops (returning the last error) once `max_retries` is reached or the
/// next sleep would exceed `total_deadline`.
///
/// # Arguments
/// * `policy` - the bounded backoff policy.
/// * `op` - builds and runs one attempt; must be safe to invoke repeatedly.
pub async fn retry_idempotent<T, E, F, Fut>(policy: &RetryPolicy, op: F) -> Result<T, E>
where
    F: Fn(u32) -> Fut,
    Fut: Future<Output = RetryOutcome<T, E>>,
{
    let start = std::time::Instant::now();
    let mut attempt: u32 = 0;
    loop {
        match op(attempt).await {
            RetryOutcome::Done(result) => return result,
            RetryOutcome::Retry {
                retry_after,
                last_err,
            } => {
                if attempt >= policy.max_retries {
                    return Err(last_err);
                }
                let delay = match retry_after {
                    Some(after) => policy.clamp_retry_after(after),
                    None => policy.jittered_backoff(attempt),
                };
                if start.elapsed() + delay > policy.total_deadline {
                    return Err(last_err);
                }
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Parse an HTTP `Retry-After` header value into a delay.
///
/// Supports the delta-seconds form (e.g. `120`). The HTTP-date form is not
/// parsed — it returns `None`, so callers fall back to computed backoff rather
/// than trusting an unbounded wait.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use super::*;

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 4,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(4),
            total_deadline: Duration::from_secs(5),
        }
    }

    #[test]
    fn jittered_backoff_never_exceeds_cap() {
        let policy = fast_policy();
        for attempt in 0..40 {
            let delay = policy.jittered_backoff(attempt);
            assert!(
                delay <= policy.max_delay,
                "attempt {attempt}: {delay:?} exceeded cap {:?}",
                policy.max_delay
            );
        }
    }

    #[test]
    fn clamp_retry_after_caps_large_values() {
        let policy = fast_policy();
        assert_eq!(
            policy.clamp_retry_after(Duration::from_secs(3600)),
            policy.max_delay
        );
        // Under-cap values are passed through unchanged.
        assert_eq!(
            policy.clamp_retry_after(Duration::from_millis(2)),
            Duration::from_millis(2)
        );
    }

    #[test]
    fn parse_retry_after_handles_seconds_and_rejects_dates() {
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("  5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_ref = calls.clone();
        let policy = fast_policy();
        let result: Result<u32, &str> = retry_idempotent(&policy, |_attempt| {
            let calls = calls_ref.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    RetryOutcome::Retry {
                        retry_after: None,
                        last_err: "transient",
                    }
                } else {
                    RetryOutcome::Done(Ok(n))
                }
            }
        })
        .await;
        assert_eq!(result, Ok(2));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stops_after_max_retries() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_ref = calls.clone();
        let policy = fast_policy();
        let result: Result<u32, &str> = retry_idempotent(&policy, |_attempt| {
            let calls = calls_ref.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                RetryOutcome::Retry {
                    retry_after: None,
                    last_err: "always fails",
                }
            }
        })
        .await;
        assert_eq!(result, Err("always fails"));
        // 1 initial attempt + max_retries retries.
        assert_eq!(calls.load(Ordering::SeqCst), policy.max_retries + 1);
    }

    #[tokio::test]
    async fn success_on_first_attempt_does_not_sleep() {
        let policy = fast_policy();
        let result: Result<&str, &str> =
            retry_idempotent(&policy, |_attempt| async { RetryOutcome::Done(Ok("ok")) }).await;
        assert_eq!(result, Ok("ok"));
    }
}
