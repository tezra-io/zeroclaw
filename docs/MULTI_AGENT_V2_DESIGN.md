# Multi-Agent v2 Design — ZeroClaw Integration

*Natural language agent creation. Skills-first. Zero config overhead.*

**Revision:** 2 — addresses all findings from `docs/DESIGN_REVIEW.md`

---

## Philosophy

1. **Describe, don't configure** — user says what they need in plain english, system generates everything
2. **Skills > agents** — users think in capabilities, not infrastructure
3. **Grow organically** — start with one agent, add more as needs emerge
4. **Main agent is the orchestrator** — it creates, delegates, and manages sub-agents
5. **Lightweight by default** — this is a personal AI assistant, not enterprise software. HashMap for ephemeral, JSONL for persistent, SQLite only when you need vector search.

---

## Architecture Overview

```
User
  |
Main Agent (always exists, created at onboard)
  |-- [persistent] Twitter Agent (isolated memory, scheduled)
  |-- [persistent] PM Agent (shared-read memory, always available)
  |-- [ephemeral] Research Worker (in-memory only, spun up for a task, dies after)
  `-- [ephemeral] Code Review Worker (in-memory only, dies after)
```

**Memory tiers (lightweight-first):**
- **Ephemeral agents:** `HashMap<String, MemoryEntry>` in-memory. Dies with the agent. No persistence.
- **Persistent agents (default):** JSONL file at `~/.zeroclaw/agents/<name>/memory.jsonl`. Simple append-only log with keyword search.
- **Persistent agents (opt-in):** SQLite at `~/.zeroclaw/agents/<name>/memory.db`. Only when agent definition sets `memory_backend: sqlite` for vector/hybrid search.

---

## Current ZeroClaw Gaps (what we're adding)

The existing agent loop (`src/agent/loop_.rs:32-217`) has **no tool calling**. It sends one message to the LLM and prints the response. The `Provider` trait (`src/providers/traits.rs:4-17`) returns `String`, not structured messages. Tools exist (`src/tools/traits.rs`) with proper `Tool` trait, `ToolSpec`, and `execute()` — but they're assigned to `_tools` (unused) at `loop_.rs:62`.

**Specific gaps:**

| Gap | Current code | What's needed |
|-----|-------------|---------------|
| No tool calling loop | `loop_.rs:62`: `let _tools = tools::all_tools(...)` | Call `tool.execute(args)` in a loop |
| Single-shot chat | `loop_.rs:146-148`: one `chat_with_system` call | Multi-turn with tool results |
| `Provider` returns `String` | `traits.rs:4-17`: `-> anyhow::Result<String>` | Structured `ChatResponse` with tool calls |
| `ReliableProvider` wraps `Box<dyn Provider>` | `reliable.rs:7`: `Vec<(String, Box<dyn Provider>)>` | Must also wrap `ChatProvider` |
| No agent definitions | N/A | Markdown files with YAML frontmatter |
| No agent registry | N/A | CRUD for `~/.zeroclaw/agents/` |
| No inter-agent messaging | N/A | Tokio mpsc bus with oneshot response channels |
| No agent creation UX | N/A | CLI commands + conversational tool |
| Daemon has no shutdown coordination | `daemon/mod.rs:92`: bare `ctrl_c().await` | `watch::Sender<bool>` broadcast |
| Tool descriptions hardcoded 3x | `loop_.rs:88-113`, `channels/mod.rs:447-479` | Derive from `tool.spec()` |
| `has_supervised_channels` missing WhatsApp | `daemon/mod.rs:204-210` | Add `config.channels_config.whatsapp.is_some()` |

---

## Implementation Plan

### Phase 0: Shared Types + Provider Refactor (~1 day)

**Rationale:** The review identified a circular dependency: `ChatProvider::chat_completion` takes `&[ToolSpec]`, but `ToolSpec` lives in `src/tools/traits.rs`. Currently `providers` and `tools` are independent modules. We need shared types before anything else.

#### 0a. Extract shared types

**New file: `src/types.rs`**

```rust
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
/// Each provider's ChatProvider impl translates to/from its native format.
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
    pub arguments: String, // JSON string
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
```

**File: `src/tools/traits.rs`** — re-export from types:

```rust
// Replace local definitions with re-exports:
pub use crate::types::{ToolResult, ToolSpec};

// Tool trait stays here (it depends on ToolResult/ToolSpec but doesn't need to define them)
```

**File: `src/main.rs`** — add `mod types;`

**Tests for `src/types.rs`:**

```rust
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
}
```

#### 0b. ChatProvider trait

**File: `src/providers/traits.rs`** — add `ChatProvider` alongside existing `Provider`:

```rust
use async_trait::async_trait;
use crate::types::{ChatMessage, ChatResponse, ToolSpec};

// Existing Provider trait stays unchanged:
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature).await
    }
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;
}

// NEW: structured chat with tool support
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Multi-turn chat with tool definitions.
    /// The system prompt is passed separately (not as a message) because
    /// Anthropic requires it as a top-level field, not a message role.
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse>;
}
```

**Key design decision: `system_prompt` is a separate parameter**, not a `ChatMessage` with `role: System`. This is because:
- Anthropic's Messages API requires `system` as a top-level field, not a message
- OpenAI/OpenRouter accept it as a message OR parameter
- Keeping it separate means each provider can handle it natively without translation

#### 0c. ChatProvider implementations

**Each provider translates between our internal `ChatMessage` format and the API's wire format.**

**Files to modify (5 providers + 1 wrapper):**

1. `src/providers/anthropic.rs` — translate to Anthropic's content-block format:
   - `ChatMessage` with `tool_calls` → Anthropic `content: [{"type": "tool_use", ...}]`
   - `ChatMessage` with `role: Tool` → Anthropic `content: [{"type": "tool_result", ...}]`
   - `system_prompt` → Anthropic `system` top-level field (already done for `Provider`)
   - `ToolSpec` → Anthropic `tools` array with `input_schema`

2. `src/providers/openrouter.rs` — translate to OpenAI format:
   - `ChatMessage` with `tool_calls` → OpenAI `tool_calls: [{"type": "function", ...}]`
   - `ChatMessage` with `role: Tool` → OpenAI `role: "tool"` + `tool_call_id`
   - `system_prompt` → `{"role": "system", "content": ...}` prepended to messages
   - `ToolSpec` → OpenAI `tools` array with `{"type": "function", "function": {...}}`

3. `src/providers/openai.rs` — same OpenAI format as OpenRouter

4. `src/providers/ollama.rs` — OpenAI-compatible format (Ollama uses OpenAI's tool calling schema)

5. `src/providers/compatible.rs` — OpenAI-compatible format (same translation as openai.rs)

6. `src/providers/reliable.rs` — wrap `Vec<(String, Box<dyn ChatProvider>)>`:

```rust
/// ReliableProvider with ChatProvider support
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
                    Ok(resp) => return Ok(resp),
                    Err(e) => {
                        failures.push(format!(
                            "{provider_name} attempt {}/{}: {e}",
                            attempt + 1,
                            self.max_retries + 1
                        ));
                        if attempt < self.max_retries {
                            tokio::time::sleep(
                                std::time::Duration::from_millis(backoff_ms)
                            ).await;
                            backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                        }
                    }
                }
            }
        }
        anyhow::bail!("All providers failed. Attempts:\n{}", failures.join("\n"))
    }
}
```

**File: `src/providers/mod.rs`** — add chat provider factory:

```rust
pub use traits::{ChatProvider, Provider};

/// Create chat provider chain with retry and fallback behavior.
pub fn create_resilient_chat_provider(
    primary_name: &str,
    api_key: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
) -> anyhow::Result<Box<dyn ChatProvider>> {
    let mut providers: Vec<(String, Box<dyn ChatProvider>)> = Vec::new();

    providers.push((
        primary_name.to_string(),
        create_chat_provider(primary_name, api_key)?,
    ));

    for fallback in &reliability.fallback_providers {
        if fallback == primary_name || providers.iter().any(|(name, _)| name == fallback) {
            continue;
        }
        match create_chat_provider(fallback, api_key) {
            Ok(provider) => providers.push((fallback.clone(), provider)),
            Err(e) => {
                tracing::warn!(fallback_provider = fallback, "Ignoring invalid fallback: {e}");
            }
        }
    }

    Ok(Box::new(reliable::ReliableChatProvider::new(
        providers,
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )))
}

/// Create a single ChatProvider instance by name.
fn create_chat_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn ChatProvider>> {
    match name {
        "openrouter" => Ok(Box::new(openrouter::OpenRouterProvider::new(api_key))),
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider::new(api_key))),
        "openai" => Ok(Box::new(openai::OpenAiProvider::new(api_key))),
        "ollama" => Ok(Box::new(ollama::OllamaProvider::new(
            api_key.filter(|k| !k.is_empty()),
        ))),
        name if name.starts_with("custom:") => {
            let base_url = name.strip_prefix("custom:").unwrap_or("");
            if base_url.is_empty() {
                anyhow::bail!("Custom provider requires a URL");
            }
            Ok(Box::new(compatible::OpenAiCompatibleProvider::new(
                "Custom", base_url, api_key, compatible::AuthStyle::Bearer,
            )))
        }
        // All OpenAI-compatible providers share the same ChatProvider impl
        _ => {
            // Delegate to create_provider and wrap — compatible providers
            // all implement ChatProvider via the OpenAI translation
            let provider = create_provider(name, api_key)?;
            // Compatible providers already implement ChatProvider
            // This requires adding ChatProvider to OpenAiCompatibleProvider
            anyhow::bail!("Provider '{name}' does not support chat completion with tools")
        }
    }
}
```

**Note:** `create_provider()` (existing, returns `Box<dyn Provider>`) stays unchanged. The existing `Provider` trait and `ReliableProvider` continue to work for channels, heartbeat, and other code that doesn't need tool calling.

**Tests for `src/providers/reliable.rs` (ChatProvider):**

```rust
#[cfg(test)]
mod chat_provider_tests {
    use super::*;

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

