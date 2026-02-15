use super::traits::{ChatProvider, Provider};
use crate::types::{ChatMessage, ChatResponse, ToolSpec};
use async_trait::async_trait;
use std::time::Duration;

/// Provider wrapper with retry + fallback behavior (legacy string API).
pub struct ReliableProvider {
    providers: Vec<(String, Box<dyn Provider>)>,
    max_retries: u32,
    base_backoff_ms: u64,
}

impl ReliableProvider {
    pub fn new(
        providers: Vec<(String, Box<dyn Provider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
        }
    }
}

#[async_trait]
impl Provider for ReliableProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut failures = Vec::new();

        for (provider_name, provider) in &self.providers {
            let mut backoff_ms = self.base_backoff_ms;

            for attempt in 0..=self.max_retries {
                match provider
                    .chat_with_system(system_prompt, message, model, temperature)
                    .await
                {
                    Ok(resp) => {
                        if attempt > 0 {
                            tracing::info!(
                                provider = provider_name,
                                attempt,
                                "Provider recovered after retries"
                            );
                        }
                        return Ok(resp);
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{provider_name} attempt {}/{}: {e}",
                            attempt + 1,
                            self.max_retries + 1
                        ));

                        if attempt < self.max_retries {
                            tracing::warn!(
                                provider = provider_name,
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                "Provider call failed, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                        }
                    }
                }
            }

            tracing::warn!(provider = provider_name, "Switching to fallback provider");
        }

        anyhow::bail!("All providers failed. Attempts:\n{}", failures.join("\n"))
    }
}

/// `ChatProvider` wrapper with retry + fallback behavior (structured API with tool support).
pub struct ReliableChatProvider {
    providers: Vec<(String, Box<dyn ChatProvider>)>,
    max_retries: u32,
    base_backoff_ms: u64,
}

impl ReliableChatProvider {
    pub fn new(
        providers: Vec<(String, Box<dyn ChatProvider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
        }
    }
}

#[async_trait]
impl ChatProvider for ReliableChatProvider {
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let mut failures = Vec::new();

        for (provider_name, provider) in &self.providers {
            let mut backoff_ms = self.base_backoff_ms;

            for attempt in 0..=self.max_retries {
                match provider
                    .chat_completion(system_prompt, messages, tools, model, temperature)
                    .await
                {
                    Ok(resp) => {
                        if attempt > 0 {
                            tracing::info!(
                                provider = provider_name,
                                attempt,
                                "ChatProvider recovered after retries"
                            );
                        }
                        return Ok(resp);
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{provider_name} attempt {}/{}: {e}",
                            attempt + 1,
                            self.max_retries + 1
                        ));

                        if attempt < self.max_retries {
                            tracing::warn!(
                                provider = provider_name,
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                "ChatProvider call failed, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                        }
                    }
                }
            }

            tracing::warn!(
                provider = provider_name,
                "Switching to fallback ChatProvider"
            );
        }

        anyhow::bail!(
            "All chat providers failed. Attempts:\n{}",
            failures.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MessageRole;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response: &'static str,
        error: &'static str,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(self.response.to_string())
        }
    }

    struct MockChatProvider {
        calls: Arc<AtomicUsize>,
        fail_until: usize,
    }

    #[async_trait]
    impl ChatProvider for MockChatProvider {
        async fn chat_completion(
            &self,
            _system_prompt: Option<&str>,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until {
                anyhow::bail!("mock error");
            }
            Ok(ChatResponse {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some("ok".into()),
                    ..Default::default()
                },
                usage: None,
            })
        }
    }

    // ── ReliableProvider tests ───────────────────────────────

    #[tokio::test]
    async fn succeeds_without_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            2,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "recovered",
                    error: "temporary",
                }),
            )],
            2,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn falls_back_after_retries_exhausted() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "fallback down",
                    }),
                ),
            ],
            1,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "from fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_aggregated_error_when_all_providers_fail() {
        let provider = ReliableProvider::new(
            vec![
                (
                    "p1".into(),
                    Box::new(MockProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p1 error",
                    }),
                ),
                (
                    "p2".into(),
                    Box::new(MockProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p2 error",
                    }),
                ),
            ],
            0,
            1,
        );

        let err = provider
            .chat("hello", "test", 0.0)
            .await
            .expect_err("all providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All providers failed"));
        assert!(msg.contains("p1 attempt 1/1"));
        assert!(msg.contains("p2 attempt 1/1"));
    }

    // ── ReliableChatProvider tests ──────────────────────────

    #[tokio::test]
    async fn chat_provider_retry_and_recover() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableChatProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockChatProvider {
                    calls: Arc::clone(&calls),
                    fail_until: 1,
                }) as Box<dyn ChatProvider>,
            )],
            2,
            1,
        );

        let result = provider
            .chat_completion(None, &[], &[], "test", 0.7)
            .await
            .unwrap();
        assert_eq!(result.message.content.as_deref(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_provider_fallback() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableChatProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockChatProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until: usize::MAX,
                    }) as Box<dyn ChatProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(MockChatProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until: 0,
                    }) as Box<dyn ChatProvider>,
                ),
            ],
            0,
            1,
        );

        let result = provider
            .chat_completion(None, &[], &[], "test", 0.7)
            .await
            .unwrap();
        assert_eq!(result.message.content.as_deref(), Some("ok"));
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_provider_all_fail() {
        let provider = ReliableChatProvider::new(
            vec![(
                "p1".into(),
                Box::new(MockChatProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until: usize::MAX,
                }) as Box<dyn ChatProvider>,
            )],
            0,
            1,
        );

        let err = provider
            .chat_completion(None, &[], &[], "test", 0.7)
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("All chat providers failed"));
    }
}
