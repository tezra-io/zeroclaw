# Multi-Agent v2 Design — ZeroClaw Integration

*Natural language agent creation. Skills-first. Zero config overhead.*

---

## Philosophy

1. **Describe, don't configure** — user says what they need in plain english, system generates everything
2. **Skills > agents** — users think in capabilities, not infrastructure
3. **Grow organically** — start with one agent, add more as needs emerge
4. **Main agent is the orchestrator** — it creates, delegates, and manages sub-agents

---

## Architecture Overview

```
User
  ↓
Main Agent (always exists, created at onboard)
  ├── [persistent] Twitter Agent (isolated memory, scheduled)
  ├── [persistent] PM Agent (shared-read memory, always available)
  ├── [ephemeral] Research Worker (spun up for a task, dies after)
  └── [ephemeral] Code Review Worker
```

---

## Current ZeroClaw Gaps (what we're adding)

The existing agent loop (`src/agent/loop_.rs`) has **no tool calling**. It sends one message to the LLM and prints the response. The `Provider` trait (`src/providers/traits.rs`) returns `String`, not structured messages. Tools exist (`src/tools/`) with proper `Tool` trait, `ToolSpec`, and `execute()` — but they're never actually called by the agent loop.

**We need to add:**
1. Tool calling loop in the agent
2. Multi-turn conversation (not single-shot)
3. Agent definitions (markdown files)
4. Agent registry + lifecycle
5. Inter-agent message bus
6. AI-powered agent creation
7. Natural language agent management via main agent (a tool)

---

## Implementation Plan

### Phase 1: Tool Calling + Multi-Turn Agent Loop (~3 days)

**The most critical gap.** Without this, nothing else works.

#### 1a. Extend Provider trait for structured messages

**File: `src/providers/traits.rs`** — add structured message types alongside existing trait

```rust
// ADD these types (keep existing Provider trait unchanged for now):

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,           // "system", "user", "assistant", "tool"
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,         // "function"
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,      // JSON string
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

// ADD new trait (don't break existing Provider):
#[async_trait]
pub trait ChatProvider: Provider {
    /// Multi-turn chat with tool definitions
    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse>;
}
```

#### 1b. Implement ChatProvider for OpenRouter/Anthropic/OpenAI

**Files to modify:**
- `src/providers/openrouter.rs` — add `impl ChatProvider for OpenRouterProvider`
- `src/providers/anthropic.rs` — add `impl ChatProvider for AnthropicProvider`
- `src/providers/openai.rs` — add `impl ChatProvider for OpenAiProvider`
- `src/providers/compatible.rs` — add `impl ChatProvider for CompatibleProvider`
- `src/providers/mod.rs` — update factory to return `Box<dyn ChatProvider>`

Each implementation sends the messages array + tools array to the API's native format.
Anthropic uses `tools` array with `input_schema`. OpenAI/OpenRouter use `functions` or `tools` format.

#### 1c. Tool calling loop in agent

**File: `src/agent/loop_.rs`** — rewrite `run()` to support multi-turn tool calling

```rust
// New function alongside existing run():
pub async fn run_with_tools(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    // ... setup (same as existing run()) ...

    let tools = tools::all_tools(&security, mem.clone(), composio_key, &config.browser);
    let tool_specs: Vec<ToolSpec> = tools.iter().map(|t| t.spec()).collect();

    // Build conversation history
    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage { role: "system".into(), content: Some(system_prompt), .. },
        ChatMessage { role: "user".into(), content: Some(user_msg), .. },
    ];

    // Tool calling loop (max 20 iterations)
    for _ in 0..20 {
        let response = provider.chat_completion(&messages, &tool_specs, model, temp).await?;
        messages.push(response.message.clone());

        // If no tool calls, we're done — print the response
        if response.message.tool_calls.is_none() {
            if let Some(content) = &response.message.content {
                println!("{content}");
            }
            break;
        }

        // Execute each tool call
        for tool_call in response.message.tool_calls.unwrap() {
            let tool = tools.iter().find(|t| t.name() == tool_call.function.name);
            let result = match tool {
                Some(t) => {
                    let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)?;
                    t.execute(args).await?
                }
                None => ToolResult { success: false, output: "Unknown tool".into(), error: None },
            };
            // Add tool result to messages
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(result.output),
                tool_call_id: Some(tool_call.id),
                ..
            });
        }
    }
}
```