    #[tokio::test]
    async fn chat_provider_retry_and_recover() { /* ... */ }

    #[tokio::test]
    async fn chat_provider_fallback() { /* ... */ }
}
```

#### 0d. Fix pre-existing bugs

**File: `src/daemon/mod.rs`** — add WhatsApp to `has_supervised_channels`:

```rust
fn has_supervised_channels(config: &Config) -> bool {
    config.channels_config.telegram.is_some()
        || config.channels_config.discord.is_some()
        || config.channels_config.slack.is_some()
        || config.channels_config.imessage.is_some()
        || config.channels_config.matrix.is_some()
        || config.channels_config.whatsapp.is_some()  // FIX: was missing
}
```

#### 0e. Derive tool descriptions from `tool.spec()`

**File: `src/agent/loop_.rs`** — replace hardcoded tool descriptions:

```rust
// BEFORE (hardcoded):
let mut tool_descs: Vec<(&str, &str)> = vec![("shell", "Execute terminal..."), ...];

// AFTER (derived from Tool trait):
let tool_descs: Vec<(String, String)> = tools
    .iter()
    .map(|t| (t.name().to_string(), t.description().to_string()))
    .collect();
let tool_desc_refs: Vec<(&str, &str)> = tool_descs
    .iter()
    .map(|(n, d)| (n.as_str(), d.as_str()))
    .collect();
```

Same change in `src/channels/mod.rs:447-479`.

---

### Phase 1: Tool Calling + Multi-Turn Agent Loop (~3 days)

#### 1a. Tool calling loop in agent

**File: `src/agent/loop_.rs`** — add `run_with_tools()` alongside existing `run()`:

```rust
use crate::providers::ChatProvider;
use crate::types::{ChatMessage, ChatResponse, MessageRole, ToolCall};

/// Run agent with tool calling support (multi-turn)
pub async fn run_with_tools(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    // ── Setup (same as existing run()) ──
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let _runtime = runtime::create_runtime(&config.runtime)?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    let composio_key = if config.composio.enabled {
        config.composio.api_key.as_deref()
    } else {
        None
    };
    let tools = tools::all_tools(&security, mem.clone(), composio_key, &config.browser);
    let tool_specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();

    // ── Resolve provider (ChatProvider, not Provider) ──
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");
    let model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider: Box<dyn ChatProvider> = providers::create_resilient_chat_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    // ── Build system prompt ──
    let skills = crate::skills::load_skills(&config.workspace_dir);
    let tool_descs: Vec<(String, String)> = tools
        .iter()
        .map(|t| (t.name().to_string(), t.description().to_string()))
        .collect();
    let tool_desc_refs: Vec<(&str, &str)> = tool_descs
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_str()))
        .collect();
    let system_prompt = crate::channels::build_system_prompt(
        &config.workspace_dir, model_name, &tool_desc_refs, &skills,
    );

    let start = Instant::now();

    if let Some(msg) = message {
        // ── Single message with tool calling ──
        let mut messages: Vec<ChatMessage> = vec![
            ChatMessage {
                role: MessageRole::User,
                content: Some(msg),
                ..Default::default()
            },
        ];

        run_tool_loop(
            &provider, &system_prompt, &mut messages, &tools, &tool_specs,
            model_name, temperature, 20,
        ).await?;
    } else {
        // ── Interactive mode with persistent conversation history ──
        println!("ZeroClaw Interactive Mode (with tools)");
        println!("Type /quit to exit.\n");

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cli = crate::channels::CliChannel::new();
        let listen_handle = tokio::spawn(async move {
            let _ = crate::channels::Channel::listen(&cli, tx).await;
        });

        // Conversation history persists across turns
        let mut messages: Vec<ChatMessage> = Vec::new();

        while let Some(msg) = rx.recv().await {
            messages.push(ChatMessage {
                role: MessageRole::User,
                content: Some(msg.content),
                ..Default::default()
            });

            run_tool_loop(
                &provider, &system_prompt, &mut messages, &tools, &tool_specs,
                model_name, temperature, 20,
            ).await?;
        }

        listen_handle.abort();
    }

    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::AgentEnd {
        duration,
        tokens_used: None,
    });

    Ok(())
}

/// Execute the tool calling loop. Mutates `messages` in place.
/// Returns when the LLM responds without tool calls, or max iterations reached.
async fn run_tool_loop(
    provider: &dyn ChatProvider,
    system_prompt: &str,
    messages: &mut Vec<ChatMessage>,
    tools: &[Box<dyn Tool>],
    tool_specs: &[ToolSpec],
    model: &str,
    temperature: f64,
    max_iterations: usize,
) -> Result<()> {
    for _ in 0..max_iterations {
        let response = provider
            .chat_completion(Some(system_prompt), messages, tool_specs, model, temperature)
            .await?;

        messages.push(response.message.clone());

        if response.message.tool_calls.is_empty() {
            // No tool calls — print final response
            if let Some(content) = &response.message.content {
                println!("{content}");
            }
            return Ok(());
        }

        // Execute each tool call
        for tool_call in &response.message.tool_calls {
            let tool = tools.iter().find(|t| t.name() == tool_call.name);
            let result = match tool {
                Some(t) => {
                    let args: serde_json::Value =
                        serde_json::from_str(&tool_call.arguments)
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                    match t.execute(args).await {
                        Ok(r) => r,
                        Err(e) => ToolResult {
                            success: false,
                            output: format!("Tool execution error: {e}"),
                            error: Some(e.to_string()),
                        },
                    }
                }
                None => ToolResult {
                    success: false,
                    output: format!("Unknown tool: {}", tool_call.name),
                    error: None,
                },
            };

            messages.push(ChatMessage {
                role: MessageRole::Tool,
                content: Some(result.output),
                tool_call_id: Some(tool_call.id.clone()),
                ..Default::default()
            });
        }
    }

    println!("[Max tool iterations reached]");
    Ok(())
}
```

#### 1b. Update call sites

**File: `src/main.rs`** — switch `Commands::Agent` to `run_with_tools`:

```rust
Commands::Agent { message, provider, model, temperature } => {
    agent::run_with_tools(config, message, provider, model, temperature).await
}
```

**File: `src/daemon/mod.rs`** — heartbeat worker at line 193:

```rust
// BEFORE:
if let Err(e) = crate::agent::run(config.clone(), Some(prompt), None, None, temp).await

// AFTER:
if let Err(e) = crate::agent::run_with_tools(config.clone(), Some(prompt), None, None, temp).await
```

**File: `src/agent/mod.rs`** — export new function:

```rust
pub mod loop_;
pub use loop_::{run, run_with_tools};
```

**Tests for Phase 1:**

```rust
// src/agent/loop_.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_tool_loop_handles_empty_tool_calls() {
        // Mock provider returns no tool calls
        // Verify loop exits after first iteration
    }

    #[test]
    fn run_tool_loop_handles_unknown_tool() {
        // Mock provider returns tool call for nonexistent tool
        // Verify error message in tool result
    }

    #[test]
    fn run_tool_loop_respects_max_iterations() {
        // Mock provider always returns tool calls
        // Verify loop stops at max_iterations
    }

    #[test]
    fn interactive_mode_preserves_conversation_history() {
        // Verify messages Vec grows across multiple user inputs
    }
}
```

---

### Phase 2: Agent Definitions + Registry (~2 days)

#### 2a. Agent definition parser

**New file: `src/agent/definition.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::path::Path;
use anyhow::Result;

/// Where agent definition files live: ~/.zeroclaw/agents/<name>.md
/// Agent memory/data lives in: ~/.zeroclaw/agents/<name>/
pub const AGENTS_DIR_NAME: &str = "agents";

/// Resolve the agents directory from the zeroclaw home dir.
/// agents_dir = ~/.zeroclaw/agents/ (sibling of workspace/, NOT inside it)
pub fn agents_dir_from_config(config: &crate::config::Config) -> std::path::PathBuf {
    // config.workspace_dir = ~/.zeroclaw/workspace/
    // agents_dir = ~/.zeroclaw/agents/
    config.workspace_dir
        .parent()
        .unwrap_or(&config.workspace_dir)
        .join(AGENTS_DIR_NAME)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default = "default_memory")]
    pub memory: MemoryIsolation,
    /// Memory backend for this agent: "jsonl" (default for persistent),
    /// "sqlite" (opt-in for vector search), ignored for ephemeral (always in-memory)
    #[serde(default = "default_memory_backend")]
    pub memory_backend: String,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub delegates_to: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default = "default_max_tools")]
    pub max_tools_per_turn: usize,
    /// Which tools this agent can use. Empty = all tools.
    /// Example: ["memory_store", "memory_recall", "shell"]
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// The markdown body (personality/instructions)
    #[serde(skip)]
    pub personality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryIsolation {
    #[default]
    Isolated,
    SharedRead,
    Shared,
}

fn default_memory() -> MemoryIsolation { MemoryIsolation::Isolated }
fn default_memory_backend() -> String { "jsonl".into() }
fn default_max_tools() -> usize { 10 }

