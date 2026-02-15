//! Shared OpenAI-compatible wire format for chat completions with tool calling.
//! Used by: openrouter, openai, ollama, compatible providers.

use crate::types::{ChatMessage, ChatResponse, MessageRole, TokenUsage, ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};

// ── Request types ───────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    pub temperature: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
}

#[derive(Debug, Serialize)]
pub struct WireMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: WireFunction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WireFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct WireTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: WireToolFunction,
}

#[derive(Debug, Serialize)]
pub struct WireToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ── Response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<ResponseChoice>,
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseChoice {
    pub message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseToolCall {
    pub id: String,
    pub function: WireFunction,
}

#[derive(Debug, Deserialize)]
pub struct ResponseUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

// ── Conversion functions ────────────────────────────────────

/// Convert internal `ChatMessage` list to `OpenAI` wire messages,
/// optionally prepending a system prompt.
pub fn to_wire_messages(system_prompt: Option<&str>, messages: &[ChatMessage]) -> Vec<WireMessage> {
    let mut wire = Vec::with_capacity(messages.len() + 1);

    if let Some(sys) = system_prompt {
        wire.push(WireMessage {
            role: "system".into(),
            content: Some(sys.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        let role = match msg.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };

        let tool_calls = if msg.tool_calls.is_empty() {
            None
        } else {
            Some(
                msg.tool_calls
                    .iter()
                    .map(|tc| WireToolCall {
                        id: tc.id.clone(),
                        call_type: "function".into(),
                        function: WireFunction {
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        },
                    })
                    .collect(),
            )
        };

        wire.push(WireMessage {
            role: role.into(),
            content: msg.content.clone(),
            tool_calls,
            tool_call_id: msg.tool_call_id.clone(),
        });
    }

    wire
}

/// Convert `ToolSpec` list to `OpenAI` wire tool definitions.
pub fn to_wire_tools(specs: &[ToolSpec]) -> Vec<WireTool> {
    specs
        .iter()
        .map(|s| WireTool {
            tool_type: "function".into(),
            function: WireToolFunction {
                name: s.name.clone(),
                description: s.description.clone(),
                parameters: s.parameters.clone(),
            },
        })
        .collect()
}

/// Convert an `OpenAI` wire response to our internal `ChatResponse`.
pub fn from_wire_response(resp: ChatCompletionResponse) -> anyhow::Result<ChatResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;

    let tool_calls: Vec<ToolCall> = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| ToolCall {
            id: tc.id,
            name: tc.function.name,
            arguments: tc.function.arguments,
        })
        .collect();

    let usage = resp.usage.map(|u| TokenUsage {
        prompt_tokens: u.prompt_tokens.unwrap_or(0),
        completion_tokens: u.completion_tokens.unwrap_or(0),
    });

    Ok(ChatResponse {
        message: ChatMessage {
            role: MessageRole::Assistant,
            content: choice.message.content,
            tool_calls,
            tool_call_id: None,
        },
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wire_messages_prepends_system() {
        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("hello".into()),
            ..Default::default()
        }];
        let wire = to_wire_messages(Some("be helpful"), &messages);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].role, "system");
        assert_eq!(wire[0].content.as_deref(), Some("be helpful"));
        assert_eq!(wire[1].role, "user");
    }

    #[test]
    fn to_wire_messages_without_system() {
        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("hi".into()),
            ..Default::default()
        }];
        let wire = to_wire_messages(None, &messages);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].role, "user");
    }

    #[test]
    fn to_wire_messages_with_tool_calls() {
        let messages = vec![ChatMessage {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "shell".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            }],
            tool_call_id: None,
        }];
        let wire = to_wire_messages(None, &messages);
        assert!(wire[0].tool_calls.is_some());
        let tcs = wire[0].tool_calls.as_ref().unwrap();
        assert_eq!(tcs[0].function.name, "shell");
        assert_eq!(tcs[0].call_type, "function");
    }

    #[test]
    fn to_wire_messages_tool_result() {
        let messages = vec![ChatMessage {
            role: MessageRole::Tool,
            content: Some("file list".into()),
            tool_call_id: Some("call_1".into()),
            ..Default::default()
        }];
        let wire = to_wire_messages(None, &messages);
        assert_eq!(wire[0].role, "tool");
        assert_eq!(wire[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn to_wire_tools_converts_specs() {
        let specs = vec![ToolSpec {
            name: "shell".into(),
            description: "run commands".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let wire = to_wire_tools(&specs);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].tool_type, "function");
        assert_eq!(wire[0].function.name, "shell");
    }

    #[test]
    fn to_wire_tools_empty() {
        let wire = to_wire_tools(&[]);
        assert!(wire.is_empty());
    }

    #[test]
    fn from_wire_response_text_only() {
        let wire = ChatCompletionResponse {
            choices: vec![ResponseChoice {
                message: ResponseMessage {
                    content: Some("hello".into()),
                    tool_calls: None,
                },
            }],
            usage: Some(ResponseUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
            }),
        };
        let resp = from_wire_response(wire).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("hello"));
        assert!(resp.message.tool_calls.is_empty());
        assert_eq!(resp.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn from_wire_response_with_tool_calls() {
        let wire = ChatCompletionResponse {
            choices: vec![ResponseChoice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![ResponseToolCall {
                        id: "call_1".into(),
                        function: WireFunction {
                            name: "shell".into(),
                            arguments: r#"{"cmd":"ls"}"#.into(),
                        },
                    }]),
                },
            }],
            usage: None,
        };
        let resp = from_wire_response(wire).unwrap();
        assert!(resp.message.content.is_none());
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].name, "shell");
    }

    #[test]
    fn from_wire_response_empty_choices_errors() {
        let wire = ChatCompletionResponse {
            choices: vec![],
            usage: None,
        };
        assert!(from_wire_response(wire).is_err());
    }

    #[test]
    fn request_serializes_with_tools() {
        let req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![WireMessage {
                role: "user".into(),
                content: Some("hi".into()),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: 0.7,
            tools: vec![WireTool {
                tool_type: "function".into(),
                function: WireToolFunction {
                    name: "shell".into(),
                    description: "run".into(),
                    parameters: serde_json::json!({}),
                },
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"function\""));
        assert!(json.contains("\"tools\""));
    }

    #[test]
    fn request_serializes_without_tools() {
        let req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![WireMessage {
                role: "user".into(),
                content: Some("hi".into()),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: 0.7,
            tools: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"tools\""));
    }
}
