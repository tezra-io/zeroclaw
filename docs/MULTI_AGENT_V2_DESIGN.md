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

### Integration with ZeroClaw

ZeroClaw's existing modules we build on top of:
- `src/skills/mod.rs` — skill loading, SKILL.toml/SKILL.md parsing, install/remove
- `src/memory/` — SQLite + FTS5 + vector + hybrid search (per-agent instances)
- `src/security/policy.rs` — command/path sandboxing (per-agent scoping)
- `src/channels/` — all 7 channel implementations
- `src/config/schema.rs` — extend with agent config
- `src/daemon/mod.rs` — extend component supervisor for persistent agents

New modules to add:
- `src/agent/definition.rs` — agent definition parser (markdown + YAML frontmatter)
- `src/agent/registry.rs` — agent lifecycle management
- `src/agent/bus.rs` — inter-agent message bus (tokio mpsc)
- `src/agent/supervisor.rs` — persistent agent supervision
- `src/agent/generator.rs` — AI-powered agent definition generation
- `src/agent/loop_.rs` — extend existing with tool calling + multi-turn

---

## Agent Definition Format

Agents are markdown files in `~/.zeroclaw/agents/<name>.md`:

```markdown
---
name: twitter-agent
persistent: true
skills:
  - bird
  - image-gen
memory: isolated
schedule: "0 10,20 * * *"
channels: []              # empty = no direct channel access
delegates_to: []          # can't spawn sub-agents
---

# Twitter Agent

You manage the @handle Twitter account. Post twice daily.
Track what performs well and adjust your approach.
Keep posts authentic, not corporate.
```

### YAML Frontmatter Schema

```yaml
name: string              # required, unique identifier
persistent: bool          # default: false (ephemeral)
skills: [string]          # skill names from ~/.zeroclaw/workspace/skills/
memory: enum              # isolated | shared-read | shared (default: isolated)
schedule: string | null   # cron expression for autonomous runs
channels: [string]        # which channels this agent can respond on (empty = none)
delegates_to: [string]    # agent names this agent can spawn/delegate to
model: string | null      # override default model
temperature: float | null # override default temperature
max_tools_per_turn: int   # default: 10
```

### Memory Isolation Levels

| Level | Read main memory | Own memory | Read other agents |
|-------|-----------------|------------|-------------------|
| `isolated` | ❌ | ✅ own SQLite | ❌ |
| `shared-read` | ✅ read-only | ✅ own SQLite | ❌ |
| `shared` | ✅ read/write | ✅ own SQLite | ✅ specified agents |

Each agent gets its own SQLite database at `~/.zeroclaw/agents/<name>/memory.db`.
Main agent's memory stays at the workspace level (existing behavior).

---

## Agent Creation Flow

### CLI Command

```bash
# Natural language creation (AI generates the definition)
zeroclaw agent create "manages my twitter, posts twice daily, tracks engagement"

# From explicit flags (power users)
zeroclaw agent create twitter-agent \
  --persistent \
  --skills bird,image-gen \
  --schedule "0 10,20 * * *" \
  --memory isolated
```

### What Happens (natural language mode)

1. User provides description string
2. System loads skill index (`~/.zeroclaw/workspace/skills/index.json`)
3. Main agent (or a generation prompt) receives:
   - User's description
   - Available skills list (name + description only)
   - Agent definition schema
4. AI generates the complete agent definition (markdown + YAML)
5. User sees a preview and confirms (or edits)
6. Definition saved to `~/.zeroclaw/agents/<name>.md`
7. If persistent: daemon supervisor starts it immediately
8. If scheduled: cron scheduler registers the schedule

### Skill Index

Auto-rebuilt on `skill install` / `skill remove`:

```json
[
  {"name": "bird", "description": "Post to X/Twitter, read timelines", "tools": ["bird_post", "bird_timeline"]},
  {"name": "dev-browser", "description": "Browser automation with screenshots", "tools": ["screenshot", "navigate"]},
  {"name": "code-review", "description": "Review code changes, suggest improvements", "tools": ["diff_read", "comment"]}
]
```