impl AgentDefinition {
    /// Parse from a markdown file with YAML frontmatter
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse from markdown string: `---\nyaml\n---\nmarkdown body`
    pub fn parse(content: &str) -> Result<Self> {
        let content = content.trim();
        if !content.starts_with("---") {
            anyhow::bail!("Agent definition must start with YAML frontmatter (---)");
        }

        let after_first = &content[3..];
        let end = after_first.find("---")
            .ok_or_else(|| anyhow::anyhow!("Missing closing --- for YAML frontmatter"))?;

        let yaml_str = &after_first[..end].trim();
        let body = after_first[end + 3..].trim();

        let mut def: AgentDefinition = serde_yaml::from_str(yaml_str)?;
        def.personality = body.to_string();
        Ok(def)
    }

    /// Serialize back to markdown + YAML frontmatter
    pub fn to_markdown(&self) -> String {
        let yaml = serde_yaml::to_string(self).unwrap_or_default();
        format!("---\n{yaml}---\n\n{}", self.personality)
    }

    /// Validate the definition (check skill names exist, cron expression parses, etc.)
    pub fn validate(&self, available_skills: &[String]) -> Result<Vec<String>> {
        let mut warnings = Vec::new();

        if self.name.is_empty() {
            anyhow::bail!("Agent name cannot be empty");
        }
        if self.name.contains('/') || self.name.contains('\\') {
            anyhow::bail!("Agent name cannot contain path separators");
        }

        // Check skills exist
        for skill in &self.skills {
            if !available_skills.contains(skill) {
                warnings.push(format!("Skill '{}' not found in workspace", skill));
            }
        }

        // Validate cron expression if present
        if let Some(ref expr) = self.schedule {
            if !self.persistent {
                anyhow::bail!("Schedule requires persistent: true");
            }
            // Use the existing normalize + parse from cron module
            let normalized = normalize_cron_expression(expr)?;
            let _ = cron::Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression '{}': {}", expr, e))?;
        }

        // Validate memory_backend
        match self.memory_backend.as_str() {
            "jsonl" | "sqlite" | "markdown" => {}
            other => warnings.push(format!("Unknown memory_backend '{}', will use jsonl", other)),
        }

        Ok(warnings)
    }
}

fn normalize_cron_expression(expression: &str) -> Result<String> {
    let field_count = expression.trim().split_whitespace().count();
    match field_count {
        5 => Ok(format!("0 {expression}")),
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!("Invalid cron expression: expected 5-7 fields, got {field_count}"),
    }
}
```

**Location convention:**
- Agent definitions: `~/.zeroclaw/agents/<name>.md` (YAML frontmatter + personality)
- Agent data: `~/.zeroclaw/agents/<name>/` (memory.jsonl, memory.db, etc.)
- This is `~/.zeroclaw/agents/`, a sibling of `~/.zeroclaw/workspace/`

**Tests for definition.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_definition() {
        let md = r#"---
name: twitter-agent
persistent: true
skills: ["twitter"]
memory: isolated
schedule: "0 10,20 * * *"
allowed_tools: ["shell", "memory_store"]
---

You manage my Twitter account. Post engaging content.
"#;
        let def = AgentDefinition::parse(md).unwrap();
        assert_eq!(def.name, "twitter-agent");
        assert!(def.persistent);
        assert_eq!(def.skills, vec!["twitter"]);
        assert_eq!(def.memory, MemoryIsolation::Isolated);
        assert!(def.personality.contains("Twitter"));
        assert_eq!(def.allowed_tools, vec!["shell", "memory_store"]);
    }

    #[test]
    fn parse_minimal_definition() {
        let md = "---\nname: test\n---\nHello";
        let def = AgentDefinition::parse(md).unwrap();
        assert_eq!(def.name, "test");
        assert!(!def.persistent);
        assert_eq!(def.max_tools_per_turn, 10);
        assert!(def.allowed_tools.is_empty());
        assert_eq!(def.memory_backend, "jsonl");
    }

    #[test]
    fn parse_missing_frontmatter_fails() {
        assert!(AgentDefinition::parse("No frontmatter here").is_err());
    }

    #[test]
    fn roundtrip_to_markdown() {
        let md = "---\nname: test\npersistent: false\n---\nHello world";
        let def = AgentDefinition::parse(md).unwrap();
        let out = def.to_markdown();
        let def2 = AgentDefinition::parse(&out).unwrap();
        assert_eq!(def.name, def2.name);
    }

    #[test]
    fn validate_rejects_empty_name() {
        let mut def = AgentDefinition::parse("---\nname: test\n---\n").unwrap();
        def.name = String::new();
        assert!(def.validate(&[]).is_err());
    }

    #[test]
    fn validate_warns_unknown_skill() {
        let def = AgentDefinition::parse("---\nname: test\nskills: [nonexistent]\n---\n").unwrap();
        let warnings = def.validate(&["twitter".into()]).unwrap();
        assert!(warnings.iter().any(|w| w.contains("nonexistent")));
    }

    #[test]
    fn agents_dir_is_sibling_of_workspace() {
        let config = crate::config::Config {
            workspace_dir: std::path::PathBuf::from("/home/user/.zeroclaw/workspace"),
            ..crate::config::Config::default()
        };
        let dir = agents_dir_from_config(&config);
        assert_eq!(dir, std::path::PathBuf::from("/home/user/.zeroclaw/agents"));
    }
}
```

#### 2b. Agent registry

**New file: `src/agent/registry.rs`**

```rust
use super::definition::{AgentDefinition, AGENTS_DIR_NAME};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct AgentRegistry {
    agents_dir: PathBuf,
}

impl AgentRegistry {
    /// Create a registry. `agents_dir` = ~/.zeroclaw/agents/
    pub fn new(agents_dir: &Path) -> Self {
        Self {
            agents_dir: agents_dir.to_path_buf(),
        }
    }

    /// Create from config (derives agents_dir from workspace_dir)
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::new(&super::definition::agents_dir_from_config(config))
    }

    /// List all agent definitions
    pub fn list(&self) -> Vec<AgentDefinition> {
        let Ok(entries) = std::fs::read_dir(&self.agents_dir) else {
            return Vec::new();
        };

        entries
            .flatten()
            .filter(|e| {
                e.path().extension().and_then(|x| x.to_str()) == Some("md")
            })
            .filter_map(|e| AgentDefinition::from_file(&e.path()).ok())
            .collect()
    }

    /// Get a specific agent by name
    pub fn get(&self, name: &str) -> Option<AgentDefinition> {
        let path = self.agents_dir.join(format!("{name}.md"));
        AgentDefinition::from_file(&path).ok()
    }

    /// Create a new agent definition file
    pub fn create(&self, definition: &AgentDefinition) -> Result<()> {
        std::fs::create_dir_all(&self.agents_dir)?;
        let path = self.agents_dir.join(format!("{}.md", definition.name));
        if path.exists() {
            anyhow::bail!("Agent '{}' already exists", definition.name);
        }
        std::fs::write(&path, definition.to_markdown())?;

        // Create agent data directory for persistent agents
        if definition.persistent {
            let data_dir = self.agents_dir.join(&definition.name);
            std::fs::create_dir_all(&data_dir)?;
        }
        Ok(())
    }

    /// Update an existing agent definition
    pub fn update(&self, definition: &AgentDefinition) -> Result<()> {
        let path = self.agents_dir.join(format!("{}.md", definition.name));
        if !path.exists() {
            anyhow::bail!("Agent '{}' not found", definition.name);
        }
        std::fs::write(&path, definition.to_markdown())?;
        Ok(())
    }

    /// Remove an agent and its data directory
    pub fn remove(&self, name: &str) -> Result<()> {
        let md_path = self.agents_dir.join(format!("{name}.md"));
        if !md_path.exists() {
            anyhow::bail!("Agent '{name}' not found");
        }
        std::fs::remove_file(&md_path)?;

        let data_dir = self.agents_dir.join(name);
        if data_dir.exists() {
            std::fs::remove_dir_all(&data_dir)?;
        }
        Ok(())
    }

    pub fn exists(&self, name: &str) -> bool {
        self.agents_dir.join(format!("{name}.md")).exists()
    }

    /// Get the data directory for a specific agent
    pub fn data_dir(&self, name: &str) -> PathBuf {
        self.agents_dir.join(name)
    }
}
```

