#[cfg(test)]
mod sampling_tests;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::Arc;

use super::provider::{CompletionParams, CompletionResult, Message, ProviderRegistry};
use super::sampling::{truncate_error, LlmCallSample, LlmCallSink};
use crate::models::*;

/// Client for LLM completion requests with multi-provider support.
#[derive(Clone)]
pub struct LLMClient {
    inner: reqwest::Client,
    provider_registry: Option<Arc<ProviderRegistry>>,
    /// RENG-57: where every attempt's latency is recorded, when the caller
    /// has a place to put it. `None` (the default) records nothing — the CLI
    /// and unit tests keep working unchanged, and only the review path (which
    /// owns a store) attaches one.
    sink: Option<Arc<dyn LlmCallSink>>,
}

/// What the retry loop should do with a failed completion (RENG-35).
///
/// The provider layer is `anyhow`-based, so there is no typed status to match
/// on — threading a new error type through `LLMProvider` (and every provider
/// implementation) would be a far larger change than the behaviour it buys.
/// The status therefore gets parsed out of the message the provider client
/// writes, by rule, instead of being inferred from arbitrary substrings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryVerdict {
    /// A status that can plausibly answer differently on a later attempt:
    /// 408 (request timeout) and 429 (rate limit) of the 4xx class, plus every
    /// 5xx.
    Retryable { status: u16 },
    /// A 4xx outside that set. The provider rejected this request itself —
    /// bad or revoked credentials (401/403), unknown model (404), malformed
    /// body (400) — and re-sending the identical request cannot change that.
    Permanent { status: u16 },
    /// No HTTP status anywhere in the error: the request never produced a
    /// response (transport/DNS/TLS failure, timeout) or the response could not
    /// be read. Retried — the class the pre-RENG-35 substring list was reaching
    /// for with `"timeout"` / `"connection"`.
    Unknown,
}