Lightweight — main agent reads this to match user intent to skills without loading full SKILL.md files.

---

## Agent Management Commands

```bash
# Create
zeroclaw agent create "description..."
zeroclaw agent create <name> --persistent --skills x,y

# List
zeroclaw agent list                    # shows all agents with status
zeroclaw agent list --persistent       # only persistent ones

# Modify
zeroclaw agent edit <name>             # opens definition in $EDITOR
zeroclaw agent skill add <name> <skill>
zeroclaw agent skill remove <name> <skill>

# Lifecycle
zeroclaw agent start <name>            # start a persistent agent
zeroclaw agent stop <name>             # stop it
zeroclaw agent restart <name>          # restart
zeroclaw agent status <name>           # memory size, uptime, last run

# Delete
zeroclaw agent remove <name>           # removes definition + memory (with confirmation)

# Natural language modification (via main agent)
# User just tells main agent: "give the twitter agent image generation"
# Main agent updates the definition file directly
```

---

## Inter-Agent Communication (Message Bus)

### Design

```rust
// Lightweight message bus using tokio mpsc
pub struct AgentBus {
    agents: HashMap<String, mpsc::Sender<AgentMessage>>,
}

pub struct AgentMessage {
    id: Uuid,
    from: String,        // agent name
    to: String,          // agent name or "main"
    kind: MessageKind,
    payload: String,
    trace_id: Uuid,      // for request/response correlation
}

pub enum MessageKind {
    Delegate,            // "please handle this task"
    Result,              // "here's what I found"
    Query,               // "do you know X?"
    Notify,              // fire-and-forget notification
    Shutdown,            // graceful stop
}
```

### Routing

1. Main agent receives user message
2. If the task matches a persistent agent's skills → delegate via bus
3. If no match but task is complex → spawn ephemeral worker with relevant skills
4. If simple → main handles directly

Main agent can also delegate based on explicit user intent:
- "Tell the twitter agent to..." → direct routing by name
- "Post this on twitter" → skill-based routing (matches `bird` skill → twitter-agent)

---

## Skill Assignment to Sub-Agents

### At Creation Time

```bash
zeroclaw agent create "monitors github PRs and reviews code" 
# AI detects: needs github skill + code-review skill
# Generates definition with skills: [github, code-review]
```

### After Creation

```bash
zeroclaw agent skill add pm-agent linear
```

Or via natural language to main agent:
> "Give the PM agent access to Linear"

Main agent:
1. Checks if `linear` skill is installed (`~/.zeroclaw/workspace/skills/linear/`)
2. If not: `zeroclaw skill install <source>` 
3. Updates the agent's definition YAML: adds `linear` to `skills` list
4. If agent is running: hot-reload the skill (re-read definition, rebuild system prompt)

### Skill Loading at Runtime

When an agent starts (or a message arrives for an ephemeral worker):

```rust
fn build_agent_context(definition: &AgentDefinition) -> SystemPrompt {
    let mut prompt = String::new();
    
    // 1. Agent's personality (markdown body from definition)
    prompt.push_str(&definition.personality);
    
    // 2. Load each skill's SKILL.md / SKILL.toml
    for skill_name in &definition.skills {
        if let Some(skill) = load_skill(skill_name) {
            prompt.push_str(&skills_to_prompt(&[skill]));
        }
    }
    
    // 3. Memory context (from agent's own SQLite)
    // ... existing memory recall logic
    
    prompt
}
```

---

## Persistent Agent Lifecycle

### Supervisor Integration

Extend `src/daemon/mod.rs` component supervisor:

```rust
// Current daemon components:
// - Gateway
// - Channels  
// - Heartbeat
// - Cron

// Add:
// - PersistentAgents (loads all agents where persistent: true)
```