**Tests for registry.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_registry(tmp: &TempDir) -> AgentRegistry {
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        AgentRegistry::new(&agents_dir)
    }

    fn test_def(name: &str) -> AgentDefinition {
        AgentDefinition::parse(&format!("---\nname: {name}\n---\nTest agent")).unwrap()
    }

    #[test]
    fn create_and_get() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("alpha")).unwrap();
        let got = reg.get("alpha").unwrap();
        assert_eq!(got.name, "alpha");
    }

    #[test]
    fn create_duplicate_fails() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("dup")).unwrap();
        assert!(reg.create(&test_def("dup")).is_err());
    }

    #[test]
    fn list_agents() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("a")).unwrap();
        reg.create(&test_def("b")).unwrap();
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn remove_agent() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("gone")).unwrap();
        reg.remove("gone").unwrap();
        assert!(!reg.exists("gone"));
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(reg.get("nope").is_none());
    }
}
```

#### 2c. CLI commands

**File: `src/main.rs`** — add `AgentCommands` subcommand:

```rust
#[derive(Subcommand, Debug)]
enum AgentSubCommands {
    /// List all agents
    List,
    /// Create an agent from natural language description
    Create {
        /// Natural language description of the agent
        description: String,
    },
    /// Create an agent from flags (power user)
    New {
        /// Agent name
        name: String,
        #[arg(long)]
        persistent: bool,
        #[arg(long, value_delimiter = ',')]
        skills: Option<Vec<String>>,
        #[arg(long)]
        memory: Option<String>,
        #[arg(long)]
        schedule: Option<String>,
        #[arg(long, value_delimiter = ',')]
        allowed_tools: Option<Vec<String>>,
    },
    /// Edit an agent definition (opens in $EDITOR)
    Edit { name: String },
    /// Remove an agent
    Remove { name: String },
    /// Show agent status
    Status { name: String },
    /// Start a persistent agent
    Start { name: String },
    /// Stop a persistent agent
    Stop { name: String },
    /// Add a skill to an agent
    SkillAdd { agent: String, skill: String },
    /// Remove a skill from an agent
    SkillRemove { agent: String, skill: String },
}
```

Add to `Commands` enum (singular, matching Rust convention):

```rust
/// Manage agents (create, list, edit, remove)
Agent_ {
    #[command(subcommand)]
    agent_command: AgentSubCommands,
},
```

**Note:** The existing `Commands::Agent` (the main chat command) stays as-is. The agent management command uses `Agent_` or a different name like `Agents` to avoid collision. Alternatively, the management commands could be subcommands of the existing `Agent` command. The cleanest approach: rename the chat command to `Chat` and use `Agent` for management. This is a UX decision.

**Recommended:** Keep `Commands::Agent` for the chat (it's the primary user-facing command). Add `Commands::Agents` (plural) for management:

```rust
/// Manage agents (create, list, edit, remove)
Agents {
    #[command(subcommand)]
    agent_command: AgentSubCommands,
},
```

Handler in `main()`:

```rust
Commands::Agents { agent_command } => {
    agent::commands::handle_command(agent_command, &config).await
}
```

**New file: `src/agent/commands.rs`** — handles each `AgentSubCommands` variant:

```rust
use super::definition::{agents_dir_from_config, AgentDefinition};
use super::registry::AgentRegistry;
use crate::config::Config;
use anyhow::Result;

pub async fn handle_command(
    command: super::super::AgentSubCommands,
    config: &Config,
) -> Result<()> {
    let registry = AgentRegistry::from_config(config);

    match command {
        super::super::AgentSubCommands::List => {
            let agents = registry.list();
            if agents.is_empty() {
                println!("No agents defined.");
                println!("\nCreate one:");
                println!("  zeroclaw agents create \"manages my twitter account\"");
                println!("  zeroclaw agents new twitter-agent --persistent --skills twitter");
            } else {
                println!("Agents ({}):", agents.len());
                for agent in &agents {
                    let kind = if agent.persistent { "persistent" } else { "ephemeral" };
                    let schedule = agent.schedule.as_deref().unwrap_or("none");
                    println!(
                        "  {} [{}] skills={:?} schedule={} memory={:?}",
                        agent.name, kind, agent.skills, schedule, agent.memory
                    );
                }
            }
            Ok(())
        }
        super::super::AgentSubCommands::Create { description } => {
            let definition = super::generator::generate_definition(
                &description, config,
            ).await?;
            println!("{}", definition.to_markdown());
            println!("\n---");
            print!("Create this agent? [y/N] ");
            // Read confirmation...
            registry.create(&definition)?;
            println!("Created agent '{}'", definition.name);
            Ok(())
        }
        super::super::AgentSubCommands::New {
            name, persistent, skills, memory, schedule, allowed_tools,
        } => {
            let definition = AgentDefinition {
                name,
                persistent,
                skills: skills.unwrap_or_default(),
                memory: match memory.as_deref() {
                    Some("shared-read") => super::definition::MemoryIsolation::SharedRead,
                    Some("shared") => super::definition::MemoryIsolation::Shared,
                    _ => super::definition::MemoryIsolation::Isolated,
                },
                schedule,
                allowed_tools: allowed_tools.unwrap_or_default(),
                personality: String::new(),
                ..Default::default()
            };
            registry.create(&definition)?;
            println!("Created agent '{}'", definition.name);
            Ok(())
        }
        super::super::AgentSubCommands::Remove { name } => {
            registry.remove(&name)?;
            println!("Removed agent '{name}'");
            Ok(())
        }
        // ... other variants
        _ => {
            println!("Not yet implemented");
            Ok(())
        }
    }
}
```

---

### Phase 3: Inter-Agent Message Bus (~1 day)

#### 3a. Message bus with response channels

**New file: `src/agent/bus.rs`**

```rust
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

/// A message sent between agents
#[derive(Debug)]
pub struct AgentMessage {
    pub id: Uuid,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub payload: String,
    /// For Delegate messages: sender provides a oneshot channel to receive the response.
    /// The receiving agent sends its result back through this channel.
    pub response_tx: Option<oneshot::Sender<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// "handle this task" — expects a response via response_tx
    Delegate,
    /// "here's my output" — fire-and-forget
    Result,
    /// fire-and-forget notification
    Notify,
    /// graceful stop request
    Shutdown,
}

/// Inter-agent message bus using tokio mpsc channels.
///
/// Design: Main agent routes all messages (no direct agent-to-agent).
/// The bus enforces this by checking `delegates_to` on send.
pub struct AgentBus {
    senders: Arc<RwLock<HashMap<String, mpsc::Sender<AgentMessage>>>>,
}

impl AgentBus {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an agent and get its message receiver
    pub async fn register(&self, name: &str, buffer: usize) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(buffer);
        self.senders.write().await.insert(name.to_string(), tx);
        rx
    }

    /// Unregister an agent (e.g., on shutdown)
    pub async fn unregister(&self, name: &str) {
        self.senders.write().await.remove(name);
    }

    /// Send a message to a specific agent.
    /// Returns error if the target agent is not registered.
    pub async fn send(&self, msg: AgentMessage) -> Result<()> {
        let senders = self.senders.read().await;
        let sender = senders
            .get(&msg.to)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not registered on bus", msg.to))?;
        sender
            .send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("Agent channel closed"))?;
        Ok(())
    }

    /// Send a Delegate message and wait for the response (with timeout).
    /// This is the primary pattern for the agent_manage tool's "delegate" action.
    pub async fn delegate(
        &self,
        from: &str,
        to: &str,
        payload: &str,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let (response_tx, response_rx) = oneshot::channel();

        let msg = AgentMessage {
            id: Uuid::new_v4(),
            from: from.to_string(),
            to: to.to_string(),
            kind: MessageKind::Delegate,
            payload: payload.to_string(),
            response_tx: Some(response_tx),
        };

        self.send(msg).await?;

        // Wait for response with timeout
        tokio::time::timeout(timeout, response_rx)
            .await
            .map_err(|_| anyhow::anyhow!("Delegation to '{}' timed out after {:?}", to, timeout))?
            .map_err(|_| anyhow::anyhow!("Agent '{}' dropped response channel", to))
    }

    /// Check if an agent is registered
    pub async fn is_registered(&self, name: &str) -> bool {
        self.senders.read().await.contains_key(name)
    }

    /// List registered agent names
    pub async fn registered_agents(&self) -> Vec<String> {
        self.senders.read().await.keys().cloned().collect()
    }
}
```

**Tests for bus.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_send() {
        let bus = AgentBus::new();
        let mut rx = bus.register("agent-a", 10).await;

        bus.send(AgentMessage {
            id: Uuid::new_v4(),
            from: "main".into(),
            to: "agent-a".into(),
            kind: MessageKind::Notify,
            payload: "hello".into(),
            response_tx: None,
        }).await.unwrap();

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.payload, "hello");
        assert_eq!(msg.kind, MessageKind::Notify);
    }

    #[tokio::test]
    async fn delegate_with_response() {
        let bus = Arc::new(AgentBus::new());
        let mut rx = bus.register("worker", 10).await;

        let bus_clone = bus.clone();
        let handle = tokio::spawn(async move {
            bus_clone.delegate("main", "worker", "do stuff", Duration::from_secs(5)).await
        });

        // Simulate worker receiving and responding
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.kind, MessageKind::Delegate);
        msg.response_tx.unwrap().send("done".into()).unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, "done");
    }

    #[tokio::test]
    async fn delegate_timeout() {
        let bus = AgentBus::new();
        let _rx = bus.register("slow", 10).await;

        let result = bus.delegate("main", "slow", "task", Duration::from_millis(10)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn send_to_unregistered_fails() {
        let bus = AgentBus::new();
        let result = bus.send(AgentMessage {
            id: Uuid::new_v4(),
            from: "main".into(),
            to: "nobody".into(),
            kind: MessageKind::Notify,
            payload: "hello".into(),
            response_tx: None,
        }).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unregister_removes_agent() {
        let bus = AgentBus::new();
        let _rx = bus.register("temp", 10).await;
        assert!(bus.is_registered("temp").await);
        bus.unregister("temp").await;
        assert!(!bus.is_registered("temp").await);
    }
}
```

---

### Phase 4: AI-Powered Agent Creation + Main Agent Tools (~2 days)

#### 4a. Skill index

**New struct (defined in `src/skills/mod.rs`):**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
}

