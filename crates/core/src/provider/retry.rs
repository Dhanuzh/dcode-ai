//! Shared transient-error retry for provider requests.
//!
//! Retryable: [`ProviderError::RateLimited`] (429) and
//! [`ProviderError::Transient`] (connection reset / timeout / stale keep-alive
//! that failed before a response). Auth, config, model-not-found, and generic
//! request failures are surfaced immediately (retrying a 400/401 just wastes
//! time and tokens).

use std::future::Future;
use std::time::Duration;

use super::ProviderError;

/// Default attempt budget for a transient provider request (1 try + 4 retries).
/// Only rate-limit (429) errors retry — non-retryable errors still fail on the
/// first attempt — so a higher budget just gives free-tier rolling limits more
/// chances to clear without affecting auth/model errors.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Exponential backoff floor for the Nth attempt: 250ms, 500ms, 1s, … capped 8s.
fn base_backoff_ms(attempt: u32) -> u64 {
    250u64.saturating_mul(1u64 << attempt.min(5)).min(8_000)
}

/// How long to wait before retrying `err` on `attempt` (1-based), or `None` if
/// the error is not retryable. Honors the server's `retry_after_ms` hint but
/// never waits less than the exponential-backoff floor.
pub fn retry_delay(err: &ProviderError, attempt: u32) -> Option<Duration> {
    match err {
        ProviderError::RateLimited { retry_after_ms } => Some(Duration::from_millis(
            (*retry_after_ms).max(base_backoff_ms(attempt)),
        )),
        // Transient transport failures (connection reset, timeout, stale
        // keep-alive) happen before any response — retrying almost always
        // succeeds. Use plain exponential backoff (no server hint available).
        ProviderError::Transient(_) => Some(Duration::from_millis(base_backoff_ms(attempt))),
        _ => None,
    }
}

/// Run `op` up to `max_attempts` times, retrying retryable errors with backoff.
/// `op` is re-invoked from scratch each attempt (it must rebuild its request).
pub async fn with_retry<T, F, Fut>(max_attempts: u32, mut op: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                match retry_delay(&err, attempt) {
                    Some(delay) if attempt < max_attempts => {
                        tokio::time::sleep(delay).await;
                    }
                    _ => return Err(err),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn only_rate_limited_is_retryable() {
        assert!(retry_delay(&ProviderError::RateLimited { retry_after_ms: 10 }, 1).is_some());
        assert!(retry_delay(&ProviderError::AuthError("x".into()), 1).is_none());
        assert!(retry_delay(&ProviderError::RequestFailed("x".into()), 1).is_none());
    }

    #[test]
    fn server_hint_overrides_when_larger() {
        // retry_after 5s beats the ~500ms floor at attempt 1.
        let d = retry_delay(
            &ProviderError::RateLimited {
                retry_after_ms: 5_000,
            },
            1,
        )
        .unwrap();
        assert_eq!(d, Duration::from_millis(5_000));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_until_success_then_stops() {
        let calls = Cell::new(0);
        let result: Result<u32, ProviderError> = with_retry(5, || {
            let n = calls.get() + 1;
            calls.set(n);
            async move {
                if n < 3 {
                    Err(ProviderError::RateLimited { retry_after_ms: 1 })
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 3);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_attempts() {
        let calls = Cell::new(0);
        let result: Result<u32, ProviderError> = with_retry(3, || {
            calls.set(calls.get() + 1);
            async { Err(ProviderError::RateLimited { retry_after_ms: 1 }) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 3); // exactly max_attempts
    }

    #[tokio::test(start_paused = true)]
    async fn non_retryable_fails_immediately() {
        let calls = Cell::new(0);
        let result: Result<u32, ProviderError> = with_retry(5, || {
            calls.set(calls.get() + 1);
            async { Err(ProviderError::AuthError("nope".into())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 1); // no retry
    }
}