**Integrate:** Update `main.rs` `Commands::Agent` match arm to call `run_with_tools` instead of `run`.

---

### Phase 2: Agent Definitions + Registry (~2 days)

#### 2a. Agent definition parser

**New file: `src/agent/definition.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default = "default_memory")]
    pub memory: MemoryIsolation,
    #[serde(default)]
    pub schedule: Option<String>,       // cron expression
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
    /// The markdown body (personality/instructions)
    #[serde(skip)]
    pub personality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryIsolation {
    #[default]
    Isolated,
    SharedRead,
    Shared,
}

fn default_memory() -> MemoryIsolation { MemoryIsolation::Isolated }
fn default_max_tools() -> usize { 10 }

impl AgentDefinition {
    /// Parse from a markdown file with YAML frontmatter
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse from markdown string: `---\nyaml\n---\nmarkdown body`
    pub fn parse(content: &str) -> Result<Self> {
        // Split YAML frontmatter from markdown body
        // Parse YAML into AgentDefinition
        // Set personality = markdown body after second ---
    }

    /// Serialize back to markdown + YAML frontmatter
    pub fn to_markdown(&self) -> String { ... }
}
```

**Location:** `~/.zeroclaw/agents/<name>.md`

#### 2b. Agent registry

**New file: `src/agent/registry.rs`**

```rust
pub struct AgentRegistry {
    agents_dir: PathBuf,    // ~/.zeroclaw/agents/
}

impl AgentRegistry {
    pub fn new(workspace_dir: &Path) -> Self { ... }
    pub fn list(&self) -> Vec<AgentDefinition> { ... }
    pub fn get(&self, name: &str) -> Option<AgentDefinition> { ... }
    pub fn create(&self, definition: &AgentDefinition) -> Result<()> { ... }
    pub fn update(&self, definition: &AgentDefinition) -> Result<()> { ... }
    pub fn remove(&self, name: &str) -> Result<()> { ... }
    pub fn exists(&self, name: &str) -> bool { ... }
}
```

#### 2c. CLI commands

**File: `src/main.rs`** — add `AgentCommands` subcommand enum:

```rust
#[derive(Subcommand, Debug)]
enum AgentCommands {
    /// List all agents
    List,
    /// Create an agent (natural language or flags)
    Create {
        /// Description in natural language, OR agent name with --flags
        description: String,
        #[arg(long)]
        persistent: bool,
        #[arg(long)]
        skills: Option<String>,     // comma-separated
        #[arg(long)]
        memory: Option<String>,
        #[arg(long)]
        schedule: Option<String>,
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

Add to `Commands` enum:
```rust
/// Manage agents (create, list, edit, remove)
Agents {
    #[command(subcommand)]
    agent_command: AgentCommands,
},
```

Add handler in `main()`:
```rust
Commands::Agents { agent_command } => {
    agent::handle_command(agent_command, &config).await
}
```

**New file: `src/agent/commands.rs`** — handles each `AgentCommands` variant.

---

### Phase 3: Inter-Agent Message Bus (~1 day)

#### 3a. Message bus

**New file: `src/agent/bus.rs`**

```rust
use tokio::sync::mpsc;
use uuid::Uuid;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AgentMessage {
    pub id: Uuid,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub payload: String,
    pub trace_id: Uuid,
}

pub enum MessageKind {
    Delegate,       // "handle this task"
    Result,         // "here's my output"
    Query,          // "do you know X?"
    Notify,         // fire-and-forget
    Shutdown,       // graceful stop
}

pub struct AgentBus {
    senders: Arc<RwLock<HashMap<String, mpsc::Sender<AgentMessage>>>>,
}