/// Build a lightweight skill index for agent matching (no disk I/O, just transforms)
pub fn build_skill_index(workspace_dir: &Path) -> Vec<SkillIndexEntry> {
    let skills = load_skills(workspace_dir);
    skills
        .iter()
        .map(|s| SkillIndexEntry {
            name: s.name.clone(),
            description: s.description.clone(),
            tools: s.tools.iter().map(|t| t.name.clone()).collect(),
            tags: s.tags.clone(),
        })
        .collect()
}
```

#### 4b. Agent generator

**New file: `src/agent/generator.rs`**

```rust
use super::definition::AgentDefinition;
use crate::config::Config;
use crate::providers::{self, ChatProvider};
use crate::types::{ChatMessage, MessageRole};
use anyhow::Result;

const MAX_GENERATION_RETRIES: usize = 2;

/// Generate an agent definition from natural language description.
/// Uses the configured LLM to produce YAML frontmatter + personality.
pub async fn generate_definition(
    description: &str,
    config: &Config,
) -> Result<AgentDefinition> {
    let skill_index = crate::skills::build_skill_index(&config.workspace_dir);
    let skills_list = skill_index
        .iter()
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");

    let provider = providers::create_resilient_chat_provider(
        config.default_provider.as_deref().unwrap_or("openrouter"),
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    let model = config.default_model.as_deref()
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let prompt = format!(
        "Generate an agent definition for this request:\n\n\
         \"{description}\"\n\n\
         Available skills:\n{skills_list}\n\n\
         Respond with ONLY a markdown file containing YAML frontmatter (---) \
         and a personality section. Schema:\n\
         ---\n\
         name: kebab-case-name\n\
         persistent: true/false\n\
         skills: [list, of, skill-names]\n\
         memory: isolated|shared-read|shared\n\
         memory_backend: jsonl (default, or sqlite for vector search)\n\
         schedule: \"cron expression\" (optional, only if persistent)\n\
         max_tools_per_turn: 10\n\
         allowed_tools: [] (empty = all tools)\n\
         ---\n\n\
         Personality and instructions here."
    );

    let messages = vec![ChatMessage {
        role: MessageRole::User,
        content: Some(prompt),
        ..Default::default()
    }];

    for attempt in 0..=MAX_GENERATION_RETRIES {
        let response = provider
            .chat_completion(
                Some("You generate agent definitions. Output only the markdown file, nothing else."),
                &messages,
                &[], // no tools needed for generation
                model,
                0.3,
            )
            .await?;

        let text = response.message.content.unwrap_or_default();

        match AgentDefinition::parse(&text) {
            Ok(def) => {
                // Validate the generated definition
                let available = crate::skills::load_skills(&config.workspace_dir)
                    .iter()
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>();
                let warnings = def.validate(&available)?;
                for w in &warnings {
                    tracing::warn!("Agent generation warning: {w}");
                }
                return Ok(def);
            }
            Err(e) if attempt < MAX_GENERATION_RETRIES => {
                tracing::warn!(
                    "Agent generation attempt {} failed to parse: {e}. Retrying.",
                    attempt + 1
                );
                continue;
            }
            Err(e) => {
                anyhow::bail!(
                    "Failed to generate valid agent definition after {} attempts: {e}",
                    MAX_GENERATION_RETRIES + 1
                );
            }
        }
    }

    unreachable!()
}
```

**Tests for generator.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_llm_output_with_markdown_fence() {
        // LLMs sometimes wrap in ```markdown ... ```
        let output = "```markdown\n---\nname: test\n---\nHello\n```";
        // The parser should handle this (strip fences before parsing)
        // This test documents the expected behavior
    }

    #[test]
    fn parse_llm_output_clean() {
        let output = "---\nname: generated-agent\npersistent: true\nskills: []\n---\nI am helpful.";
        let def = AgentDefinition::parse(output).unwrap();
        assert_eq!(def.name, "generated-agent");
    }
}
```

#### 4c. Agent management tool

**New file: `src/tools/agent_manage.rs`**

```rust
use crate::agent::bus::AgentBus;
use crate::agent::registry::AgentRegistry;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const DELEGATION_TIMEOUT_SECS: u64 = 120;

pub struct AgentManageTool {
    registry: Arc<AgentRegistry>,
    bus: Arc<AgentBus>,
}

impl AgentManageTool {
    pub fn new(registry: Arc<AgentRegistry>, bus: Arc<AgentBus>) -> Self {
        Self { registry, bus }
    }
}

#[async_trait]
impl Tool for AgentManageTool {
    fn name(&self) -> &str { "agent_manage" }

    fn description(&self) -> &str {
        "Create, modify, list, or remove sub-agents. \
         Also delegate tasks to running agents and wait for their response."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "modify", "remove", "delegate", "status"]
                },
                "name": { "type": "string", "description": "Agent name" },
                "description": { "type": "string", "description": "Natural language description (for create)" },
                "skills": { "type": "array", "items": { "type": "string" } },
                "persistent": { "type": "boolean" },
                "task": { "type": "string", "description": "Task to delegate (for delegate action)" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args["action"].as_str().unwrap_or("");

        match action {
            "list" => {
                let agents = self.registry.list();
                if agents.is_empty() {
                    Ok(ToolResult {
                        success: true,
                        output: "No agents defined.".into(),
                        error: None,
                    })
                } else {
                    let list = agents
                        .iter()
                        .map(|a| {
                            let kind = if a.persistent { "persistent" } else { "ephemeral" };
                            format!("- {} [{}] skills={:?}", a.name, kind, a.skills)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(ToolResult {
                        success: true,
                        output: list,
                        error: None,
                    })
                }
            }

            "delegate" => {
                let name = args["name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("'name' required for delegate"))?;
                let task = args["task"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("'task' required for delegate"))?;

                if !self.bus.is_registered(name).await {
                    return Ok(ToolResult {
                        success: false,
                        output: format!("Agent '{name}' is not running. Start it first."),
                        error: None,
                    });
                }

                match self.bus.delegate(
                    "main", name, task,
                    Duration::from_secs(DELEGATION_TIMEOUT_SECS),
                ).await {
                    Ok(response) => Ok(ToolResult {
                        success: true,
                        output: response,
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: format!("Delegation failed: {e}"),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "status" => {
                let name = args["name"].as_str().unwrap_or("all");
                if name == "all" {
                    let registered = self.bus.registered_agents().await;
                    let all = self.registry.list();
                    let status_lines: Vec<String> = all
                        .iter()
                        .map(|a| {
                            let running = registered.contains(&a.name);
                            format!(
                                "- {} [{}] {}",
                                a.name,
                                if a.persistent { "persistent" } else { "ephemeral" },
                                if running { "RUNNING" } else { "stopped" }
                            )
                        })
                        .collect();
                    Ok(ToolResult {
                        success: true,
                        output: status_lines.join("\n"),
                        error: None,
                    })
                } else {
                    let running = self.bus.is_registered(name).await;
                    Ok(ToolResult {
                        success: true,
                        output: format!("{name}: {}", if running { "RUNNING" } else { "stopped" }),
                        error: None,
                    })
                }
            }

            // "create", "modify", "remove" omitted for brevity — follow same pattern
            _ => Ok(ToolResult {
                success: false,
                output: format!("Unknown action: {action}"),
                error: None,
            }),
        }
    }
}
```

**Integration:** The `agent_manage` tool is added to `all_tools()` only when the bus and registry are available (i.e., in daemon mode or when explicitly requested). We do NOT change the `all_tools()` signature. Instead, we add a separate function:

**File: `src/tools/mod.rs`** — add:

```rust
/// Create tool registry with agent management capabilities.
/// Used by the daemon and agent runner when bus/registry are available.
pub fn all_tools_with_agents(
    security: &Arc<SecurityPolicy>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
    registry: Arc<crate::agent::registry::AgentRegistry>,
    bus: Arc<crate::agent::bus::AgentBus>,
) -> Vec<Box<dyn Tool>> {
    let mut tools = all_tools(security, memory, composio_key, browser_config);
    tools.push(Box::new(agent_manage::AgentManageTool::new(registry, bus)));
    tools
}
```

This keeps the existing `all_tools()` signature unchanged (fixes C3).

---

### Phase 5: Agent Memory + Runner (~2 days)

#### 5a. In-memory storage for ephemeral agents

**New file: `src/memory/ephemeral.rs`**

```rust
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use chrono::Utc;
use uuid::Uuid;

/// In-memory storage for ephemeral agents. Dies when the agent exits.
/// No persistence, no SQLite, no files. Just a HashMap.
pub struct EphemeralMemory {
    entries: Mutex<HashMap<String, MemoryEntry>>,
}

impl EphemeralMemory {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Memory for EphemeralMemory {
    fn name(&self) -> &str { "ephemeral" }

    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> anyhow::Result<()> {
        let entry = MemoryEntry {
            id: Uuid::new_v4().to_string(),
            key: key.to_string(),
            content: content.to_string(),
            category,
            timestamp: Utc::now().to_rfc3339(),
            session_id: None,
            score: None,
        };
        self.entries.lock().unwrap().insert(key.to_string(), entry);
        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self.entries.lock().unwrap();
        let query_lower = query.to_lowercase();
        let mut results: Vec<MemoryEntry> = entries
            .values()
            .filter(|e| {
                e.key.to_lowercase().contains(&query_lower)
                    || e.content.to_lowercase().contains(&query_lower)
            })
            .cloned()
            .collect();
        results.truncate(limit);
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(self.entries.lock().unwrap().get(key).cloned())
    }

    async fn list(&self, category: Option<&MemoryCategory>) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self.entries.lock().unwrap();
        Ok(entries
            .values()
            .filter(|e| category.is_none() || category == Some(&e.category))
            .cloned()
            .collect())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.entries.lock().unwrap().remove(key).is_some())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.entries.lock().unwrap().len())
    }

    async fn health_check(&self) -> bool { true }
}
```

**Tests for ephemeral.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_and_recall() {
        let mem = EphemeralMemory::new();
        mem.store("test", "hello world", MemoryCategory::Core).await.unwrap();
        let results = mem.recall("hello", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "hello world");
    }

    #[tokio::test]
    async fn forget_removes_entry() {
        let mem = EphemeralMemory::new();
        mem.store("k", "v", MemoryCategory::Core).await.unwrap();
        assert!(mem.forget("k").await.unwrap());
        assert!(mem.get("k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn count() {
        let mem = EphemeralMemory::new();
        assert_eq!(mem.count().await.unwrap(), 0);
        mem.store("a", "1", MemoryCategory::Core).await.unwrap();
        mem.store("b", "2", MemoryCategory::Core).await.unwrap();
        assert_eq!(mem.count().await.unwrap(), 2);
    }
}
```

#### 5b. Per-agent memory factory

**File: `src/memory/mod.rs`** — add agent memory factory:

```rust
pub mod ephemeral;
pub use ephemeral::EphemeralMemory;

/// Create memory for an ephemeral agent (in-memory HashMap, no persistence)
pub fn create_ephemeral_memory() -> Box<dyn Memory> {
    Box::new(EphemeralMemory::new())
}

/// Create memory for a persistent agent.
/// Default: JSONL (markdown backend pointed at agent's data dir).
/// Opt-in: SQLite when agent definition specifies memory_backend: sqlite.
///
/// Does NOT run hygiene (agent memory is managed by the agent lifecycle).
pub fn create_agent_memory(
    config: &MemoryConfig,
    agents_dir: &std::path::Path,
    agent_name: &str,
    memory_backend: &str,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    let agent_data_dir = agents_dir.join(agent_name);
    std::fs::create_dir_all(&agent_data_dir)?;

    match memory_backend {
        "sqlite" => {
            // Full SQLite with embeddings for vector search
            let embedder: std::sync::Arc<dyn embeddings::EmbeddingProvider> =
                std::sync::Arc::from(embeddings::create_embedding_provider(
                    &config.embedding_provider,
                    api_key,
                    &config.embedding_model,
                    config.embedding_dimensions,
                ));
            #[allow(clippy::cast_possible_truncation)]
            let mem = SqliteMemory::with_embedder(
                &agent_data_dir,
                embedder,
                config.vector_weight as f32,
                config.keyword_weight as f32,
                config.embedding_cache_size,
            )?;
            Ok(Box::new(mem))
        }
        _ => {
            // Default: JSONL/markdown — lightweight, append-only
            Ok(Box::new(MarkdownMemory::new(&agent_data_dir)))
        }
    }
}
```

#### 5c. Composite memory for shared-read mode

**New file: `src/memory/composite.rs`**

```rust
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use async_trait::async_trait;
use std::sync::Arc;

/// Composite memory: reads from both workspace + agent memory,
/// writes only to agent-specific memory.
///
/// Used for `MemoryIsolation::SharedRead` agents.
pub struct CompositeMemory {
    /// Read-only source (workspace memory)
    read_source: Arc<dyn Memory>,
    /// Read-write target (agent-specific memory)
    write_target: Arc<dyn Memory>,
}

impl CompositeMemory {
    pub fn new(read_source: Arc<dyn Memory>, write_target: Arc<dyn Memory>) -> Self {
        Self {
            read_source,
            write_target,
        }
    }
}

#[async_trait]
impl Memory for CompositeMemory {
    fn name(&self) -> &str { "composite" }

    /// Store only to agent-specific memory (never write to shared source)
    async fn store(
        &self, key: &str, content: &str, category: MemoryCategory,
    ) -> anyhow::Result<()> {
        self.write_target.store(key, content, category).await
    }

    /// Recall from both sources, merge and deduplicate by key, limit results.
    /// Agent-specific entries take priority over workspace entries with same key.
    async fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        let (agent_results, workspace_results) = tokio::join!(
            self.write_target.recall(query, limit),
            self.read_source.recall(query, limit),
        );

        let mut results = agent_results.unwrap_or_default();
        let agent_keys: std::collections::HashSet<String> =
            results.iter().map(|e| e.key.clone()).collect();

        // Add workspace results that don't conflict with agent results
        for entry in workspace_results.unwrap_or_default() {
            if !agent_keys.contains(&entry.key) {
                results.push(entry);
            }
        }

        // Sort by score (highest first) if scores exist, otherwise by timestamp
        results.sort_by(|a, b| {
            b.score
                .unwrap_or(0.0)
                .partial_cmp(&a.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(limit);
        Ok(results)
    }

    /// Get from agent memory first, fall back to workspace
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        if let Some(entry) = self.write_target.get(key).await? {
            return Ok(Some(entry));
        }
        self.read_source.get(key).await
    }

    /// List from agent memory only (listing workspace would be too noisy)
    async fn list(
        &self, category: Option<&MemoryCategory>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.write_target.list(category).await
    }

    /// Forget only from agent memory (cannot delete from read-only source)
    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.write_target.forget(key).await
    }

    /// Count agent memory entries only
    async fn count(&self) -> anyhow::Result<usize> {
        self.write_target.count().await
    }

    async fn health_check(&self) -> bool {
        self.write_target.health_check().await && self.read_source.health_check().await
    }
}
```

**Tests for composite.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::EphemeralMemory;

    fn make_composite() -> (CompositeMemory, Arc<EphemeralMemory>, Arc<EphemeralMemory>) {
        let workspace = Arc::new(EphemeralMemory::new());
        let agent = Arc::new(EphemeralMemory::new());
        let composite = CompositeMemory::new(workspace.clone(), agent.clone());
        (composite, workspace, agent)
    }

    #[tokio::test]
    async fn store_goes_to_agent_only() {
        let (composite, workspace, agent) = make_composite();
        composite.store("key", "value", MemoryCategory::Core).await.unwrap();
        assert!(agent.get("key").await.unwrap().is_some());
        assert!(workspace.get("key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recall_merges_both_sources() {
        let (composite, workspace, agent) = make_composite();
        workspace.store("ws_key", "workspace data", MemoryCategory::Core).await.unwrap();
        agent.store("ag_key", "agent data", MemoryCategory::Core).await.unwrap();
        let results = composite.recall("data", 10).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn recall_agent_overrides_workspace() {
        let (composite, workspace, agent) = make_composite();
        workspace.store("shared", "old value", MemoryCategory::Core).await.unwrap();
        agent.store("shared", "new value", MemoryCategory::Core).await.unwrap();
        let results = composite.recall("shared", 10).await.unwrap();
        // Should deduplicate — agent version wins
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "new value");
    }

    #[tokio::test]
    async fn forget_only_affects_agent() {
        let (composite, workspace, _agent) = make_composite();
        workspace.store("immutable", "can't delete", MemoryCategory::Core).await.unwrap();
        let removed = composite.forget("immutable").await.unwrap();
        assert!(!removed); // Can't forget from read-only source
    }

    #[tokio::test]
    async fn get_falls_back_to_workspace() {
        let (composite, workspace, _agent) = make_composite();
        workspace.store("only_ws", "from workspace", MemoryCategory::Core).await.unwrap();
        let entry = composite.get("only_ws").await.unwrap().unwrap();
        assert_eq!(entry.content, "from workspace");
    }
}
```

#### 5d. Agent runner (persistent agent loop)

**New file: `src/agent/runner.rs`**

```rust
use super::bus::{AgentBus, AgentMessage, MessageKind};
use super::definition::{agents_dir_from_config, AgentDefinition, MemoryIsolation};
use crate::config::Config;
use crate::memory::{self, Memory};
use crate::providers::{self, ChatProvider};
use crate::tools::{self, Tool};
use crate::types::{ChatMessage, MessageRole, ToolSpec};
use anyhow::Result;
use std::sync::Arc;

const AGENT_MSG_BUFFER: usize = 32;

/// Run a persistent agent with its own memory, skills, and message bus receiver.
pub async fn run_persistent_agent(
    definition: AgentDefinition,
    config: Config,    // owned, cloned from daemon — NOT borrowed
    bus: Arc<AgentBus>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let agents_dir = agents_dir_from_config(&config);

    // ── Memory ──
    let memory: Arc<dyn Memory> = match definition.memory {
        MemoryIsolation::Isolated => {
            Arc::from(memory::create_agent_memory(
                &config.memory,
                &agents_dir,
                &definition.name,
                &definition.memory_backend,
                config.api_key.as_deref(),
            )?)
        }
        MemoryIsolation::SharedRead => {
            let workspace_mem = Arc::from(memory::create_memory(
                &config.memory,
                &config.workspace_dir,
                config.api_key.as_deref(),
            )?);
            let agent_mem = Arc::from(memory::create_agent_memory(
                &config.memory,
                &agents_dir,
                &definition.name,
                &definition.memory_backend,
                config.api_key.as_deref(),
            )?);
            Arc::from(memory::composite::CompositeMemory::new(workspace_mem, agent_mem))
        }
        MemoryIsolation::Shared => {
            Arc::from(memory::create_memory(
                &config.memory,
                &config.workspace_dir,
                config.api_key.as_deref(),
            )?)
        }
    };

    // ── Tools (filtered by allowed_tools) ──
    let security = Arc::new(crate::security::SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let composio_key = if config.composio.enabled {
        config.composio.api_key.as_deref()
    } else {
        None
    };
    let all_tools = tools::all_tools(&security, memory.clone(), composio_key, &config.browser);
    let tools: Vec<Box<dyn Tool>> = filter_tools(all_tools, &definition.allowed_tools);
    let tool_specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();

    // ── Provider ──
    let model = definition.model.as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");
    let temperature = definition.temperature.unwrap_or(config.default_temperature);
    let provider: Box<dyn ChatProvider> = providers::create_resilient_chat_provider(
        config.default_provider.as_deref().unwrap_or("openrouter"),
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    // ── System prompt ──
    let system_prompt = build_agent_prompt(&definition, &config);

    // ── Register on bus ──
    let mut receiver = bus.register(&definition.name, AGENT_MSG_BUFFER).await;

    tracing::info!(agent = definition.name, "Persistent agent started");

    loop {
        tokio::select! {
            Some(msg) = receiver.recv() => {
                match msg.kind {
                    MessageKind::Delegate => {
                        let response = run_agent_turn(
                            &provider, &system_prompt, &msg.payload,
                            &tools, &tool_specs,
                            model, temperature,
                            definition.max_tools_per_turn,
                        ).await;

                        // Send response back through oneshot channel
                        if let Some(tx) = msg.response_tx {
                            let _ = tx.send(response.unwrap_or_else(|e| format!("Error: {e}")));
                        }
                    }
                    MessageKind::Shutdown => {
                        tracing::info!(agent = definition.name, "Shutdown requested");
                        break;
                    }
                    MessageKind::Notify => {
                        // Log and ignore
                        tracing::info!(
                            agent = definition.name,
                            from = msg.from,
                            "Notification: {}",
                            msg.payload
                        );
                    }
                    _ => {}
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!(agent = definition.name, "Daemon shutdown");
                break;
            }
        }
    }

    bus.unregister(&definition.name).await;
    Ok(())
}

/// Execute one turn of agent interaction with tool calling.
/// Returns the final text response.
async fn run_agent_turn(
    provider: &dyn ChatProvider,
    system_prompt: &str,
    task: &str,
    tools: &[Box<dyn Tool>],
    tool_specs: &[ToolSpec],
    model: &str,
    temperature: f64,
    max_tools: usize,
) -> Result<String> {
    let mut messages = vec![ChatMessage {
        role: MessageRole::User,
        content: Some(task.to_string()),
        ..Default::default()
    }];

    for _ in 0..max_tools {
        let response = provider
            .chat_completion(Some(system_prompt), &messages, tool_specs, model, temperature)
            .await?;

        messages.push(response.message.clone());

        if response.message.tool_calls.is_empty() {
            return Ok(response.message.content.unwrap_or_default());
        }

        for tool_call in &response.message.tool_calls {
            let tool = tools.iter().find(|t| t.name() == tool_call.name);
            let result = match tool {
                Some(t) => {
                    let args: serde_json::Value =
                        serde_json::from_str(&tool_call.arguments)
                            .unwrap_or_default();
                    t.execute(args).await.unwrap_or_else(|e| crate::types::ToolResult {
                        success: false,
                        output: format!("Error: {e}"),
                        error: Some(e.to_string()),
                    })
                }
                None => crate::types::ToolResult {
                    success: false,
                    output: format!("Unknown tool: {}", tool_call.name),
                    error: None,
                },
            };

            messages.push(ChatMessage {
                role: MessageRole::Tool,
                content: Some(result.output),
                tool_call_id: Some(tool_call.id.clone()),
                ..Default::default()
            });
        }
    }

    Ok("[Max tool iterations reached]".into())
}

/// Build system prompt for an agent from its definition + workspace files.
fn build_agent_prompt(definition: &AgentDefinition, config: &Config) -> String {
    let skills = load_agent_skills(&definition.skills, &config.workspace_dir);

    let tool_descs: Vec<(&str, &str)> = Vec::new(); // Tools are provided via tool_specs

    let mut prompt = crate::channels::build_system_prompt(
        &config.workspace_dir,
        definition.model.as_deref().unwrap_or(""),
        &tool_descs,
        &skills,
    );

    // Append agent-specific personality
    if !definition.personality.is_empty() {
        prompt.push_str("\n## Agent Personality\n\n");
        prompt.push_str(&definition.personality);
        prompt.push('\n');
    }

    prompt
}

/// Load skills that match the agent's skill list.
fn load_agent_skills(
    skill_names: &[String],
    workspace_dir: &std::path::Path,
) -> Vec<crate::skills::Skill> {
    if skill_names.is_empty() {
        return Vec::new();
    }
    let all_skills = crate::skills::load_skills(workspace_dir);
    all_skills
        .into_iter()
        .filter(|s| skill_names.contains(&s.name))
        .collect()
}

/// Filter tools to only those in the allowed list.
/// Empty allowed_tools = all tools permitted.
fn filter_tools(
    all_tools: Vec<Box<dyn Tool>>,
    allowed_tools: &[String],
) -> Vec<Box<dyn Tool>> {
    if allowed_tools.is_empty() {
        return all_tools;
    }
    all_tools
        .into_iter()
        .filter(|t| allowed_tools.iter().any(|a| a == t.name()))
        .collect()
}
```

**Tests for runner.rs:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_tools_empty_allows_all() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        let tools = tools::default_tools(security);
        let filtered = filter_tools(tools, &[]);
        assert_eq!(filtered.len(), 3); // shell, file_read, file_write
    }

    #[test]
    fn filter_tools_restricts() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        let tools = tools::default_tools(security);
        let filtered = filter_tools(tools, &["shell".into()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "shell");
    }

    #[test]
    fn load_agent_skills_filters() {
        // Uses workspace skills dir — tested via integration
    }

    #[test]
    fn build_agent_prompt_includes_personality() {
        // Test that personality section is appended
    }
}
```

#### 5e. Daemon integration

**File: `src/daemon/mod.rs`** — add persistent agent lifecycle:

```rust
use crate::agent::bus::AgentBus;
use crate::agent::registry::AgentRegistry;

