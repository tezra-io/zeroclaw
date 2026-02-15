use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::observability::{self, Observer, ObserverEvent};
use crate::providers::{self, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools;
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
        .unwrap_or("openrouter");

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
