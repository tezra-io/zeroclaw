use crate::providers::traits::{ChatProvider, Provider};
use crate::types::{ChatMessage, ChatResponse, MessageRole, TokenUsage, ToolCall, ToolSpec};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct AnthropicProvider {
    api_key: Option<String>,
    client: Client,
}

// ── Legacy request/response types (Provider trait) ──────────

#[derive(Debug, Serialize)]
struct LegacyChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<LegacyMessage>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct LegacyMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LegacyChatResponse {
    content: Vec<LegacyContentBlock>,
}

#[derive(Debug, Deserialize)]
struct LegacyContentBlock {
    text: String,
}

// ── Structured request/response types (ChatProvider trait) ──

#[derive(Debug, Serialize)]
struct AnthropicChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

/// Anthropic messages use either a string or an array of content blocks.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicChatResponse {
    content: Vec<ResponseContentBlock>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

impl AnthropicProvider {
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
        self.api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("Anthropic API key not set. Set ANTHROPIC_API_KEY or edit config.toml.")
        })
    }

    /// Apply auth header based on token type.
    /// OAuth tokens (sk-ant-oat*) use `Authorization: Bearer`.
    /// Standard API keys use `x-api-key`.
    #[allow(clippy::unused_self)]
    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        if api_key.starts_with("sk-ant-oat") {
            builder.header("authorization", format!("Bearer {api_key}"))
        } else {
            builder.header("x-api-key", api_key)
        }
    }
}

/// Convert internal `ChatMessage` list to Anthropic's wire format.
/// Anthropic rules:
/// - System prompt is a separate top-level field (handled by caller)
/// - Messages must alternate user/assistant (no system role in messages)
/// - Tool use is returned as content blocks in assistant messages
/// - Tool results are sent as user messages with `tool_result` content blocks
fn to_anthropic_messages(messages: &[ChatMessage]) -> Vec<AnthropicMessage> {
    let mut result = Vec::with_capacity(messages.len());

    for msg in messages {
        match msg.role {
            MessageRole::System => {
                // System messages are handled via the top-level `system` field.
                // If one sneaks in here, treat as user message.
                if let Some(ref content) = msg.content {
                    result.push(AnthropicMessage {
                        role: "user".into(),
                        content: AnthropicContent::Text(content.clone()),
                    });
                }
            }
            MessageRole::User => {
                if let Some(ref content) = msg.content {
                    result.push(AnthropicMessage {
                        role: "user".into(),
                        content: AnthropicContent::Text(content.clone()),
                    });
                }
            }
            MessageRole::Assistant => {
                let mut blocks = Vec::new();

                if let Some(ref content) = msg.content {
                    if !content.is_empty() {
                        blocks.push(AnthropicContentBlock::Text {
                            text: content.clone(),
                        });
                    }
                }

                for tc in &msg.tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                    blocks.push(AnthropicContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input,
                    });
                }

                if blocks.is_empty() {
                    // Anthropic requires non-empty content
                    blocks.push(AnthropicContentBlock::Text {
                        text: String::new(),
                    });
                }

                result.push(AnthropicMessage {
                    role: "assistant".into(),
                    content: AnthropicContent::Blocks(blocks),
                });
            }
            MessageRole::Tool => {
                // Anthropic: tool results are user messages with tool_result blocks
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                let content = msg.content.clone().unwrap_or_default();
                result.push(AnthropicMessage {
                    role: "user".into(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    }]),
                });
            }
        }
    }

    result
}

