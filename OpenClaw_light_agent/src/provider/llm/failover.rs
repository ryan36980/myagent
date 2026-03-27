//! Failover LLM provider — tries multiple providers in order.
//!
//! On failover-eligible errors (rate limit, auth, timeout, server error),
//! automatically tries the next provider. Non-failover errors (context overflow,
//! format errors) are returned immediately.
//!
//! Includes per-provider cooldown with exponential backoff to avoid hammering
//! a failing provider.

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use tokio::time::Instant;
use tracing::{info, warn};

use super::LlmProvider;
use crate::channel::types::{ChatMessage, LlmResponse, StreamEvent, ToolDefinition};
use crate::error::{GatewayError, Result};

/// Cooldown state for a single provider.
struct ProviderCooldown {
    /// When cooldown expires (provider can be tried again).
    until: Instant,
    /// Consecutive error count (for exponential backoff).
    error_count: u8,
}

impl ProviderCooldown {
    fn new() -> Self {
        Self {
            until: Instant::now(),
            error_count: 0,
        }
    }

    /// Check if provider is in cooldown.
    fn is_cooling_down(&self) -> bool {
        Instant::now() < self.until
    }

    /// Record a failure and set cooldown with exponential backoff.
    /// Base: 60s, factor: 5x, cap: 3600s (1 hour).
    fn record_failure(&mut self) {
        self.error_count = self.error_count.saturating_add(1);
        let backoff_secs = match self.error_count {
            1 => 60,
            2 => 300,
            3 => 1500,
            _ => 3600,
        };
        self.until = Instant::now() + std::time::Duration::from_secs(backoff_secs);
    }

    /// Record a success — reset error count.
    fn record_success(&mut self) {
        self.error_count = 0;
    }
}

/// Check if an error is eligible for failover (transient / auth issues).
fn is_failover_eligible(e: &GatewayError) -> bool {
    let msg = e.to_string().to_lowercase();
    // Rate limit / quota
    msg.contains("429") || msg.contains("rate limit") || msg.contains("rate_limit") ||
    // Billing / payment
    msg.contains("402") || msg.contains("billing") ||
    // Auth errors (token expired, revoked, etc.)
    msg.contains("401") || msg.contains("403") ||
    // Timeout / connection
    msg.contains("timeout") || msg.contains("connection") || msg.contains("timed out") ||
    // Server errors
    msg.contains("502") || msg.contains("503") || msg.contains("500")
    // Do NOT failover: context overflow, abort, format errors
}

/// An LLM provider that tries multiple providers in order with failover.
pub struct FailoverLlmProvider {
    providers: Vec<Box<dyn LlmProvider>>,
    cooldowns: Mutex<Vec<ProviderCooldown>>,
}

impl FailoverLlmProvider {
    /// Create from a list of providers (first = primary, rest = fallbacks).
    pub fn new(providers: Vec<Box<dyn LlmProvider>>) -> Self {
        let n = providers.len();
        Self {
            providers,
            cooldowns: Mutex::new((0..n).map(|_| ProviderCooldown::new()).collect()),
        }
    }
}

#[async_trait]
impl LlmProvider for FailoverLlmProvider {
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let mut last_error = None;

        for (i, provider) in self.providers.iter().enumerate() {
            // Skip providers in cooldown
            {
                let cooldowns = self.cooldowns.lock().unwrap();
                if cooldowns[i].is_cooling_down() {
                    continue;
                }
            }

            match provider.chat(system, messages, tools).await {
                Ok(r) => {
                    {
                        let mut cooldowns = self.cooldowns.lock().unwrap();
                        cooldowns[i].record_success();
                    }
                    if i > 0 {
                        info!(provider_index = i, "failover: using fallback provider");
                    }
                    return Ok(r);
                }
                Err(e) if is_failover_eligible(&e) => {
                    warn!(
                        provider_index = i,
                        error = %e,
                        "failover: provider failed, trying next"
                    );
                    {
                        let mut cooldowns = self.cooldowns.lock().unwrap();
                        cooldowns[i].record_failure();
                    }
                    last_error = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            GatewayError::Config("all LLM providers are in cooldown".into())
        }))
    }

    async fn chat_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let mut last_error = None;

        for (i, provider) in self.providers.iter().enumerate() {
            {
                let cooldowns = self.cooldowns.lock().unwrap();
                if cooldowns[i].is_cooling_down() {
                    continue;
                }
            }

            match provider.chat_stream(system, messages, tools).await {
                Ok(stream) => {
                    {
                        let mut cooldowns = self.cooldowns.lock().unwrap();
                        cooldowns[i].record_success();
                    }
                    if i > 0 {
                        info!(provider_index = i, "failover: streaming from fallback provider");
                    }
                    return Ok(stream);
                }
                Err(e) if is_failover_eligible(&e) => {
                    warn!(
                        provider_index = i,
                        error = %e,
                        "failover: stream failed, trying next"
                    );
                    {
                        let mut cooldowns = self.cooldowns.lock().unwrap();
                        cooldowns[i].record_failure();
                    }
                    last_error = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            GatewayError::Config("all LLM providers are in cooldown".into())
        }))
    }
}