impl LLMClient {
    /// Create a new `LLMClient` with a default reqwest HTTP client (120s timeout).
    ///
    /// The provider registry is initially `None`; call [`with_registry`](Self::with_registry)
    /// to enable provider-based routing.
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        Self {
            inner: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
            provider_registry: None,
            sink: None,
        }
    }

    /// Set a provider registry for provider-based routing.
    pub fn with_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.provider_registry = Some(registry);
        self
    }

    /// Attach the sink every attempt's latency is recorded to (RENG-57).
    ///
    /// A review owns one sink bound to its id
    /// ([`crate::store::llm_samples::StoreLlmCallSink`]); the client only
    /// reports what it did. Without a sink no sample is written anywhere —
    /// which is the CLI's case, where no store exists to write to.
    pub fn with_sink(mut self, sink: Option<Arc<dyn LlmCallSink>>) -> Self {
        self.sink = sink;
        self
    }

    fn build_messages(system_prompt: &str, user_prompt: &str) -> Vec<Message> {
        vec![
            Message {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user_prompt.to_string(),
            },
        ]
    }

    /// Build the OpenAI-compatible chat request body.
    ///
    /// Injects `"thinking": {"type": "disabled"}` only when the config opts in
    /// via `disable_thinking`, so providers that do not recognise the field
    /// never receive it. Reasoning models otherwise spend the whole
    /// `max_tokens` budget on `reasoning_tokens` and return an empty content.
    fn build_chat_request_body(config: &LLMConfig, system_prompt: &str, user_prompt: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": config.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "max_tokens": config.max_tokens,
            "temperature": config.temperature,
        });
        if config.disable_thinking == Some(true) {
            body["thinking"] = serde_json::json!({"type": "disabled"});
        }
        body
    }

    fn record_llm_metrics(provider: &str, model: &str, success: bool) {
        let status = if success { "success" } else { "error" };
        crate::metrics::LLM_REQUESTS
            .with_label_values(&[provider, model, status])
            .inc();
    }

    /// Exponential backoff with jitter: base * 2^attempt + pseudo-random jitter.
    fn retry_delay(attempt: u32) -> std::time::Duration {
        let base_ms = 1000u64 * 2u64.pow(attempt);
        let jitter_ms = (attempt as u64 * 137) % 500; // pseudo-random jitter
        std::time::Duration::from_millis(base_ms.min(30_000) + jitter_ms.min(1000))
    }

    /// Classify a provider error to decide whether the request is worth
    /// re-sending (RENG-35).
    ///
    /// The rule is the HTTP status, never free text: 408 / 429 / 5xx retry,
    /// every other 4xx is permanent, and an error carrying no status at all —
    /// the transport/DNS/TLS/serialization class — stays retryable, so the
    /// pre-RENG-35 behaviour for those paths is unchanged. An unparsable error
    /// is [`RetryVerdict::Unknown`], i.e. retried: a retry costs one attempt
    /// while a wrongly-permanent verdict would silently give up on a provider
    /// that was only briefly unreachable.
    fn retry_verdict(err: &anyhow::Error) -> RetryVerdict {
        match Self::http_status_code(err) {
            Some(status) if status == 408 || status == 429 || (500..600).contains(&status) => {
                RetryVerdict::Retryable { status }
            }
            Some(status) if (400..500).contains(&status) => RetryVerdict::Permanent { status },
            // Anything outside 4xx/5xx is not a verdict about this request.
            Some(_) | None => RetryVerdict::Unknown,
        }
    }

    /// The HTTP status a provider error carries, if there is one.
    ///
    /// Every HTTP-failure message in this crate's LLM stack has the shape
    /// `"<Provider> API returned <status> <reason>: <body>"`
    /// (`complete_direct`, `OpenAIProvider`, `AnthropicProvider`), where
    /// `<status>` is a `reqwest::StatusCode` rendered as `401 Unauthorized`.
    /// The status is read from the first `" returned "` marker of the *first*
    /// message in the chain that has one, which is what keeps a status-looking
    /// number inside a response body from being mistaken for the real verdict.
    /// [`anyhow::Error::chain`] is walked so the status survives the
    /// `.context(...)` wrappers the provider layer adds.
    fn http_status_code(err: &anyhow::Error) -> Option<u16> {
        const MARKER: &str = " returned ";
        err.chain().find_map(|cause| {
            let message = cause.to_string();
            let start = message.find(MARKER)? + MARKER.len();
            let digits: String = message[start..].chars().take_while(char::is_ascii_digit).collect();
            if digits.len() == 3 {
                digits.parse().ok()
            } else {
                None
            }
        })
    }

    /// Attribute a successful completion to the hitting config's `provider`
    /// (RENG-38): the provider instance name usually equals it (registry is
    /// keyed by config.provider), but the config entry is the source of truth
    /// for history snapshots — e.g. an empty `config.provider` falls back to
    /// the registry name instead of recording an empty string.
    ///
    /// `fallback` (RENG-55) marks a hit that was NOT the head of the chain:
    /// either a chain advance in [`Self::complete_with_fallback`] or — for
    /// direct [`Self::complete`] calls — whatever the caller passes.
    fn attribute_provider(mut result: CompletionResult, config: &LLMConfig, fallback: bool) -> CompletionResult {
        if !config.provider.is_empty() {
            result.provider = config.provider.clone();
        }
        result.fallback = fallback;
        // RENG-75: attribute the exact card, for the usage snapshot's `fp`.
        result.entry_fp = Some(config.entry_fp());
        result
    }

    /// Complete using a specific LLM config (backward-compatible API).
    ///
    /// The single-config entry point: there is no chain here, so a recorded
    /// sample (RENG-57) carries `chain_position = 1` / `attempt = 1`.
    pub async fn complete(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        self.complete_attempt(config, system_prompt, user_prompt, 1, 1).await
    }

    /// One attempt of one config — the body of [`Self::complete`], plus the
    /// RENG-57 latency sample.
    ///
    /// `chain_position` / `attempt` are the caller's bookkeeping for the
    /// recorded row ([`Self::complete_with_fallback`] passes the real ones);
    /// they do not change what is sent to the provider.
    ///
    /// The sample is written for BOTH outcomes, after the request returns and
    /// before the result is handed back: a failed attempt is recorded by the
    /// same path that records a successful one, which is what gives the page
    /// the call-level failure history the review-level snapshot cannot.
    async fn complete_attempt(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
        chain_position: u32,
        attempt: u32,
    ) -> Result<CompletionResult> {
        let started = std::time::Instant::now();
        let result = self.dispatch(config, system_prompt, user_prompt).await;
        self.record_sample(config, started.elapsed(), &result, chain_position, attempt)
            .await;
        result
    }

    /// The provider call itself: registry routing when the provider is known,
    /// the direct OpenAI-compatible HTTP path otherwise. Records the
    /// Prometheus metric and attributes the result to the hitting config.
    async fn dispatch(&self, config: &LLMConfig, system_prompt: &str, user_prompt: &str) -> Result<CompletionResult> {
        // If we have a provider registry, use it for better routing
        if let Some(ref registry) = self.provider_registry {
            if let Some(provider) = registry.get(&config.provider) {
                let params = CompletionParams {
                    model: config.model.clone(),
                    messages: Self::build_messages(system_prompt, user_prompt),
                    max_tokens: config.max_tokens,
                    temperature: config.temperature,
                    reasoning_effort: None,
                    disable_thinking: config.disable_thinking,
                };
                let result = provider.complete(&params).await;
                Self::record_llm_metrics(&config.provider, &config.model, result.is_ok());
                return result.map(|r| Self::attribute_provider(r, config, false));
            }
        }

        // Fallback: use the direct OpenAI-compatible HTTP approach (original behavior)
        let result = self.complete_direct(config, system_prompt, user_prompt).await;
        Self::record_llm_metrics(&config.provider, &config.model, result.is_ok());
        result.map(|r| Self::attribute_provider(r, config, false))
    }

    /// Hand one attempt's latency to the sink, if one is attached (RENG-57).
    ///
    /// Best-effort by contract: the sink logs its own write failure, and a
    /// recording problem never turns a successful completion into an error or
    /// hides the provider's own failure from the caller.
    async fn record_sample(
        &self,
        config: &LLMConfig,
        elapsed: std::time::Duration,
        result: &Result<CompletionResult>,
        chain_position: u32,
        attempt: u32,
    ) {
        let Some(sink) = &self.sink else { return };
        sink.record(&LlmCallSample {
            at: Utc::now(),
            provider: config.provider.clone(),
            model: config.model.clone(),
            // RENG-75: which CARD served the attempt, so same-named cards
            // aggregate separately. Never logged (it hashes the key).
            entry_fp: config.entry_fp(),
            latency_ms: elapsed.as_millis() as u64,
            success: result.is_ok(),
            error: result.as_ref().err().map(|e| truncate_error(&format!("{e:#}"))),
            chain_position,
            attempt,
        })
        .await;
    }

    /// Direct HTTP-based completion (backward compat, OpenAI-compatible only).
    async fn complete_direct(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        let _start = std::time::Instant::now();

        // Validate API base URL early so we give a helpful error instead of
        // reqwest's cryptic "builder error".
        let base = config.api_base.trim();
        if base.is_empty() || !base.starts_with("http") {
            // If api_base is empty, check if the user might have used `base_url`
            // (a common alias that we support via serde(alias)).
            anyhow::bail!(
                "LLM config '{}' has no api_base set. \
                 Use api_base = \"https://api.example.com/v1\" or \
                 LLM_CONFIG environment variable.",
                config.provider,
            );
        }
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let body = Self::build_chat_request_body(config, system_prompt, user_prompt);

        let latency_send = _start.elapsed();
        let resp = self
            .inner
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("builder error") {
                    anyhow::anyhow!(
                        "LLM request failed: invalid API base URL '{}'. \
                         Check api_base in your config — it should be like \
                         https://api.deepseek.com or https://api.openai.com/v1",
                        config.api_base,
                    )
                } else if msg.contains("dns error") || msg.contains("DNS") {
                    anyhow::anyhow!(
                        "LLM request failed: DNS resolution error for '{}'. \
                         Check api_base and network connectivity.",
                        config.api_base,
                    )
                } else if msg.contains("tls") || msg.contains("certificate") {
                    anyhow::anyhow!(
                        "LLM request failed: TLS error when connecting to '{}'. \
                         Try using http:// instead of https:// for local endpoints.",
                        config.api_base,
                    )
                } else {
                    anyhow::anyhow!("LLM request failed: {e}")
                }
            })?;

        let latency_resp = _start.elapsed();
        tracing::debug!(
            "LLM call to {}: send={:?} resp={:?} total={:?}",
            config.model,
            latency_send,
            latency_resp - latency_send,
            _start.elapsed()
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API returned {status}: {text}");
        }

        tracing::debug!("parsing JSON at {:?}", _start.elapsed());
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {}", e))?;
        tracing::debug!("JSON parsed at {:?}", _start.elapsed());

        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("LLM response missing content"))?
            .to_string();

        let total_tokens = value["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let model = value["model"].as_str().unwrap_or(&config.model).to_string();

        Ok(CompletionResult {
            content,
            total_tokens,
            model,
            provider: config.provider.clone(),
            fallback: false,
            // Filled by `attribute_provider` on the way out.
            entry_fp: None,
        })
    }

    /// Complete with fallback across multiple configs.
    ///
    /// For each provider, retries up to 3 times with exponential backoff + jitter
    /// when the failure is plausibly transient: a 408 / 429 / 5xx status, or an
    /// error that carries no status at all (transport/DNS/TLS/timeout) — see
    /// [`Self::retry_verdict`]. Any other 4xx is permanent: the request is
    /// returned after a single attempt, with no further attempt and no backoff
    /// sleep, and the provider's status and reason are preserved verbatim so a
    /// credential problem reads as one.
    ///
    /// A permanent verdict is a verdict about *that config*, not about the
    /// chain: the walk continues to the next entry, because the next entry is a
    /// different provider with its own credentials — the very case the chain
    /// exists for (the RENG-55 fallback logging covers it unchanged). The total
    /// cost stays bounded by one attempt per config.
    pub async fn complete_with_fallback(
        &self,
        configs: &[LLMConfig],
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        let mut last_error = anyhow::anyhow!("no LLM configs provided");
        let _cf_start = std::time::Instant::now();
        let max_retries = 3u32;

        // Head of the chain = the primary provider (RENG-55). Everything past
        // index 0 is a fallback; a hit there is logged at INFO so "the review
        // ran on the secondary provider" is visible in the logs instead of
        // being indistinguishable from a normal run.
        let primary = configs.first().map(|c| c.provider.clone()).unwrap_or_default();

        tracing::debug!(
            "complete_with_fallback: {} config(s), system={}b user={}b",
            configs.len(),
            system_prompt.len(),
            user_prompt.len()
        );

        for (i, config) in configs.iter().enumerate() {
            for attempt in 0..max_retries {
                let _attempt_start = std::time::Instant::now();
                // RENG-57: the sample this attempt produces must name where in
                // the chain it happened and which retry it was, so a failed
                // primary attempt is distinguishable from a retry of it.
                let result = self
                    .complete_attempt(config, system_prompt, user_prompt, i as u32 + 1, attempt + 1)
                    .await;
                let attempt_dur = _attempt_start.elapsed();

                match result {
                    Ok(r) => {
                        let fallback = i > 0;
                        if fallback {
                            tracing::info!(
                                primary_provider = %primary,
                                used_provider = %config.provider,
                                used_model = %config.model,
                                chain_position = i + 1,
                                attempt = attempt + 1,
                                took = ?attempt_dur,
                                "LLM fallback engaged: the primary provider did not answer, \
                                 this call was served by a later chain entry"
                            );
                        }
                        tracing::debug!(
                            "Fallback attempt {}/{} SUCCESS: model={} took={:?} total={:?}",
                            i + 1,
                            attempt + 1,
                            config.model,
                            attempt_dur,
                            _cf_start.elapsed()
                        );
                        // RENG-38: attribute the hit to THIS config's provider
                        // — the fallback chain may have succeeded on a later
                        // entry than the caller's primary. RENG-55: flag it.
                        return Ok(Self::attribute_provider(r, config, fallback));
                    }
                    Err(e) => {
                        let verdict = Self::retry_verdict(&e);
                        let is_retriable = matches!(verdict, RetryVerdict::Retryable { .. } | RetryVerdict::Unknown);

                        if is_retriable && attempt + 1 < max_retries {
                            let delay = Self::retry_delay(attempt);
                            tracing::warn!(
                                provider = %config.provider,
                                model = %config.model,
                                attempt = attempt + 1,
                                max_retries = max_retries,
                                retry_delay_ms = delay.as_millis(),
                                error = %e,
                                "LLM request failed, retrying with backoff"
                            );
                            tokio::time::sleep(delay).await;
                            last_error = e;
                            continue;
                        }

                        // RENG-35: a permanent verdict gives up right here — no
                        // second attempt and no backoff sleep — and names the
                        // status so the log reads as "this is the credential /
                        // request, not a flaky provider".
                        if let RetryVerdict::Permanent { status } = verdict {
                            tracing::warn!(
                                provider = %config.provider,
                                model = %config.model,
                                status,
                                attempt = attempt + 1,
                                max_retries = max_retries,
                                took = ?attempt_dur,
                                error = %e,
                                "LLM request failed permanently ({status}), not retrying"
                            );
                        }

                        // RENG-55: structured INFO — the user-visible symptom was
                        // "primary set to deepseek, every review ran on xiaomi"
                        // with nothing in the logs naming the skip. `reason` is
                        // the provider error verbatim; `next_*` names where the
                        // chain goes. The terminal failure of the LAST entry
                        // stays a WARN below this branch's tail.
                        if i + 1 < configs.len() {
                            tracing::info!(
                                primary_provider = %primary,
                                chain_position = i + 1,
                                provider = %config.provider,
                                model = %config.model,
                                next_provider = %configs[i + 1].provider,
                                next_model = %configs[i + 1].model,
                                attempt = attempt + 1,
                                took = ?attempt_dur,
                                reason = %e,
                                "LLM request failed, falling back to the next provider in the chain"
                            );
                        } else {
                            tracing::warn!(
                                provider = %config.provider,
                                model = %config.model,
                                attempt = attempt + 1,
                                took = ?attempt_dur,
                                reason = %e,
                                "LLM request failed on the last provider in the chain"
                            );
                        }
                        last_error = e;
                        break; // try next config
                    }
                }
            }
        }

        tracing::error!("all LLM configs exhausted");
        Err(last_error).context("all LLM providers failed")
    }
}
