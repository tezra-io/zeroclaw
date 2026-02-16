use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::observability::{self, Observer, ObserverEvent};
use crate::providers::{self, ChatProvider, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools;
use crate::tools::traits::Tool;
use crate::types::{ChatMessage, MessageRole, ToolResult, ToolSpec};
use anyhow::Result;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

/// Build context preamble by searching memory for relevant entries
async fn build_context(mem: &dyn Memory, user_msg: &str) -> String {
    let mut context = String::new();

    // Pull relevant memories for this message
    if let Ok(entries) = mem.recall(user_msg, 5).await {
        if !entries.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &entries {
                let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
            }
            context.push('\n');
        }
    }

    context
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let _runtime = runtime::create_runtime(&config.runtime)?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Tools (including memory tools) ────────────────────────────
    let composio_key = if config.composio.enabled {
        config.composio.api_key.as_deref()
    } else {
        None
    };
    let registered_tools = tools::all_tools(&security, mem.clone(), composio_key, &config.browser);

    // ── Resolve provider ─────────────────────────────────────────
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("anthropic");

    let model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider: Box<dyn Provider> = providers::create_resilient_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Build system prompt from workspace MD files (OpenClaw framework) ──
    let skills = crate::skills::load_skills(&config.workspace_dir);
    let tool_desc_owned: Vec<(String, String)> = registered_tools
        .iter()
        .map(|t| {
            let spec = t.spec();
            (spec.name, spec.description)
        })
        .collect();
    let tool_descs: Vec<(&str, &str)> = tool_desc_owned
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_str()))
        .collect();
    let system_prompt = crate::channels::build_system_prompt(
        &config.workspace_dir,
        model_name,
        &tool_descs,
        &skills,
    );

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    if let Some(msg) = message {
        // Auto-save user message to memory
        if config.memory.auto_save {
            let _ = mem
                .store("user_msg", &msg, MemoryCategory::Conversation)
                .await;
        }

        // Inject memory context into user message
        let context = build_context(mem.as_ref(), &msg).await;
        let enriched = if context.is_empty() {
            msg.clone()
        } else {
            format!("{context}{msg}")
        };

        let response = provider
            .chat_with_system(Some(&system_prompt), &enriched, model_name, temperature)
            .await?;
        println!("{response}");

        // Auto-save assistant response to daily log
        if config.memory.auto_save {
            let summary = if response.len() > 100 {
                format!("{}...", &response[..100])
            } else {
                response.clone()
            };
            let _ = mem
                .store("assistant_resp", &summary, MemoryCategory::Daily)
                .await;
        }
    } else {
        println!("🦀 ZeroClaw Interactive Mode");
        println!("Type /quit to exit.\n");

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cli = crate::channels::CliChannel::new();

        // Spawn listener
        let listen_handle = tokio::spawn(async move {
            let _ = crate::channels::Channel::listen(&cli, tx).await;
        });

        while let Some(msg) = rx.recv().await {
            // Auto-save conversation turns
            if config.memory.auto_save {
                let _ = mem
                    .store("user_msg", &msg.content, MemoryCategory::Conversation)
                    .await;
            }

            // Inject memory context into user message
            let context = build_context(mem.as_ref(), &msg.content).await;
            let enriched = if context.is_empty() {
                msg.content.clone()
            } else {
                format!("{context}{}", msg.content)
            };

            let response = provider
                .chat_with_system(Some(&system_prompt), &enriched, model_name, temperature)
                .await?;
            println!("\n{response}\n");

            if config.memory.auto_save {
                let summary = if response.len() > 100 {
                    format!("{}...", &response[..100])
                } else {
                    response.clone()
                };
                let _ = mem
                    .store("assistant_resp", &summary, MemoryCategory::Daily)
                    .await;
            }
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

/// Run agent with tool calling support (multi-turn).
///
/// This supplements the existing `run()` function.  Instead of a single
/// LLM round-trip it enters a loop: call `chat_completion` → if the
/// response contains tool calls, execute each tool, feed results back,
/// and repeat until the LLM responds with plain text or the iteration
/// cap is reached.
#[allow(clippy::too_many_lines)]
pub async fn run_with_tools(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let _runtime = runtime::create_runtime(&config.runtime)?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    // ── Memory ────────────────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Tools ─────────────────────────────────────────────────────
    let composio_key = if config.composio.enabled {
        config.composio.api_key.as_deref()
    } else {
        None
    };
    let registered_tools = tools::all_tools(&security, mem.clone(), composio_key, &config.browser);
    let tool_specs: Vec<ToolSpec> = registered_tools.iter().map(|t| t.spec()).collect();

    // ── Resolve provider (ChatProvider, not legacy Provider) ─────
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("anthropic");

    let model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider: Box<dyn ChatProvider> = providers::create_resilient_chat_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Build system prompt ──────────────────────────────────────
    let skills = crate::skills::load_skills(&config.workspace_dir);
    let tool_desc_owned: Vec<(String, String)> = registered_tools
        .iter()
        .map(|t| {
            let spec = t.spec();
            (spec.name, spec.description)
        })
        .collect();
    let tool_descs: Vec<(&str, &str)> = tool_desc_owned
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_str()))
        .collect();
    let system_prompt = crate::channels::build_system_prompt(
        &config.workspace_dir,
        model_name,
        &tool_descs,
        &skills,
    );

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    if let Some(msg) = message {
        let mut messages: Vec<ChatMessage> = vec![ChatMessage {
            role: MessageRole::User,
            content: Some(msg),
            ..Default::default()
        }];

        run_tool_loop(
            provider.as_ref(),
            &system_prompt,
            &mut messages,
            &registered_tools,
            &tool_specs,
            model_name,
            temperature,
            20,
        )
        .await?;
    } else {
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
                provider.as_ref(),
                &system_prompt,
                &mut messages,
                &registered_tools,
                &tool_specs,
                model_name,
                temperature,
                20,
            )
            .await?;
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
#[allow(clippy::too_many_arguments)]
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
    for iteration in 0..max_iterations {
        let response = provider
            .chat_completion(
                Some(system_prompt),
                messages,
                tool_specs,
                model,
                temperature,
            )
            .await?;

        messages.push(response.message.clone());

        if response.message.tool_calls.is_empty() {
            // No tool calls — print final response
            if let Some(content) = &response.message.content {
                println!("{content}");
            }
            return Ok(());
        }

        tracing::debug!(
            iteration,
            num_tool_calls = response.message.tool_calls.len(),
            "Executing tool calls"
        );

        // Execute each tool call
        for tool_call in &response.message.tool_calls {
            let tool = tools.iter().find(|t| t.name() == tool_call.name);
            let result = match tool {
                Some(t) => {
                    let args: serde_json::Value = serde_json::from_str(&tool_call.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatResponse, ToolCall};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock provider for testing the tool loop.
    struct MockLoopProvider {
        responses: Vec<ChatResponse>,
        call_count: AtomicUsize,
    }

    #[async_trait]
    impl ChatProvider for MockLoopProvider {
        async fn chat_completion(
            &self,
            _system_prompt: Option<&str>,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            self.responses
                .get(idx)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No more mock responses"))
        }
    }

    /// Mock tool for testing.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes the input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)");
            Ok(ToolResult {
                success: true,
                output: format!("echo: {text}"),
                error: None,
            })
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: Some(text.into()),
                ..Default::default()
            },
            usage: None,
        }
    }

    fn tool_call_response(tool_name: &str, call_id: &str, args: &str) -> ChatResponse {
        ChatResponse {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: call_id.into(),
                    name: tool_name.into(),
                    arguments: args.into(),
                }],
                ..Default::default()
            },
            usage: None,
        }
    }

    #[tokio::test]
    async fn tool_loop_exits_when_no_tool_calls() {
        let provider = MockLoopProvider {
            responses: vec![text_response("Hello!")],
            call_count: AtomicUsize::new(0),
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();
        let mut messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("hi".into()),
            ..Default::default()
        }];

        run_tool_loop(
            &provider,
            "sys",
            &mut messages,
            &tools,
            &specs,
            "m",
            0.0,
            10,
        )
        .await
        .unwrap();

        // Should have user message + assistant response
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.as_deref(), Some("Hello!"));
    }

    #[tokio::test]
    async fn tool_loop_executes_tool_and_feeds_back() {
        let provider = MockLoopProvider {
            responses: vec![
                tool_call_response("echo", "call-1", r#"{"text":"ping"}"#),
                text_response("Got it: echo ping"),
            ],
            call_count: AtomicUsize::new(0),
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();
        let mut messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("echo ping".into()),
            ..Default::default()
        }];

        run_tool_loop(
            &provider,
            "sys",
            &mut messages,
            &tools,
            &specs,
            "m",
            0.0,
            10,
        )
        .await
        .unwrap();

        // user + assistant(tool_call) + tool_result + assistant(final)
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].role, MessageRole::Tool);
        assert_eq!(messages[2].content.as_deref(), Some("echo: ping"));
        assert_eq!(messages[3].content.as_deref(), Some("Got it: echo ping"));
    }

    #[tokio::test]
    async fn tool_loop_handles_unknown_tool() {
        let provider = MockLoopProvider {
            responses: vec![
                tool_call_response("nonexistent", "call-1", "{}"),
                text_response("ok"),
            ],
            call_count: AtomicUsize::new(0),
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();
        let mut messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("test".into()),
            ..Default::default()
        }];

        run_tool_loop(
            &provider,
            "sys",
            &mut messages,
            &tools,
            &specs,
            "m",
            0.0,
            10,
        )
        .await
        .unwrap();

        // user + assistant(tool_call) + tool_result(error) + assistant(final)
        assert_eq!(messages.len(), 4);
        assert!(messages[2]
            .content
            .as_deref()
            .unwrap()
            .contains("Unknown tool"));
    }

    #[tokio::test]
    async fn tool_loop_respects_max_iterations() {
        // Provider always returns a tool call — loop should cap at max_iterations
        let provider = MockLoopProvider {
            responses: vec![
                tool_call_response("echo", "c1", r#"{"text":"1"}"#),
                tool_call_response("echo", "c2", r#"{"text":"2"}"#),
                tool_call_response("echo", "c3", r#"{"text":"3"}"#),
            ],
            call_count: AtomicUsize::new(0),
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();
        let mut messages = vec![ChatMessage {
            role: MessageRole::User,
            content: Some("loop forever".into()),
            ..Default::default()
        }];

        run_tool_loop(&provider, "sys", &mut messages, &tools, &specs, "m", 0.0, 2)
            .await
            .unwrap();

        // user + (assistant+tool) * 2 = 1 + 4 = 5
        assert_eq!(messages.len(), 5);
    }
}