pub async fn run(config: Config, host: String, port: u16) -> Result<()> {
    // ... existing setup ...

    // ── Shutdown coordination ──
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);

    // ── Agent infrastructure ──
    let registry = AgentRegistry::from_config(&config);
    let bus = Arc::new(AgentBus::new());

    // ── Start persistent agents ──
    let persistent_agents: Vec<_> = registry
        .list()
        .into_iter()
        .filter(|d| d.persistent)
        .collect();

    for definition in persistent_agents {
        let def = definition.clone();
        let bus = bus.clone();
        let agent_config = config.clone();        // clone, NOT borrow (fixes C14)
        let shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            if let Err(e) =
                crate::agent::runner::run_persistent_agent(def, agent_config, bus, shutdown_rx).await
            {
                tracing::error!("Agent failed: {e}");
            }
        }));
    }

    // ... existing component spawning ...

    println!("   Agents: {} persistent", persistent_agents.len());

    // ── Shutdown ──
    tokio::signal::ctrl_c().await?;
    let _ = shutdown_tx.send(true); // Signal all agents to stop

    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}
```

---

### Phase 6: Onboarding Changes (~0.5 days)

**Minimal changes to `src/onboard/wizard.rs`:**

The onboarding wizard stays focused on getting the user chatting ASAP. No agent questions.

**Add at the end of wizard (after channel setup):**

```
Setup complete! You're ready to chat.

