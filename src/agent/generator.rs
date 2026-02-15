use super::definition::AgentDefinition;
use crate::config::Config;
use crate::providers;
use crate::types::{ChatMessage, MessageRole};
use anyhow::Result;

const MAX_GENERATION_RETRIES: usize = 2;

/// Generate an agent definition from natural language description.
/// Uses the configured LLM to produce YAML frontmatter + personality.
pub async fn generate_definition(description: &str, config: &Config) -> Result<AgentDefinition> {
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

    let model = config
        .default_model
        .as_deref()
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
                Some(
                    "You generate agent definitions. Output only the markdown file, nothing else.",
                ),
                &messages,
                &[], // no tools needed for generation
                model,
                0.3,
            )
            .await?;

        let text = response.message.content.unwrap_or_default();
        let cleaned = strip_markdown_fences(&text);

        match AgentDefinition::parse(&cleaned) {
            Ok(def) => {
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

/// Strip markdown code fences that LLMs sometimes wrap around output.
/// Handles ```markdown ... ```, ```yaml ... ```, or plain ``` ... ```
fn strip_markdown_fences(text: &str) -> String {
    let trimmed = text.trim();

    // Check if wrapped in code fences
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }

    // Find end of opening fence line
    let after_open = if let Some(newline_pos) = trimmed.find('\n') {
        &trimmed[newline_pos + 1..]
    } else {
        return trimmed.to_string();
    };

    // Strip closing fence
    if let Some(stripped) = after_open.strip_suffix("```") {
        stripped.trim().to_string()
    } else {
        after_open.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fences_clean_input() {
        let input = "---\nname: test\n---\nHello";
        assert_eq!(strip_markdown_fences(input), input);
    }

    #[test]
    fn strip_fences_markdown() {
        let input = "```markdown\n---\nname: test\n---\nHello\n```";
        assert_eq!(strip_markdown_fences(input), "---\nname: test\n---\nHello");
    }

    #[test]
    fn strip_fences_yaml() {
        let input = "```yaml\n---\nname: test\n---\nHello\n```";
        assert_eq!(strip_markdown_fences(input), "---\nname: test\n---\nHello");
    }

    #[test]
    fn strip_fences_plain() {
        let input = "```\n---\nname: test\n---\nHello\n```";
        assert_eq!(strip_markdown_fences(input), "---\nname: test\n---\nHello");
    }

    #[test]
    fn strip_fences_no_closing() {
        let input = "```markdown\n---\nname: test\n---\nHello";
        assert_eq!(strip_markdown_fences(input), "---\nname: test\n---\nHello");
    }

    #[test]
    fn strip_fences_with_whitespace() {
        let input = "  ```markdown\n---\nname: test\n---\nHello\n```  ";
        assert_eq!(strip_markdown_fences(input), "---\nname: test\n---\nHello");
    }

    #[test]
    fn parse_llm_output_clean() {
        let output = "---\nname: generated-agent\npersistent: true\nskills: []\n---\nI am helpful.";
        let def = AgentDefinition::parse(output).unwrap();
        assert_eq!(def.name, "generated-agent");
        assert!(def.persistent);
    }

    #[test]
    fn parse_llm_output_with_markdown_fence() {
        let output = "```markdown\n---\nname: fenced-agent\n---\nHello\n```";
        let cleaned = strip_markdown_fences(output);
        let def = AgentDefinition::parse(&cleaned).unwrap();
        assert_eq!(def.name, "fenced-agent");
        assert_eq!(def.personality, "Hello");
    }
}
