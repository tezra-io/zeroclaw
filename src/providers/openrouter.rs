use crate::providers::openai_format;
use crate::providers::traits::{ChatProvider, Provider};
use crate::types::{ChatMessage, ChatResponse, ToolSpec};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenRouterProvider {
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LegacyChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

impl OpenRouterProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn require_key(&self) -> anyhow::Result<&str> {
        self.api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var."))
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.require_key()?;

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature,
        };

        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header(
                "HTTP-Referer",
                "https://github.com/theonlyhennygod/zeroclaw",
            )
            .header("X-Title", "ZeroClaw")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("OpenRouter API error: {error}");
        }

        let chat_response: LegacyChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))
    }
}

#[async_trait]
impl ChatProvider for OpenRouterProvider {
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.require_key()?;

        let request = openai_format::ChatCompletionRequest {
            model: model.to_string(),
            messages: openai_format::to_wire_messages(system_prompt, messages),
            temperature,
            tools: openai_format::to_wire_tools(tools),
        };

        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header(
                "HTTP-Referer",
                "https://github.com/theonlyhennygod/zeroclaw",
            )
            .header("X-Title", "ZeroClaw")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("OpenRouter API error: {error}");
        }

        let wire: openai_format::ChatCompletionResponse = response.json().await?;
        openai_format::from_wire_response(wire)
    }
}