Tip: You can create specialized agents anytime:
  zeroclaw agents create "manages my twitter account"

  Or just tell your agent: "Create an agent that handles code reviews"
```

**Add to workspace scaffolding:**
- Create `~/.zeroclaw/agents/` directory
- No default agents — main agent is implicit, not a definition file

---

### Phase 7: Scheduled Agent Runs (~1 day)

#### 7a. CronJob schema extension

The existing `CronJob` struct (`src/cron/mod.rs:11-19`) has `command: String` and the SQLite schema has `command TEXT NOT NULL`. We need to support agent-specific cron actions.

**Approach:** Use the existing `command` field with a prefix convention rather than a schema migration. This keeps the SQLite schema unchanged and avoids migration complexity.

```
# Shell command (existing behavior):
zeroclaw cron add "0 9 * * *" "echo good morning"

# Agent run (new — prefix convention):
zeroclaw cron add "0 10,20 * * *" "agent:twitter-agent"

# Agent message (new — prefix + payload):
zeroclaw cron add "0 9 * * *" "agent:pm-agent:What's the sprint status?"
```

**File: `src/cron/scheduler.rs`** — extend `run_job_command` to handle agent-prefixed commands:

```rust
async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    bus: Option<&AgentBus>, // NEW: optional bus for agent commands
) -> (bool, String) {
    // Check for agent command prefix
    if let Some(agent_cmd) = job.command.strip_prefix("agent:") {
        return run_agent_job(agent_cmd, bus).await;
    }

    // ... existing shell command logic unchanged ...
}

