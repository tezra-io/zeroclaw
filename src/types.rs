use serde::{Deserialize, Serialize};

// ── Tool spec (moved from tools::traits, re-exported there) ──

/// Description of a tool for the LLM (provider-agnostic)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

// ── Chat types (provider-agnostic internal format) ──

/// Internal message representation. NOT any provider's wire format.
/// Each provider's `ChatProvider` impl translates to/from its native format.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatMessage {
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    #[default]
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON string of arguments
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_default() {
        let msg = ChatMessage::default();
        assert_eq!(msg.role, MessageRole::User);
        assert!(msg.content.is_none());
        assert!(msg.tool_calls.is_empty());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn chat_message_serde_roundtrip() {
        let msg = ChatMessage {
            role: MessageRole::Assistant,
            content: Some("Hello".into()),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "shell".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            }],
            tool_call_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, MessageRole::Assistant);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }

    #[test]
    fn message_role_serde() {
        assert_eq!(
            serde_json::to_string(&MessageRole::System).unwrap(),
            r#""system""#
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Tool).unwrap(),
            r#""tool""#
        );
    }

    #[test]
    fn tool_spec_serde_roundtrip() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: ToolSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
    }

    #[test]
    fn tool_result_serde_roundtrip() {
        let result = ToolResult {
            success: true,
            output: "done".into(),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.output, "done");
    }

    #[test]
    fn chat_response_serde() {
        let resp = ChatResponse {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: Some("hi".into()),
                ..Default::default()
            },
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message.role, MessageRole::Assistant);
        assert_eq!(parsed.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn chat_message_skips_empty_fields_in_json() {
        let msg = ChatMessage {
            role: MessageRole::User,
            content: Some("hello".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }
}