impl AgentBus {
    pub fn new() -> Self { ... }
    pub async fn register(&self, name: &str) -> mpsc::Receiver<AgentMessage> { ... }
    pub async fn unregister(&self, name: &str) { ... }
    pub async fn send(&self, msg: AgentMessage) -> Result<()> { ... }
    pub async fn broadcast(&self, from: &str, kind: MessageKind, payload: &str) { ... }
}
```

**Integration point:** `AgentBus` is created in `daemon::run()` and passed to all agent loops.

---

### Phase 4: AI-Powered Agent Creation + Main Agent Tools (~2 days)

#### 4a. Skill index

**File: `src/skills/mod.rs`** — add index generation:

```rust
/// Rebuild the lightweight skill index for agent matching
pub fn rebuild_index(workspace_dir: &Path) -> Result<()> {
    let skills = load_skills(workspace_dir);
    let index: Vec<SkillIndexEntry> = skills.iter().map(|s| SkillIndexEntry {
        name: s.name.clone(),
        description: s.description.clone(),
        tools: s.tools.iter().map(|t| t.name.clone()).collect(),
    }).collect();
    let path = workspace_dir.join("skills").join("index.json");
    std::fs::write(path, serde_json::to_string_pretty(&index)?)?;
    Ok(())
}
```

Call `rebuild_index` at end of `handle_command` for `Install` and `Remove`.

#### 4b. Agent generator

**New file: `src/agent/generator.rs`**

```rust
/// Generate an agent definition from natural language description.
/// Uses the configured LLM to produce the YAML frontmatter + personality.
pub async fn generate_definition(
    description: &str,
    skill_index: &[SkillIndexEntry],
    provider: &dyn ChatProvider,
    model: &str,
) -> Result<AgentDefinition> {
    let prompt = format!(
        "Generate an agent definition for this request:\n\n\
         \"{description}\"\n\n\
         Available skills:\n{skills}\n\n\
         Respond with ONLY a markdown file containing YAML frontmatter (---) \
         and a personality section. Use this schema:\n{schema}",
        skills = format_skill_index(skill_index),
        schema = AGENT_DEFINITION_SCHEMA,
    );

    let response = provider.chat_with_system(
        Some("You generate agent definitions. Output only the markdown file, nothing else."),
        &prompt, model, 0.3
    ).await?;

    AgentDefinition::parse(&response)
}
```

**Integration in `src/agent/commands.rs`:**
```rust
AgentCommands::Create { description, persistent, skills, .. } => {
    if skills.is_some() || persistent {
        // Flag-based creation (power user)
        create_from_flags(description, persistent, skills, ...)
    } else {
        // Natural language creation
        let definition = generator::generate_definition(&description, ...).await?;
        println!("{}", definition.to_markdown());
        if confirm("Create this agent?") {
            registry.create(&definition)?;
        }
    }
}
```

#### 4c. Agent management tool (main agent can create/modify agents via conversation)

**New file: `src/tools/agent_manage.rs`**

```rust
pub struct AgentManageTool {
    registry: Arc<AgentRegistry>,
    bus: Arc<AgentBus>,
}

impl Tool for AgentManageTool {
    fn name(&self) -> &str { "agent_manage" }
    fn description(&self) -> &str {
        "Create, modify, list, or remove sub-agents. Use when the user asks to \
         create a new agent, add skills to an agent, or manage existing agents."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "list", "modify", "remove", "delegate"] },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "skills": { "type": "array", "items": { "type": "string" } },
                "persistent": { "type": "boolean" },
                "task": { "type": "string" }  // for delegate action
            },
            "required": ["action"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        match args["action"].as_str() {
            Some("create") => { /* generate + save definition */ }
            Some("list") => { /* return agent list */ }
            Some("modify") => { /* update definition */ }
            Some("remove") => { /* delete agent */ }
            Some("delegate") => { /* send task via bus */ }
            _ => { /* error */ }
        }
    }
}
```

**Integration:** Add to `tools::all_tools()` in `src/tools/mod.rs`:
```rust
tools.push(Box::new(AgentManageTool::new(registry.clone(), bus.clone())));
```

Now when the user says "create an agent that manages my twitter" in conversation, the main agent calls the `agent_manage` tool with `action: "create"`.

---

### Phase 5: Persistent Agents + Memory Isolation (~2 days)

#### 5a. Per-agent memory

**File: `src/memory/mod.rs`** — add factory for agent-specific memory:

```rust
/// Create a memory instance scoped to a specific agent
pub fn create_agent_memory(
    config: &MemoryConfig,
    agents_dir: &Path,
    agent_name: &str,
    api_key: Option<&str>,
) -> Result<Box<dyn Memory>> {
    let agent_dir = agents_dir.join(agent_name);
    std::fs::create_dir_all(&agent_dir)?;
    // Creates SQLite at ~/.zeroclaw/agents/<name>/memory.db
    create_memory(config, &agent_dir, api_key)
}
```

#### 5b. Agent runner (persistent agent loop)

**New file: `src/agent/runner.rs`**

```rust
/// Run a persistent agent with its own memory, skills, and message bus receiver
pub async fn run_persistent_agent(
    definition: AgentDefinition,
    config: &Config,
    bus: Arc<AgentBus>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let memory = memory::create_agent_memory(&config.memory, &agents_dir, &definition.name, ...)?;
    let skills = load_agent_skills(&definition.skills, &config.workspace_dir);
    let system_prompt = build_agent_prompt(&definition, &skills);
    let mut receiver = bus.register(&definition.name).await;

    loop {
        tokio::select! {
            Some(msg) = receiver.recv() => {
                // Handle delegated task
                let response = run_agent_turn(&definition, &system_prompt, &msg.payload, ...).await?;
                bus.send(AgentMessage { to: msg.from, kind: MessageKind::Result, payload: response, .. }).await?;
            }
            _ = shutdown.changed() => break,
        }
    }
}
```

#### 5c. Daemon integration

**File: `src/daemon/mod.rs`** — add persistent agents as supervised components:

```rust
// In run() function, after existing component setup:

let registry = AgentRegistry::new(&config.workspace_dir);
let bus = Arc::new(AgentBus::new());

// Start all persistent agents
for definition in registry.list().iter().filter(|d| d.persistent) {
    let def = definition.clone();
    let bus = bus.clone();
    let shutdown_rx = shutdown_tx.subscribe();
    components.push(tokio::spawn(async move {
        agent::runner::run_persistent_agent(def, &config, bus, shutdown_rx).await
    }));
}
```

---

### Phase 6: Onboarding Changes (~0.5 days)

**Minimal changes to `src/onboard/wizard.rs`:**

The onboarding wizard stays focused on getting the user chatting ASAP. No agent questions.

**Add at the end of wizard (after channel setup):**

```
✅ Setup complete! You're ready to chat.

💡 Tip: You can create specialized agents anytime:
   zeroclaw agent create "manages my twitter account"
   
   Or just tell your agent: "Create an agent that handles code reviews"
```

**Add to workspace scaffolding** (the part that creates initial MD files):
- Create `~/.zeroclaw/agents/` directory
- No default agents — main agent is implicit, not a definition file

---

### Phase 7: Scheduled Agent Runs (~1 day)

**File: `src/cron/scheduler.rs`** — extend to support agent-specific tasks:

```rust
// Current: cron runs shell commands
// Add: cron can trigger agent runs