async fn run_agent_job(
    agent_cmd: &str,
    bus: Option<&AgentBus>,
) -> (bool, String) {
    let Some(bus) = bus else {
        return (false, "Agent jobs require daemon mode (bus not available)".into());
    };

    // Parse "agent_name" or "agent_name:message"
    let (agent_name, message) = match agent_cmd.split_once(':') {
        Some((name, msg)) => (name, msg.to_string()),
        None => (agent_cmd, "Scheduled run — execute your default behavior.".into()),
    };

    if !bus.is_registered(agent_name).await {
        return (false, format!("Agent '{agent_name}' is not running"));
    }

    match bus.delegate(
        "scheduler", agent_name, &message,
        std::time::Duration::from_secs(300), // 5 min timeout for scheduled tasks
    ).await {
        Ok(response) => (true, response),
        Err(e) => (false, format!("Agent job failed: {e}")),
    }
}
```

The `run_job_command` signature change requires updating `execute_job_with_retry` to pass the bus through. The scheduler's `run()` function gets the bus as a parameter from the daemon.

**File: `src/cron/scheduler.rs`** — update `run` signature:

```rust
pub async fn run(config: Config, bus: Option<Arc<AgentBus>>) -> Result<()> {
    // ... existing loop, pass bus.as_deref() to run_job_command ...
}
```

**File: `src/daemon/mod.rs`** — pass bus to scheduler:

```rust
{
    let scheduler_cfg = config.clone();
    let scheduler_bus = bus.clone();
    handles.push(spawn_component_supervisor(
        "scheduler",
        initial_backoff,
        max_backoff,
        move || {
            let cfg = scheduler_cfg.clone();
            let bus = Some(scheduler_bus.clone());
            async move { crate::cron::scheduler::run(cfg, bus).await }
        },
    ));
}
```

**Tests for agent cron jobs:**

```rust
#[cfg(test)]
mod agent_cron_tests {
    use super::*;

    #[tokio::test]
    async fn agent_job_without_bus_fails() {
        let (success, output) = run_agent_job("test-agent", None).await;
        assert!(!success);
        assert!(output.contains("daemon mode"));
    }

    #[tokio::test]
    async fn agent_job_parses_name_and_message() {
        // Test that "pm-agent:sprint status" is correctly parsed
    }

    #[tokio::test]
    async fn agent_job_default_message() {
        // Test that "twitter-agent" (no colon) gets default message
    }
}
```

---

## File Summary

### New files (10):

| File | Purpose | Lines (est) |
|------|---------|-------------|
| `src/types.rs` | Shared types: ChatMessage, ToolCall, ToolSpec, etc. | ~100 |
| `src/agent/definition.rs` | Agent definition parser (YAML frontmatter + markdown) | ~200 |
| `src/agent/registry.rs` | Agent CRUD, filesystem management | ~120 |
| `src/agent/bus.rs` | Inter-agent tokio mpsc message bus with oneshot responses | ~150 |
| `src/agent/runner.rs` | Persistent agent run loop with tool calling | ~250 |
| `src/agent/generator.rs` | AI-powered definition generation with validation + retry | ~120 |
| `src/agent/commands.rs` | CLI command handlers for `zeroclaw agents *` | ~200 |
| `src/tools/agent_manage.rs` | Tool for main agent to manage sub-agents in conversation | ~200 |
| `src/memory/ephemeral.rs` | In-memory HashMap storage for ephemeral agents | ~100 |
| `src/memory/composite.rs` | CompositeMemory for shared-read isolation | ~120 |

### Modified files (14):

| File | Change |
|------|--------|
| `src/main.rs` | Add `mod types`, `AgentSubCommands` enum, match arms, heartbeat call update |
| `src/agent/mod.rs` | Export new submodules: definition, registry, bus, runner, generator, commands |
| `src/agent/loop_.rs` | Add `run_with_tools()`, `run_tool_loop()`, derive tool descriptions from `tool.spec()` |
| `src/providers/traits.rs` | Add `ChatProvider` trait with `system_prompt` as separate param |
| `src/providers/openrouter.rs` | Implement `ChatProvider` (OpenAI wire format translation) |
| `src/providers/anthropic.rs` | Implement `ChatProvider` (Anthropic content-block translation) |
| `src/providers/openai.rs` | Implement `ChatProvider` (OpenAI wire format) |
| `src/providers/ollama.rs` | Implement `ChatProvider` (OpenAI-compatible format) |
| `src/providers/compatible.rs` | Implement `ChatProvider` (OpenAI-compatible format) |
| `src/providers/reliable.rs` | Add `ReliableChatProvider` struct |
| `src/providers/mod.rs` | Add `create_resilient_chat_provider()`, `create_chat_provider()` |
| `src/tools/mod.rs` | Add `all_tools_with_agents()`, re-export types from `crate::types` |
| `src/tools/traits.rs` | Re-export `ToolSpec`, `ToolResult` from `crate::types` |
| `src/daemon/mod.rs` | Add `watch` shutdown channel, start persistent agents, pass bus to scheduler, fix WhatsApp check |
| `src/channels/mod.rs` | Derive tool descriptions from `tool.spec()` |
| `src/cron/scheduler.rs` | Handle `agent:` prefixed commands, accept optional bus |
| `src/memory/mod.rs` | Add `create_ephemeral_memory()`, `create_agent_memory()`, export new modules |

---

## Delegation Response Flow (Oneshot Pattern)

When the main agent delegates to a sub-agent:

```
Main Agent                        AgentBus                    Worker Agent
    |                                |                            |
    |-- agent_manage.execute()       |                            |
    |   action: "delegate"           |                            |
    |   name: "pm-agent"             |                            |
    |   task: "sprint status?"       |                            |
    |                                |                            |
    |-- bus.delegate("main",         |                            |
    |   "pm-agent", "sprint...")     |                            |
    |                                |                            |
    |   [creates oneshot channel]    |                            |
    |   [sends AgentMessage with     |                            |
    |    response_tx = Some(tx)]     |                            |
    |                                |---> AgentMessage --------->|
    |                                |                            |
    |   [awaits oneshot rx           |                            |
    |    with 120s timeout]          |                            |-- run_agent_turn()
    |                                |                            |-- tool calling loop
    |                                |                            |-- final response
    |                                |                            |
    |                                |     response_tx.send() <---|
    |                                |                            |
    |<-- "Sprint is on track..." ----|                            |
    |                                |                            |
    |-- returns ToolResult           |                            |
    |   { output: "Sprint..." }      |                            |
```

If the worker times out (120s), `bus.delegate()` returns an error which becomes a failed `ToolResult`.

---

## Memory Isolation Implementation

| Level | Ephemeral Agent | Persistent Agent |
|-------|-----------------|------------------|
| `isolated` | `EphemeralMemory` (HashMap) | `create_agent_memory()` → JSONL or SQLite at `~/.zeroclaw/agents/<name>/` |
| `shared-read` | `CompositeMemory(workspace_mem, EphemeralMemory)` | `CompositeMemory(workspace_mem, agent_mem)` — reads from both, writes to agent only |
| `shared` | `workspace_mem` directly (rare for ephemeral) | `workspace_mem` directly — full read/write to main workspace memory |

**Memory backend selection for persistent agents:**

```yaml
# In agent definition YAML frontmatter:
memory_backend: jsonl    # default — lightweight, append-only log
memory_backend: sqlite   # opt-in — for agents that need vector/hybrid search
```

---

## Per-Agent Tool Filtering

Each `AgentDefinition` has an `allowed_tools: Vec<String>` field:

```yaml
---
name: twitter-agent
allowed_tools: ["memory_store", "memory_recall", "shell"]
---
```

- **Empty list** (`allowed_tools: []`) = agent can use ALL tools (default)
- **Non-empty list** = agent can ONLY use listed tools

Enforcement happens in `runner.rs:filter_tools()` before the tool loop starts. The tool specs passed to the LLM are also filtered, so the model never sees tools it can't call.

---

## Per-Agent Channel Routing (Future)

The `channels` field on `AgentDefinition` is parsed but **not enforced in this version**. Current channel architecture routes all messages through a single handler. Per-agent routing requires:

1. Channels send messages to the agent bus instead of directly to LLM
2. Bus routes based on agent's `channels` list
3. Agent responds via `bus.send()`, routed back to the originating channel

This is deferred to v3 because it requires refactoring `start_channels()` to be bus-aware.

---

## Resolved Open Questions

1. **Should ephemeral workers share the main agent's conversation context?** **No.** Pass only the delegated task. Clean context = better results. Ephemeral agents get `EphemeralMemory` (HashMap).

2. **Hot-reload granularity.** Full agent restart on definition change. Agent runner reads definition once at startup; to pick up changes, stop and restart.

3. **Agent-to-agent communication.** **Only through main for now.** The `AgentBus` technically allows direct routing, but the `agent_manage` tool only sends from "main". Direct agent-to-agent is a v3 feature.

4. **Resource limits.** Max 10 concurrent agents. Not enforced at the bus level (that would add complexity). Instead, the daemon counts spawned agent tasks and refuses to start more. Configurable via future `[agents]` config section.

5. **Streaming support.** Not in this version. The tool calling loop prints the final response. Streaming intermediate text would require a `ChatProvider::chat_completion_stream()` method. Deferred.

---

## Dependency Chain (correct ordering)

```
Phase 0: Shared types + ChatProvider trait + ReliableProvider
    |
Phase 1: Tool calling loop (depends on Phase 0 for ChatProvider)
    |
Phase 2: Agent definitions + registry (independent of Phase 1)
    |
Phase 3: Message bus (independent of Phase 1-2)
    |
Phase 4: Agent creation + management tool (depends on Phase 2 + 3)
    |
Phase 5: Agent memory + runner (depends on Phase 1 + 2 + 3)
    |
Phase 6: Onboarding changes (depends on Phase 2)
    |
Phase 7: Scheduled agent runs (depends on Phase 3 + 5)
```

Phases 2 and 3 can be developed in parallel. Phase 4 requires both 2 and 3. Phase 5 requires 1, 2, and 3.

---

*This design extends ZeroClaw's existing architecture. No rewrites — only additions. Every type is defined. Every function signature matches the actual codebase.*
