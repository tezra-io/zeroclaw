use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Where agent definition files live: `~/.zeroclaw/agents/<name>.md`
/// Agent data lives in: `~/.zeroclaw/agents/<name>/`
pub const AGENTS_DIR_NAME: &str = "agents";

/// Resolve the agents directory from the zeroclaw home dir.
/// `agents_dir` = `~/.zeroclaw/agents/` (sibling of workspace/, NOT inside it)
pub fn agents_dir_from_config(config: &crate::config::Config) -> std::path::PathBuf {
    // config.workspace_dir = ~/.zeroclaw/workspace/
    // agents_dir = ~/.zeroclaw/agents/
    config
        .workspace_dir
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
    #[serde(default)]
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

fn default_memory_backend() -> String {
    "jsonl".into()
}

fn default_max_tools() -> usize {
    10
}

impl Default for AgentDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            persistent: false,
            skills: Vec::new(),
            memory: MemoryIsolation::default(),
            memory_backend: default_memory_backend(),
            schedule: None,
            channels: Vec::new(),
            delegates_to: Vec::new(),
            model: None,
            temperature: None,
            max_tools_per_turn: default_max_tools(),
            allowed_tools: Vec::new(),
            personality: String::new(),
        }
    }
}

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
        let end = after_first
            .find("---")
            .ok_or_else(|| anyhow::anyhow!("Missing closing --- for YAML frontmatter"))?;

        let yaml_str = after_first[..end].trim();
        let body = after_first[end + 3..].trim();

        let mut def: Self = serde_yaml::from_str(yaml_str)?;
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
                warnings.push(format!("Skill '{skill}' not found in workspace"));
            }
        }

        // Validate cron expression if present
        if let Some(ref expr) = self.schedule {
            if !self.persistent {
                anyhow::bail!("Schedule requires persistent: true");
            }
            let normalized = normalize_cron_expression(expr)?;
            let _ = cron::Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression '{expr}': {e}"))?;
        }

        // Validate memory_backend
        match self.memory_backend.as_str() {
            "jsonl" | "sqlite" | "markdown" => {}
            other => warnings.push(format!("Unknown memory_backend '{other}', will use jsonl")),
        }

        Ok(warnings)
    }
}