/// Convert Anthropic response content blocks to our internal format.
fn from_anthropic_response(resp: AnthropicChatResponse) -> ChatResponse {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in resp.content {
        match block {
            ResponseContentBlock::Text { text } => {
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
            ResponseContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: serde_json::to_string(&input).unwrap_or_default(),
                });
            }
        }
    }

    let content = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join("\n"))
    };

    let usage = resp.usage.map(|u| TokenUsage {
        prompt_tokens: u.input_tokens.unwrap_or(0),
        completion_tokens: u.output_tokens.unwrap_or(0),
    });

    ChatResponse {
        message: ChatMessage {
            role: MessageRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        },
        usage,
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.require_key()?;

        let request = LegacyChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt.map(ToString::to_string),
            messages: vec![LegacyMessage {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            temperature,
        };

        let builder = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request);
        let response = self.apply_auth(builder, api_key).send().await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Anthropic API error: {error}");
        }

        let chat_response: LegacyChatResponse = response.json().await?;

        chat_response
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .ok_or_else(|| anyhow::anyhow!("No response from Anthropic"))
    }
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.require_key()?;

        let anthropic_tools: Vec<AnthropicTool> = tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect();

        let request = AnthropicChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt.map(ToString::to_string),
            messages: to_anthropic_messages(messages),
            tools: anthropic_tools,
            temperature,
        };

        let builder = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request);
        let response = self.apply_auth(builder, api_key).send().await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Anthropic API error: {error}");
        }

        let wire: AnthropicChatResponse = response.json().await?;
        Ok(from_anthropic_response(wire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = AnthropicProvider::new(Some("sk-ant-test123"));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some("sk-ant-test123"));
    }

    #[test]
    fn oauth_token_uses_bearer_header() {
        let p = AnthropicProvider::new(Some("sk-ant-oat01-test123"));
        let client = reqwest::Client::new();
        let builder = client.post("https://api.anthropic.com/v1/messages");
        let req = p
            .apply_auth(builder, "sk-ant-oat01-test123")
            .build()
            .unwrap();
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer sk-ant-oat01-test123"
        );
        assert!(req.headers().get("x-api-key").is_none());
    }

    #[test]
    fn standard_key_uses_x_api_key_header() {
        let p = AnthropicProvider::new(Some("sk-ant-api03-test123"));
        let client = reqwest::Client::new();
        let builder = client.post("https://api.anthropic.com/v1/messages");
        let req = p
            .apply_auth(builder, "sk-ant-api03-test123")
            .build()
            .unwrap();
        assert_eq!(req.headers().get("x-api-key").unwrap(), "sk-ant-api03-test123");
        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicProvider::new(Some(""));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(None, "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("API key not set"),
            "Expected key error, got: {err}"
        );
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn chat_completion_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_completion(None, &[], &[], "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[test]
    fn legacy_request_serializes_without_system() {
        let req = LegacyChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![LegacyMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("system"),
            "system field should be skipped when None"
        );
        assert!(json.contains("claude-3-opus"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn legacy_request_serializes_with_system() {
        let req = LegacyChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are ZeroClaw".to_string()),
            messages: vec![LegacyMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\":\"You are ZeroClaw\""));
    }

    #[test]
    fn legacy_response_deserializes() {
        let json = r#"{"content":[{"type":"text","text":"Hello there!"}]}"#;
        let resp: LegacyChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].text, "Hello there!");
    }

    #[test]
    fn legacy_response_empty_content() {
        let json = r#"{"content":[]}"#;
        let resp: LegacyChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_empty());
    }

    #[test]
    fn legacy_response_multiple_blocks() {
        let json =
            r#"{"content":[{"type":"text","text":"First"},{"type":"text","text":"Second"}]}"#;
        let resp: LegacyChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.content[0].text, "First");
        assert_eq!(resp.content[1].text, "Second");
    }

    #[test]
    fn temperature_range_serializes() {
        for temp in [0.0, 0.5, 1.0, 2.0] {
            let req = LegacyChatRequest {
                model: "claude-3-opus".to_string(),
                max_tokens: 4096,
                system: None,
                messages: vec![],
                temperature: temp,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(&format!("{temp}")));
        }
    }

    // ── ChatProvider-specific tests ─────────────────────────

    #[test]
    fn to_anthropic_messages_user_text() {
        let msgs = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("hello".into()),
            ..Default::default()
        }];
        let wire = to_anthropic_messages(&msgs);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].role, "user");
    }

    #[test]
    fn to_anthropic_messages_assistant_with_tool_calls() {
        let msgs = vec![ChatMessage {
            role: MessageRole::Assistant,
            content: Some("Let me check".into()),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "shell".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            }],
            tool_call_id: None,
        }];
        let wire = to_anthropic_messages(&msgs);
        assert_eq!(wire[0].role, "assistant");
        // Should be blocks format
        if let AnthropicContent::Blocks(ref blocks) = wire[0].content {
            assert_eq!(blocks.len(), 2); // text + tool_use
        } else {
            panic!("Expected Blocks content");
        }
    }

    #[test]
    fn to_anthropic_messages_tool_result() {
        let msgs = vec![ChatMessage {
            role: MessageRole::Tool,
            content: Some("file list".into()),
            tool_call_id: Some("call_1".into()),
            ..Default::default()
        }];
        let wire = to_anthropic_messages(&msgs);
        assert_eq!(wire[0].role, "user"); // Tool results are user messages in Anthropic
    }

    #[test]
    fn from_anthropic_response_text_only() {
        let resp = AnthropicChatResponse {
            content: vec![ResponseContentBlock::Text {
                text: "Hello!".into(),
            }],
            usage: Some(AnthropicUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
            }),
        };
        let result = from_anthropic_response(resp);
        assert_eq!(result.message.content.as_deref(), Some("Hello!"));
        assert!(result.message.tool_calls.is_empty());
        assert_eq!(result.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn from_anthropic_response_with_tool_use() {
        let resp = AnthropicChatResponse {
            content: vec![
                ResponseContentBlock::Text {
                    text: "Let me run that".into(),
                },
                ResponseContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ],
            usage: None,
        };
        let result = from_anthropic_response(resp);
        assert_eq!(result.message.content.as_deref(), Some("Let me run that"));
        assert_eq!(result.message.tool_calls.len(), 1);
        assert_eq!(result.message.tool_calls[0].name, "shell");
        assert!(result.message.tool_calls[0].arguments.contains("command"));
    }

    #[test]
    fn from_anthropic_response_tool_use_only() {
        let resp = AnthropicChatResponse {
            content: vec![ResponseContentBlock::ToolUse {
                id: "call_1".into(),
                name: "file_read".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            }],
            usage: None,
        };
        let result = from_anthropic_response(resp);
        assert!(result.message.content.is_none());
        assert_eq!(result.message.tool_calls.len(), 1);
    }

    #[test]
    fn anthropic_request_serializes_with_tools() {
        let req = AnthropicChatRequest {
            model: "claude-3-opus".into(),
            max_tokens: 4096,
            system: Some("be helpful".into()),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: AnthropicContent::Text("hello".into()),
            }],
            tools: vec![AnthropicTool {
                name: "shell".into(),
                description: "run commands".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("input_schema"));
        assert!(json.contains("\"system\":\"be helpful\""));
    }

    #[test]
    fn anthropic_request_serializes_without_tools() {
        let req = AnthropicChatRequest {
            model: "claude-3-opus".into(),
            max_tokens: 4096,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: AnthropicContent::Text("hi".into()),
            }],
            tools: vec![],
            temperature: 0.5,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tools"));
        assert!(!json.contains("system"));
    }

    #[test]
    fn anthropic_response_deserializes_mixed() {
        let json = r#"{"content":[{"type":"text","text":"checking"},{"type":"tool_use","id":"c1","name":"shell","input":{"cmd":"ls"}}],"usage":{"input_tokens":50,"output_tokens":20}}"#;
        let resp: AnthropicChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.usage.unwrap().input_tokens, Some(50));
    }
}