Each persistent agent runs as a supervised task:
- Crash → exponential backoff restart (existing pattern)
- Schedule → cron triggers `agent.run()` at defined times
- Channel message → routed via bus if agent has channel access
- Delegation → main agent sends via bus

### State Persistence

Persistent agents maintain state in their memory DB:
- Conversation history (for context continuity)
- Skill-specific data (e.g., twitter agent stores post performance)
- Run history (when it ran, what it did, outcomes)

---

## Ephemeral Agent Lifecycle

1. Main agent decides to delegate
2. Creates temporary `AgentDefinition` with specified skills
3. Spawns tokio task with its own memory (in-memory SQLite or temp file)
4. Worker executes, returns result via bus
5. Main agent receives result, continues
6. Worker task ends, temp memory dropped

No definition file written. No CLI command needed. Pure runtime delegation.

---

## Implementation Phases

### Phase 1: Foundation (on ZeroClaw) — ~3 days
- [ ] `src/agent/definition.rs` — parse markdown + YAML frontmatter agent definitions
- [ ] `src/agent/registry.rs` — load/list/create/remove agent definitions from `~/.zeroclaw/agents/`
- [ ] `src/agent/bus.rs` — tokio mpsc message bus with `AgentMessage` type
- [ ] Extend `src/agent/loop_.rs` — add tool calling loop (currently missing in zeroclaw)
- [ ] CLI: `zeroclaw agent list`, `zeroclaw agent create <name>`, `zeroclaw agent remove <name>`
- [ ] Tests: definition parsing, registry CRUD, bus message routing

### Phase 2: Skills + Agent Creation — ~2 days
- [ ] Extend `src/skills/mod.rs` — add skill index generation (`index.json`)
- [ ] `src/agent/generator.rs` — AI-powered agent definition generation from natural language
- [ ] CLI: `zeroclaw agent create "description..."` (natural language mode)
- [ ] CLI: `zeroclaw agent skill add/remove <name> <skill>`
- [ ] Skill-to-agent matching logic (keyword + description matching)
- [ ] Tests: generation, skill matching, index rebuild

### Phase 3: Persistent Agents + Memory Isolation — ~3 days
- [ ] Per-agent SQLite memory instances (`~/.zeroclaw/agents/<name>/memory.db`)
- [ ] Memory isolation enforcement (isolated / shared-read / shared)
- [ ] Extend `src/daemon/mod.rs` — supervisor for persistent agents
- [ ] Scheduled agent runs via cron integration
- [ ] CLI: `zeroclaw agent start/stop/restart/status <name>`
- [ ] Tests: memory isolation, supervisor lifecycle, scheduled runs

### Phase 4: Delegation + Orchestration — ~2 days
- [ ] Main agent routing logic (skill-based + name-based)
- [ ] Ephemeral worker spawning with skill attachment
- [ ] Result collection and response merging
- [ ] Natural language agent modification via main agent
- [ ] Hot-reload agent definitions on change
- [ ] Tests: delegation routing, ephemeral lifecycle, hot-reload

### Phase 5: Polish — ~1 day
- [ ] `zeroclaw agent status` with memory stats, run history, uptime
- [ ] Agent logs viewer
- [ ] Error handling for missing skills, failed agents, bus timeouts
- [ ] Documentation: README section, CLAUDE.md update

---

## Open Questions

1. **Should ephemeral workers share the main agent's conversation context?** Or start fresh with just the delegated task?
2. **Hot-reload granularity** — reload skills only, or full agent restart?
3. **Agent-to-agent communication** — should workers be able to talk to each other, or only through main?
4. **Skill compatibility** — should skills declare which agent types they support? (some skills may not make sense for ephemeral workers)
5. **Resource limits** — max concurrent agents? Max memory per agent?

---

*This design integrates with ZeroClaw's existing architecture. No rewrites — only extensions.*
