use crate::types::{ChatMessage, ChatResponse, ToolSpec};
use async_trait::async_trait;

/// Legacy single-turn provider (string in, string out).
/// Kept for backward compatibility with channels, heartbeat, etc.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;
}

/// Structured chat with tool support.
/// The system prompt is passed separately (not as a message) because
/// Anthropic requires it as a top-level field, not a message role.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse>;
}
