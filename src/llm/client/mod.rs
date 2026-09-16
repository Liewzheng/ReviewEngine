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
    /// The provider ANSWERED, but with nothing usable: an empty (or
    /// whitespace-only) completion (RENG-77 §4). Re-sending the identical
    /// request is not expected to help — the measured case is a reasoning
    /// model spending its whole `max_tokens` budget on `reasoning_tokens`, a
    /// property of the config's request shape — so this config is given up
    /// WITHOUT retry and the chain moves to the next entry, which is the
    /// provider that can actually answer.
    Empty,
}

/// The provider answered with a blank completion (RENG-77 §4).
///
/// A typed marker rather than a message match: [`LLMClient::retry_verdict`]
/// downcasts to it to give the config up without a retry, and the error text
/// is what reaches the review's `errors` list. Before this, an empty answer
/// was accepted as a successful call — `parse_llm_response` is fail-soft, so
/// it became an expert report with no findings and no raw response, and a
/// review that produced nothing reported itself as a clean, high-scoring pass.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmptyCompletion {
    provider: String,
    model: String,
}

impl std::fmt::Display for EmptyCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "provider '{}' (model '{}') returned an empty completion — no content to review. \
             A reasoning model may be spending its whole max_tokens budget on reasoning tokens; \
             set disable_thinking on this provider, or promote a provider that answers to the head of the chain",
            self.provider, self.model
        )
    }
}

