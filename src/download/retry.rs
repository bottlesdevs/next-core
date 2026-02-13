//! Retry logic with exponential backoff
//!
//! This module provides retry strategies for failed downloads, implementing
//! exponential backoff to avoid overwhelming servers while maximizing
//! the chance of successful recovery.

use crate::Error;
use std::time::Duration;
use tokio::time::sleep;

/// Retry policy configuration
///
/// Defines the retry behavior including maximum attempts and backoff timing.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts
    pub max_retries: u32,
    /// Initial delay before first retry (in milliseconds)
    pub initial_delay_ms: u64,
    /// Maximum delay between retries (in milliseconds)
    pub max_delay_ms: u64,
    /// Multiplier for exponential backoff
    pub backoff_multiplier: f64,
    /// Whether to add jitter to retry delays
    pub use_jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            backoff_multiplier: 2.0,
            use_jitter: true,
        }
    }
}

impl RetryPolicy {
    /// Create a new retry policy with the specified max retries
    pub fn with_max_retries(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Default::default()
        }
    }

    /// Create a policy with no retries
    pub fn no_retries() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Calculate the delay for a specific retry attempt
    ///
    /// Uses exponential backoff: delay = initial * (multiplier ^ attempt)
    /// Capped at max_delay_ms and optionally includes jitter.
    ///
    /// # Arguments
    ///
    /// * `attempt` - The retry attempt number (0-indexed)
    ///
    /// # Returns
    ///
    /// The delay duration for this retry attempt
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::from_millis(self.initial_delay_ms);
        }

        // Calculate exponential backoff
        let exponential_delay = self.initial_delay_ms as f64
            * self.backoff_multiplier.powi(attempt as i32);
        
        // Cap at maximum delay
        let delay_ms = (exponential_delay as u64).min(self.max_delay_ms);

        // Add jitter (±25%) to avoid thundering herd
        let final_delay_ms = if self.use_jitter && delay_ms > 0 {
            let jitter_range = delay_ms / 4;
            if jitter_range > 0 {
                let jitter = fastrand::u64(0..jitter_range * 2);
                delay_ms.saturating_add(jitter).saturating_sub(jitter_range)
            } else {
                delay_ms
            }
        } else {
            delay_ms
        };

        Duration::from_millis(final_delay_ms)
    }

    /// Check if a retry should be attempted
    ///
    /// # Arguments
    ///
    /// * `current_attempt` - The number of retry attempts already made (0 = first retry)
    ///
    /// # Returns
    ///
    /// true if another retry should be attempted, false otherwise
    pub fn should_retry(&self, current_attempt: u32) -> bool {
        current_attempt <= self.max_retries
    }
}

/// Retry context for tracking retry state
#[derive(Debug, Clone)]
pub struct RetryContext {
    /// The retry policy being used
    pub policy: RetryPolicy,
    /// Current attempt number (0 = first attempt, not a retry)
    pub attempt: u32,
    /// History of errors from previous attempts
    pub errors: Vec<String>,
}

impl RetryContext {
    /// Create a new retry context with the given policy
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            attempt: 0,
            errors: Vec::new(),
        }
    }

    /// Record a failure and determine if we should retry
    ///
    /// # Arguments
    ///
    /// * `error` - The error that occurred
    ///
    /// # Returns
    ///
    /// Some(Duration) if we should retry after the delay, None if max retries exceeded
    pub fn record_failure(&mut self, error: &Error) -> Option<Duration> {
        self.errors.push(error.to_string());
        self.attempt += 1;

        if self.policy.should_retry(self.attempt) {
            Some(self.policy.calculate_delay(self.attempt))
        } else {
            None
        }
    }

    /// Get the total number of attempts made (including the initial attempt)
    pub fn total_attempts(&self) -> u32 {
        self.attempt + 1
    }

    /// Check if we've exhausted all retries
    pub fn is_exhausted(&self) -> bool {
        self.attempt > self.policy.max_retries
    }

    /// Get a summary of all errors encountered
    pub fn error_summary(&self) -> String {
        self.errors.join("; ")
    }
}

/// Execute an async operation with retry logic
///
/// This is a helper function that wraps an async operation with automatic
/// retry logic using exponential backoff.
///
/// # Type Parameters
///
/// * `T` - The return type of the operation
/// * `F` - The operation function type
/// * `Fut` - The future type returned by the operation
///
/// # Arguments
///
/// * `operation` - The async operation to execute
/// * `policy` - The retry policy to use
///
/// # Returns
///
/// The result of the operation if it eventually succeeds, or the last error
///
/// # Example
///
/// ```rust,ignore
/// use bottles_core::download::retry::{execute_with_retry, RetryPolicy};
///
/// async fn fetch_data() -> Result<String, Error> {
///     // Might fail
///     Ok("data".to_string())
/// }
///
/// let result = execute_with_retry(
///     || fetch_data(),
///     RetryPolicy::default()
/// ).await;
/// ```
pub async fn execute_with_retry<T, F, Fut>(
    mut operation: F,
    policy: RetryPolicy,
) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Error>>,
{
    let mut context = RetryContext::new(policy);

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if let Some(delay) = context.record_failure(&error) {
                    tracing::warn!(
                        "Operation failed (attempt {}/{}), retrying after {:?}: {}",
                        context.attempt,
                        policy.max_retries,
                        delay,
                        error
                    );
                    sleep(delay).await;
                } else {
                    tracing::error!(
                        "Operation failed after {} attempts: {}",
                        context.total_attempts(),
                        context.error_summary()
                    );
                    return Err(Error::RetryLimitExceeded(context.error_summary()));
                }
            }
        }
    }
}