fn normalize_cron_expression(expression: &str) -> Result<String> {
    let field_count = expression.split_whitespace().count();
    match field_count {
        5 => Ok(format!("0 {expression}")),
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!("Invalid cron expression: expected 5-7 fields, got {field_count}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_definition() {
        let md = r#"---
name: twitter-agent
persistent: true
skills:
  - twitter
memory: isolated
schedule: "0 10,20 * * *"
allowed_tools:
  - shell
  - memory_store
---

You manage my Twitter account. Post engaging content.
"#;
        let def = AgentDefinition::parse(md).unwrap();
        assert_eq!(def.name, "twitter-agent");
        assert!(def.persistent);
        assert_eq!(def.skills, vec!["twitter"]);
        assert_eq!(def.memory, MemoryIsolation::Isolated);
        assert!(def.personality.contains("Twitter"));
        assert_eq!(
            def.allowed_tools,
            vec!["shell".to_string(), "memory_store".to_string()]
        );
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
        assert_eq!(def.personality, "Hello");
    }

    #[test]
    fn parse_missing_frontmatter_fails() {
        assert!(AgentDefinition::parse("No frontmatter here").is_err());
    }

    #[test]
    fn parse_missing_closing_frontmatter_fails() {
        assert!(AgentDefinition::parse("---\nname: test\nno closing").is_err());
    }

    #[test]
    fn parse_invalid_yaml_fails() {
        assert!(AgentDefinition::parse("---\n{{{invalid\n---\nBody").is_err());
    }

    #[test]
    fn roundtrip_to_markdown() {
        let md = "---\nname: test\npersistent: false\n---\nHello world";
        let def = AgentDefinition::parse(md).unwrap();
        let out = def.to_markdown();
        let def2 = AgentDefinition::parse(&out).unwrap();
        assert_eq!(def.name, def2.name);
        assert_eq!(def.persistent, def2.persistent);
    }

    #[test]
    fn validate_rejects_empty_name() {
        let mut def = AgentDefinition::parse("---\nname: test\n---\n").unwrap();
        def.name = String::new();
        assert!(def.validate(&[]).is_err());
    }

    #[test]
    fn validate_rejects_path_separators_in_name() {
        let mut def = AgentDefinition::parse("---\nname: test\n---\n").unwrap();
        def.name = "foo/bar".to_string();
        assert!(def.validate(&[]).is_err());
    }

    #[test]
    fn validate_warns_unknown_skill() {
        let def =
            AgentDefinition::parse("---\nname: test\nskills:\n  - nonexistent\n---\n").unwrap();
        let warnings = def.validate(&["twitter".into()]).unwrap();
        assert!(warnings.iter().any(|w| w.contains("nonexistent")));
    }

    #[test]
    fn validate_schedule_requires_persistent() {
        let md = "---\nname: test\npersistent: false\nschedule: \"* * * * *\"\n---\n";
        let def = AgentDefinition::parse(md).unwrap();
        assert!(def.validate(&[]).is_err());
    }

    #[test]
    fn validate_warns_unknown_memory_backend() {
        let md = "---\nname: test\nmemory_backend: nosql\n---\n";
        let def = AgentDefinition::parse(md).unwrap();
        let warnings = def.validate(&[]).unwrap();
        assert!(warnings.iter().any(|w| w.contains("nosql")));
    }

    #[test]
    fn validate_accepts_valid_definition() {
        let md = "---\nname: test\nmemory_backend: sqlite\n---\n";
        let def = AgentDefinition::parse(md).unwrap();
        let warnings = def.validate(&[]).unwrap();
        assert!(warnings.is_empty());
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

    #[test]
    fn default_definition() {
        let def = AgentDefinition::default();
        assert!(def.name.is_empty());
        assert!(!def.persistent);
        assert_eq!(def.memory, MemoryIsolation::Isolated);
        assert_eq!(def.max_tools_per_turn, 10);
        assert!(def.allowed_tools.is_empty());
        assert!(def.delegates_to.is_empty());
        assert!(def.channels.is_empty());
    }

    #[test]
    fn memory_isolation_serde() {
        assert_eq!(
            serde_yaml::to_string(&MemoryIsolation::SharedRead)
                .unwrap()
                .trim(),
            "shared-read"
        );
        let parsed: MemoryIsolation = serde_yaml::from_str("shared-read").unwrap();
        assert_eq!(parsed, MemoryIsolation::SharedRead);
    }

    #[test]
    fn parse_all_fields() {
        let md = r#"---
name: full-agent
persistent: true
skills:
  - twitter
  - memory
memory: shared-read
memory_backend: sqlite
schedule: "0 10 * * *"
channels:
  - telegram
delegates_to:
  - helper
model: gpt-4o
temperature: 0.5
max_tools_per_turn: 5
allowed_tools:
  - shell
---

A fully configured agent.
"#;
        let def = AgentDefinition::parse(md).unwrap();
        assert_eq!(def.name, "full-agent");
        assert!(def.persistent);
        assert_eq!(def.skills, vec!["twitter", "memory"]);
        assert_eq!(def.memory, MemoryIsolation::SharedRead);
        assert_eq!(def.memory_backend, "sqlite");
        assert_eq!(def.schedule.as_deref(), Some("0 10 * * *"));
        assert_eq!(def.channels, vec!["telegram"]);
        assert_eq!(def.delegates_to, vec!["helper"]);
        assert_eq!(def.model.as_deref(), Some("gpt-4o"));
        assert!((def.temperature.unwrap() - 0.5).abs() < f64::EPSILON);
        assert_eq!(def.max_tools_per_turn, 5);
        assert_eq!(def.allowed_tools, vec!["shell"]);
        assert!(def.personality.contains("fully configured"));
    }

    #[test]
    fn normalize_cron_5_fields() {
        let result = normalize_cron_expression("10 * * * *").unwrap();
        assert_eq!(result, "0 10 * * * *");
    }

    #[test]
    fn normalize_cron_6_fields() {
        let result = normalize_cron_expression("0 10 * * * *").unwrap();
        assert_eq!(result, "0 10 * * * *");
    }

    #[test]
    fn normalize_cron_invalid_count() {
        assert!(normalize_cron_expression("* *").is_err());
    }

    #[test]
    fn from_file_nonexistent_fails() {
        assert!(AgentDefinition::from_file(std::path::Path::new("/nonexistent/file.md")).is_err());
    }
}