impl std::error::Error for EmptyCompletion {}

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
        // An empty completion is a verdict about the CONFIG, not about the
        // moment: the same request went through and produced nothing (RENG-77
        // §4). Checked before the status parse because it carries no status.
        if err.downcast_ref::<EmptyCompletion>().is_some() {
            return RetryVerdict::Empty;
        }
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
    /// The model is attributed the same way (RENG-77): `result.model` is
    /// overwritten with the CONFIGURED `config.model`. Providers alias model
    /// ids in their responses (`deepseek-flash` for a configured
    /// `deepseek-v4-flash`, `gpt-4o-2024-11-20` for `gpt-4o`) and every
    /// consumer — `reviews.llm_summary`, `expert_reports.llm_model`, the
    /// review detail — looks the value up by what the CARD says, so the alias
    /// silently matched nothing (the provider's usage and latency showed 0).
    /// The alias is not lost: it is logged at DEBUG when it differs.
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
        if result.model != config.model {
            tracing::debug!(
                configured_model = %config.model,
                reported_model = %result.model,
                provider = %config.provider,
                "the provider reported a different model id; attributing by the configured one (RENG-77)"
            );
            result.model = config.model.clone();
        }
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
    ///
    /// RENG-77 §4: a blank completion is turned into an error HERE, before the
    /// caller can treat it as a report — see [`EmptyCompletion`]. This is the
    /// narrowest layer that sees every attempt (direct, chain, retry) and the
    /// one that owns the sample, so the recorded row and the returned outcome
    /// can never disagree about whether the call produced anything.
    async fn complete_attempt(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
        chain_position: u32,
        attempt: u32,
    ) -> Result<CompletionResult> {
        let started = std::time::Instant::now();
        let (result, ttfb_ms) = self.dispatch(config, system_prompt, user_prompt).await;
        let result = result.and_then(|r| {
            if r.content.trim().is_empty() {
                Err(anyhow::Error::new(EmptyCompletion {
                    provider: config.provider.clone(),
                    model: config.model.clone(),
                }))
            } else {
                Ok(r)
            }
        });
        self.record_sample(config, started.elapsed(), ttfb_ms, &result, chain_position, attempt)
            .await;
        result
    }

    /// The provider call itself: registry routing when the provider is known,
    /// the direct OpenAI-compatible HTTP path otherwise. Records the
    /// Prometheus metric and attributes the result to the hitting config.
    ///
    /// Returns `(Result, Option<u64>)` — the second element is the
    /// time-to-first-byte in whole milliseconds for the underlying HTTP
    /// exchange (RENG-77 §5). `None` for call paths that don't expose it
    /// (the registry providers wrap reqwest internally and never report it
    /// back); the direct OpenAI-compatible path measures the gap between
    /// request issue and response-headers received and reports it.
    async fn dispatch(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> (Result<CompletionResult>, Option<u64>) {
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
                // Registry providers own their own reqwest client: they can
                // expose TTFB later by changing the trait, but today they
                // don't, so the recorded sample carries `ttfb_ms: None`.
                return (result.map(|r| Self::attribute_provider(r, config, false)), None);
            }
        }

        // Fallback: use the direct OpenAI-compatible HTTP approach (original behavior)
        let (result, ttfb_ms) = self.complete_direct(config, system_prompt, user_prompt).await;
        Self::record_llm_metrics(&config.provider, &config.model, result.is_ok());
        (result.map(|r| Self::attribute_provider(r, config, false)), ttfb_ms)
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
        ttfb_ms: Option<u64>,
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
            // RENG-77 §5: the time-to-first-byte of THIS attempt. `None` when
            // the underlying provider does not expose it (registry path);
            // `Some(_)` for the direct OpenAI-compatible path, where it is
            // the gap between request issue and response-headers received.
            ttfb_ms,
            success: result.is_ok(),
            error: result.as_ref().err().map(|e| truncate_error(&format!("{e:#}"))),
            chain_position,
            attempt,
        })
        .await;
    }

    /// Direct HTTP-based completion (backward compat, OpenAI-compatible only).
    ///
    /// Returns `(Result<CompletionResult>, Option<u64>)`: the second element
    /// is the time-to-first-byte (the gap between the request being issued
    /// and the response headers being received), in whole milliseconds.
    /// `Some(_)` on the direct path because the reqwest `send()` await returns
    /// once headers arrive — there is no body read happening yet — so the
    /// elapsed at that point IS the TTFB. `None` if the request errored
    /// before headers came back (DNS / TLS / connection refused / timeout).
    async fn complete_direct(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> (Result<CompletionResult>, Option<u64>) {
        let _start = std::time::Instant::now();

        // Validate API base URL early so we give a helpful error instead of
        // reqwest's cryptic "builder error".
        let base = config.api_base.trim();
        if base.is_empty() || !base.starts_with("http") {
            // If api_base is empty, check if the user might have used `base_url`
            // (a common alias that we support via serde(alias)).
            return (
                Err(anyhow::anyhow!(
                    "LLM config '{}' has no api_base set. \
                     Use api_base = \"https://api.example.com/v1\" or \
                     LLM_CONFIG environment variable.",
                    config.provider,
                )),
                None,
            );
        }
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let body = Self::build_chat_request_body(config, system_prompt, user_prompt);

        let resp = self
            .inner
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;
        // RENG-77 §5: `send()` resolves when the response headers arrive
        // (the response body has not been read yet), so the elapsed since
        // `_start` at this point is the time-to-first-byte. An error
        // before headers means no measurement, hence `None` — distinct
        // from a measured 0 ms (the aggregate would still average those
        // in, and we do not want to).
        let (resp, ttfb_ms) = match resp {
            Ok(r) => (Ok(r), Some(_start.elapsed().as_millis() as u64)),
            Err(e) => {
                let msg = e.to_string();
                let mapped = if msg.contains("builder error") {
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
                };
                (Err(mapped), None)
            }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return (Err(e), ttfb_ms),
        };

        tracing::debug!(
            "LLM call to {}: ttfb={:?}ms total={:?}ms",
            config.model,
            ttfb_ms,
            _start.elapsed()
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return (Err(anyhow::anyhow!("LLM API returned {status}: {text}")), ttfb_ms);
        }

        let value: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return (Err(anyhow::anyhow!("Failed to parse LLM response: {}", e)), ttfb_ms),
        };

        let content = match value["choices"][0]["message"]["content"].as_str() {
            Some(s) => s.to_string(),
            None => return (Err(anyhow::anyhow!("LLM response missing content")), ttfb_ms),
        };

        let total_tokens = value["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let model = value["model"].as_str().unwrap_or(&config.model).to_string();

        (
            Ok(CompletionResult {
                content,
                total_tokens,
                model,
                provider: config.provider.clone(),
                fallback: false,
                // Filled by `attribute_provider` on the way out.
                entry_fp: None,
            }),
            ttfb_ms,
        )
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
    ///
    /// RENG-77 §4 adds a third non-retriable verdict: an EMPTY completion. The
    /// provider answered, but with nothing to review, so the config is skipped
    /// without a retry and the chain advances to a provider that answers — a
    /// chain of one then fails the expert, which is what surfaces the problem
    /// instead of reporting a clean, empty, high-scoring review.
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
                        // RENG-77 §4: the provider answered with nothing. The
                        // request itself was accepted, so a retry would repeat
                        // it — the config is given up and the chain advances.
                        if let RetryVerdict::Empty = verdict {
                            tracing::warn!(
                                provider = %config.provider,
                                model = %config.model,
                                attempt = attempt + 1,
                                took = ?attempt_dur,
                                "LLM provider returned an empty completion; not retrying this provider (RENG-77)"
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
