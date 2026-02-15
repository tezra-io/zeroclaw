use super::definition::AgentDefinition;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct AgentRegistry {
    agents_dir: PathBuf,
}

impl AgentRegistry {
    /// Create a registry. `agents_dir` = `~/.zeroclaw/agents/`
    pub fn new(agents_dir: &Path) -> Self {
        Self {
            agents_dir: agents_dir.to_path_buf(),
        }
    }

    /// Create from config (derives `agents_dir` from `workspace_dir`)
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::new(&super::definition::agents_dir_from_config(config))
    }

    /// List all agent definitions
    pub fn list(&self) -> Vec<AgentDefinition> {
        let Ok(entries) = std::fs::read_dir(&self.agents_dir) else {
            return Vec::new();
        };

        let mut agents: Vec<AgentDefinition> = entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .filter_map(|e| AgentDefinition::from_file(&e.path()).ok())
            .collect();

        agents.sort_by(|a, b| a.name.cmp(&b.name));
        agents
    }

    /// Get a specific agent by name
    pub fn get(&self, name: &str) -> Option<AgentDefinition> {
        let path = self.agent_path(name);
        AgentDefinition::from_file(&path).ok()
    }

    /// Create a new agent definition file
    pub fn create(&self, definition: &AgentDefinition) -> Result<()> {
        std::fs::create_dir_all(&self.agents_dir)?;
        let path = self.agent_path(&definition.name);
        if path.exists() {
            anyhow::bail!("Agent '{}' already exists", definition.name);
        }
        std::fs::write(&path, definition.to_markdown())?;

        // Create agent data directory for persistent agents
        if definition.persistent {
            let data_dir = self.data_dir(&definition.name);
            std::fs::create_dir_all(&data_dir)?;
        }
        Ok(())
    }

    /// Update an existing agent definition
    pub fn update(&self, definition: &AgentDefinition) -> Result<()> {
        let path = self.agent_path(&definition.name);
        if !path.exists() {
            anyhow::bail!("Agent '{}' not found", definition.name);
        }
        std::fs::write(&path, definition.to_markdown())?;
        Ok(())
    }

    /// Remove an agent and its data directory
    pub fn remove(&self, name: &str) -> Result<()> {
        let md_path = self.agent_path(name);
        if !md_path.exists() {
            anyhow::bail!("Agent '{name}' not found");
        }
        std::fs::remove_file(&md_path)?;

        let data_dir = self.data_dir(name);
        if data_dir.exists() {
            std::fs::remove_dir_all(&data_dir)?;
        }
        Ok(())
    }

    /// Check if an agent exists
    pub fn exists(&self, name: &str) -> bool {
        self.agent_path(name).exists()
    }

    /// Get the data directory for a specific agent
    pub fn data_dir(&self, name: &str) -> PathBuf {
        self.agents_dir.join(name)
    }

    /// Get the definition file path for a specific agent
    fn agent_path(&self, name: &str) -> PathBuf {
        self.agents_dir.join(format!("{name}.md"))
    }
}

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

    fn persistent_def(name: &str) -> AgentDefinition {
        AgentDefinition::parse(&format!(
            "---\nname: {name}\npersistent: true\n---\nPersistent agent"
        ))
        .unwrap()
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
        reg.create(&test_def("b")).unwrap();
        reg.create(&test_def("a")).unwrap();
        let agents = reg.list();
        assert_eq!(agents.len(), 2);
        // Sorted by name
        assert_eq!(agents[0].name, "a");
        assert_eq!(agents[1].name, "b");
    }

    #[test]
    fn list_empty() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn list_nonexistent_dir() {
        let reg = AgentRegistry::new(std::path::Path::new("/nonexistent/agents"));
        assert!(reg.list().is_empty());
    }

    #[test]
    fn remove_agent() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("gone")).unwrap();
        assert!(reg.exists("gone"));
        reg.remove("gone").unwrap();
        assert!(!reg.exists("gone"));
    }

    #[test]
    fn remove_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(reg.remove("nope").is_err());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn update_existing() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("updatable")).unwrap();

        let mut def = reg.get("updatable").unwrap();
        def.personality = "Updated personality".to_string();
        reg.update(&def).unwrap();

        let updated = reg.get("updatable").unwrap();
        assert!(updated.personality.contains("Updated"));
    }

    #[test]
    fn update_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(reg.update(&test_def("nope")).is_err());
    }

    #[test]
    fn persistent_agent_creates_data_dir() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&persistent_def("persist")).unwrap();
        assert!(reg.data_dir("persist").exists());
    }

    #[test]
    fn ephemeral_agent_no_data_dir() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&test_def("ephemeral")).unwrap();
        assert!(!reg.data_dir("ephemeral").exists());
    }

    #[test]
    fn remove_cleans_data_dir() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        reg.create(&persistent_def("cleanup")).unwrap();
        assert!(reg.data_dir("cleanup").exists());
        reg.remove("cleanup").unwrap();
        assert!(!reg.data_dir("cleanup").exists());
    }

    #[test]
    fn exists_checks_md_file() {
        let tmp = TempDir::new().unwrap();
        let reg = test_registry(&tmp);
        assert!(!reg.exists("nope"));
        reg.create(&test_def("yep")).unwrap();
        assert!(reg.exists("yep"));
    }

    #[test]
    fn from_config() {
        let config = crate::config::Config {
            workspace_dir: std::path::PathBuf::from("/home/user/.zeroclaw/workspace"),
            ..crate::config::Config::default()
        };
        let reg = AgentRegistry::from_config(&config);
        // Just verify it doesn't panic and points to the right place
        assert!(reg.agent_path("test").to_string_lossy().contains("agents"));
    }
}