/// Errors that should not trigger a retry
///
/// Some errors are permanent and retrying won't help.
pub fn is_permanent_error(error: &Error) -> bool {
    match error {
        // Don't retry on 404 Not Found
        Error::Http(e) => {
            if let Some(status) = e.status() {
                matches!(status.as_u16(), 404 | 410 | 403)
            } else {
                false
            }
        }
        // Don't retry invalid URL errors
        Error::UrlParse(_) => true,
        // Don't retry configuration errors
        Error::InvalidConfig(_) => true,
        // Retry everything else
        _ => false,
    }
}

/// Execute with retry, but only for transient errors
///
/// Same as execute_with_retry, but won't retry permanent errors.
pub async fn execute_with_smart_retry<T, F, Fut>(
    mut operation: F,
    policy: RetryPolicy,
) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Error>>,
{
    let mut context = RetryContext::new(policy);

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(error) => {
                // Don't retry permanent errors
                if is_permanent_error(&error) {
                    tracing::warn!("Permanent error, not retrying: {}", error);
                    return Err(error);
                }

                if let Some(delay) = context.record_failure(&error) {
                    tracing::warn!(
                        "Transient error (attempt {}/{}), retrying after {:?}: {}",
                        context.attempt,
                        policy.max_retries,
                        delay,
                        error
                    );
                    sleep(delay).await;
                } else {
                    tracing::error!(
                        "Operation failed after {} attempts: {}",
                        context.total_attempts(),
                        context.error_summary()
                    );
                    return Err(Error::RetryLimitExceeded(context.error_summary()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_policy_calculate_delay() {
        let policy = RetryPolicy::default();

        // First retry - base is 2000ms, with ±25% jitter
        let delay1 = policy.calculate_delay(1);
        assert!(delay1 <= Duration::from_millis(2500)); // max: 2000 + 500

        // Second retry - base is 4000ms, with ±25% jitter  
        let delay2 = policy.calculate_delay(2);
        assert!(delay2 <= Duration::from_millis(5000)); // max: 4000 + 1000
    }

    #[test]
    fn test_retry_policy_max_delay() {
        let policy = RetryPolicy {
            max_delay_ms: 5000,
            backoff_multiplier: 10.0,
            use_jitter: false, // Disable jitter for this test
            ..Default::default()
        };

        let delay = policy.calculate_delay(10);
        assert!(delay <= Duration::from_millis(5000));
    }

    #[test]
    fn test_retry_policy_should_retry() {
        let policy = RetryPolicy::with_max_retries(3);

        // can_retry(retry_attempts_made)
        assert!(policy.should_retry(0)); // 0 retries made, can do 1st retry
        assert!(policy.should_retry(1)); // 1 retry made, can do 2nd retry  
        assert!(policy.should_retry(2)); // 2 retries made, can do 3rd retry
        assert!(policy.should_retry(3)); // 3 retries made, can do 4th retry
        assert!(!policy.should_retry(4)); // 4 retries made, max reached
    }

    #[test]
    fn test_retry_context() {
        let policy = RetryPolicy::with_max_retries(2);
        let mut context = RetryContext::new(policy);

        assert_eq!(context.attempt, 0);
        assert!(!context.is_exhausted());

        let error = Error::Download("test error".to_string());
        
        // First failure
        let delay1 = context.record_failure(&error);
        assert!(delay1.is_some());
        assert_eq!(context.attempt, 1);

        // Second failure
        let delay2 = context.record_failure(&error);
        assert!(delay2.is_some());
        assert_eq!(context.attempt, 2);

        // Third failure - exhausted
        let delay3 = context.record_failure(&error);
        assert!(delay3.is_none());
        assert!(context.is_exhausted());
    }

    #[tokio::test]
    async fn test_execute_with_retry_success() {
        let policy = RetryPolicy::with_max_retries(3);
        
        let result = execute_with_retry(
            || async { Ok::<_, Error>(42) },
            policy,
        ).await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_execute_with_retry_eventual_success() {
        let policy = RetryPolicy::with_max_retries(3);
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_clone = attempts.clone();

        let result = execute_with_retry(
            move || {
                let attempts = attempts_clone.clone();
                async move {
                    let count = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if count < 2 {
                        Err(Error::Download("temporary failure".to_string()))
                    } else {
                        Ok(42)
                    }
                }
            },
            policy,
        ).await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn test_is_permanent_error() {
        // Test with mock HTTP error - can't easily create reqwest::Error in tests
        // Just test the Error variants we can create
        assert!(is_permanent_error(&Error::UrlParse(url::ParseError::EmptyHost)));
        assert!(is_permanent_error(&Error::InvalidConfig("test".to_string())));
        assert!(!is_permanent_error(&Error::Download("test".to_string())));
    }
}
