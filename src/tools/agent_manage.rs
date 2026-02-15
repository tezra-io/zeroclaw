use crate::agent::bus::AgentBus;
use crate::agent::definition::AgentDefinition;
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
    fn name(&self) -> &str {
        "agent_manage"
    }

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
                "description": {
                    "type": "string",
                    "description": "Natural language description (for create)"
                },
                "skills": { "type": "array", "items": { "type": "string" } },
                "persistent": { "type": "boolean" },
                "task": {
                    "type": "string",
                    "description": "Task to delegate (for delegate action)"
                }
            },
            "required": ["action"]
        })
    }

    #[allow(clippy::too_many_lines)]
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
                            let kind = if a.persistent {
                                "persistent"
                            } else {
                                "ephemeral"
                            };
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

            "create" => {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'name' required for create"))?;
                let persistent = args["persistent"].as_bool().unwrap_or(false);
                let skills: Vec<String> = args["skills"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                let definition = AgentDefinition {
                    name: name.to_string(),
                    persistent,
                    skills,
                    ..AgentDefinition::default()
                };

                match self.registry.create(&definition) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Agent '{name}' created successfully."),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: format!("Failed to create agent '{name}': {e}"),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "modify" => {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'name' required for modify"))?;

                let Some(mut def) = self.registry.get(name) else {
                    return Ok(ToolResult {
                        success: false,
                        output: format!("Agent '{name}' not found."),
                        error: None,
                    });
                };

                if let Some(p) = args["persistent"].as_bool() {
                    def.persistent = p;
                }
                if let Some(skills) = args["skills"].as_array() {
                    def.skills = skills
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                }

                match self.registry.update(&def) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Agent '{name}' updated."),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: format!("Failed to update agent '{name}': {e}"),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "remove" => {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'name' required for remove"))?;

                match self.registry.remove(name) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Agent '{name}' removed."),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: format!("Failed to remove agent '{name}': {e}"),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "delegate" => {
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'name' required for delegate"))?;
                let task = args["task"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'task' required for delegate"))?;

                if !self.bus.is_registered(name).await {
                    return Ok(ToolResult {
                        success: false,
                        output: format!("Agent '{name}' is not running. Start it first."),
                        error: None,
                    });
                }

                match self
                    .bus
                    .delegate(
                        "main",
                        name,
                        task,
                        Duration::from_secs(DELEGATION_TIMEOUT_SECS),
                    )
                    .await
                {
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
                                if a.persistent {
                                    "persistent"
                                } else {
                                    "ephemeral"
                                },
                                if running { "RUNNING" } else { "stopped" }
                            )
                        })
                        .collect();
                    if status_lines.is_empty() {
                        Ok(ToolResult {
                            success: true,
                            output: "No agents defined.".into(),
                            error: None,
                        })
                    } else {
                        Ok(ToolResult {
                            success: true,
                            output: status_lines.join("\n"),
                            error: None,
                        })
                    }
                } else {
                    let running = self.bus.is_registered(name).await;
                    Ok(ToolResult {
                        success: true,
                        output: format!(
                            "{name}: {}",
                            if running { "RUNNING" } else { "stopped" }
                        ),
                        error: None,
                    })
                }
            }

            _ => Ok(ToolResult {
                success: false,
                output: format!("Unknown action: {action}"),
                error: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::bus::AgentBus;
    use crate::agent::registry::AgentRegistry;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<AgentRegistry>, Arc<AgentBus>) {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let registry = Arc::new(AgentRegistry::new(&agents_dir));
        let bus = Arc::new(AgentBus::new());
        (tmp, registry, bus)
    }

    #[tokio::test]
    async fn list_empty() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No agents"));
    }

    #[tokio::test]
    async fn create_and_list() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);

        let result = tool
            .execute(json!({
                "action": "create",
                "name": "test-agent",
                "persistent": false
            }))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("test-agent"));
    }

    #[tokio::test]
    async fn create_duplicate_fails() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);

        tool.execute(json!({"action": "create", "name": "dup"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({"action": "create", "name": "dup"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("already exists"));
    }

    #[tokio::test]
    async fn remove_agent() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);

        tool.execute(json!({"action": "create", "name": "removable"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({"action": "remove", "name": "removable"}))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(!result.output.contains("removable"));
    }

    #[tokio::test]
    async fn remove_nonexistent() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool
            .execute(json!({"action": "remove", "name": "ghost"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn modify_agent() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);

        tool.execute(json!({"action": "create", "name": "modifiable"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({
                "action": "modify",
                "name": "modifiable",
                "persistent": true
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn modify_nonexistent() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool
            .execute(json!({"action": "modify", "name": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn status_all_empty() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No agents"));
    }

    #[tokio::test]
    async fn status_single_stopped() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool
            .execute(json!({"action": "status", "name": "ghost"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("stopped"));
    }

    #[tokio::test]
    async fn status_all_with_agents() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry.clone(), bus.clone());

        // Create an agent
        tool.execute(json!({"action": "create", "name": "statusable"}))
            .await
            .unwrap();

        // Register it on the bus to simulate running
        let _rx = bus.register("statusable", 10).await;

        let result = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("RUNNING"));
    }

    #[tokio::test]
    async fn delegate_not_running() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool
            .execute(json!({
                "action": "delegate",
                "name": "offline",
                "task": "do stuff"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not running"));
    }

    #[tokio::test]
    async fn delegate_with_response() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus.clone());

        let mut rx = bus.register("worker", 10).await;

        let tool_arc = Arc::new(tool);
        let tool_clone = tool_arc.clone();
        let handle = tokio::spawn(async move {
            tool_clone
                .execute(json!({
                    "action": "delegate",
                    "name": "worker",
                    "task": "sprint status?"
                }))
                .await
        });

        // Simulate worker responding
        let msg = rx.recv().await.unwrap();
        msg.response_tx
            .unwrap()
            .send("Sprint on track".into())
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(result.success);
        assert!(result.output.contains("Sprint on track"));
    }

    #[tokio::test]
    async fn unknown_action() {
        let (_tmp, registry, bus) = setup();
        let tool = AgentManageTool::new(registry, bus);
        let result = tool
            .execute(json!({"action": "explode"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("Unknown action"));
    }

    #[test]
    fn tool_metadata() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let registry = Arc::new(AgentRegistry::new(&agents_dir));
        let bus = Arc::new(AgentBus::new());
        let tool = AgentManageTool::new(registry, bus);

        assert_eq!(tool.name(), "agent_manage");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }
}