pub enum CronAction {
    Shell(String),                    // existing
    AgentRun(String),                 // agent name — triggers a run
    AgentMessage(String, String),     // agent name, message — sends a task
}
```

When a persistent agent has `schedule: "0 10,20 * * *"`, the daemon registers a cron job:
```rust
scheduler.add(CronJob {
    expression: definition.schedule.clone(),
    action: CronAction::AgentRun(definition.name.clone()),
});
```

On trigger, sends a `Delegate` message via bus with a configurable prompt (or the agent just runs its personality prompt autonomously).

---

## File Summary

### New files (8):
| File | Purpose | Lines (est) |
|------|---------|-------------|
| `src/agent/definition.rs` | Agent definition parser (YAML frontmatter + markdown) | ~200 |
| `src/agent/registry.rs` | Agent CRUD, filesystem management | ~150 |
| `src/agent/bus.rs` | Inter-agent tokio mpsc message bus | ~120 |
| `src/agent/runner.rs` | Persistent agent run loop | ~150 |
| `src/agent/generator.rs` | AI-powered definition generation | ~100 |
| `src/agent/commands.rs` | CLI command handlers for `zeroclaw agent *` | ~200 |
| `src/tools/agent_manage.rs` | Tool for main agent to manage sub-agents in conversation | ~150 |
| `docs/AGENTS.md` | User-facing agent documentation | ~100 |

### Modified files (8):
| File | Change |
|------|--------|
| `src/main.rs` | Add `mod` declarations, `AgentCommands` enum, match arms |
| `src/agent/mod.rs` | Export new submodules |
| `src/agent/loop_.rs` | Add `run_with_tools()` with tool calling loop |
| `src/providers/traits.rs` | Add `ChatMessage`, `ToolCall`, `ChatProvider` trait |
| `src/providers/openrouter.rs` | Implement `ChatProvider` |
| `src/providers/anthropic.rs` | Implement `ChatProvider` |
| `src/providers/openai.rs` | Implement `ChatProvider` |
| `src/providers/compatible.rs` | Implement `ChatProvider` |
| `src/providers/mod.rs` | Update factory for `ChatProvider` |
| `src/tools/mod.rs` | Add `agent_manage` tool to registry |
| `src/daemon/mod.rs` | Start persistent agents, create bus |
| `src/cron/scheduler.rs` | Add `AgentRun`/`AgentMessage` cron actions |
| `src/skills/mod.rs` | Add `rebuild_index()`, `SkillIndexEntry` |
| `src/onboard/wizard.rs` | Add agent tip at end, create agents dir |

---

## How Main Agent Handles Agent Creation in Conversation

When a user says "create an agent that manages my twitter" to the main agent:

1. Main agent's tool calling loop sees this is an agent management request
2. Calls `agent_manage` tool with `{ "action": "create", "description": "manages my twitter" }`
3. `agent_manage.execute()` internally:
   a. Reads skill index
   b. Calls `generator::generate_definition()` which uses the LLM
   c. Saves to `~/.zeroclaw/agents/twitter-agent.md`
   d. If persistent: registers with daemon supervisor + bus
   e. Returns success message to main agent
4. Main agent tells user: "Created twitter-agent with bird skill, running on schedule 10am/8pm"

Same flow for modification:
> "Give the twitter agent image generation"

1. Main agent calls `agent_manage` with `{ "action": "modify", "name": "twitter-agent", "skills": ["image-gen"] }`
2. Tool updates definition file, hot-reloads if running
3. Main agent confirms

And delegation:
> "Ask the PM agent for sprint status"

1. Main agent calls `agent_manage` with `{ "action": "delegate", "name": "pm-agent", "task": "What's the sprint status?" }`
2. Tool sends message via bus, waits for response
3. Returns result to main agent, which relays to user

---

## Memory Isolation Implementation

Each agent gets its own SQLite at `~/.zeroclaw/agents/<name>/memory.db`.

| Level | Implementation |
|-------|---------------|
| `isolated` | Agent only accesses its own `memory.db`. No access to main workspace memory. |
| `shared-read` | Agent reads from main workspace memory (read-only connection) + writes to own `memory.db` |
| `shared` | Agent reads/writes main workspace memory + own `memory.db` + can read specified agents' DBs |

In code (`src/agent/runner.rs`):
```rust
let memory: Arc<dyn Memory> = match definition.memory {
    MemoryIsolation::Isolated => {
        Arc::from(create_agent_memory(&config.memory, &agents_dir, &definition.name, api_key)?)
    }
    MemoryIsolation::SharedRead => {
        // Composite memory: reads from main + writes to agent-specific
        Arc::from(CompositeMemory::new(
            create_memory(&config.memory, &config.workspace_dir, api_key)?,  // read source
            create_agent_memory(&config.memory, &agents_dir, &definition.name, api_key)?,  // write target
        ))
    }
    MemoryIsolation::Shared => {
        // Full access to main workspace memory
        Arc::from(create_memory(&config.memory, &config.workspace_dir, api_key)?)
    }
};
```

New `CompositeMemory` struct needed in `src/memory/composite.rs` — wraps two `Memory` instances, routes `recall()` to both, `store()` to write target only.

---

## Open Questions

1. **Should ephemeral workers share the main agent's conversation context?** Recommendation: No. Pass only the delegated task. Clean context = better results.
2. **Hot-reload granularity** — Recommendation: Full agent restart on definition change. Simpler, no edge cases.
3. **Agent-to-agent communication** — Recommendation: Only through main for now. Direct agent-to-agent adds complexity without clear benefit yet.
4. **Resource limits** — Recommendation: Max 10 concurrent agents. Configurable in config.toml.
5. **Should we rename zeroclaw?** — Separate decision. The fork can be renamed later without architectural impact.

---

*This design extends ZeroClaw's existing architecture. No rewrites — only additions.*
