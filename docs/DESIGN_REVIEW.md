# Multi-Agent v2 Design Review

**Reviewer:** Claude Code
**Date:** 2026-02-14
**Document reviewed:** `docs/MULTI_AGENT_V2_DESIGN.md`
**Verdict:** The design correctly identifies the core gaps but has **14 critical issues**, **12 significant gaps**, and **8 minor issues** that would cause compilation failures, runtime panics, or silent misbehavior if implemented as written.

---

## Accuracy of Gap Analysis

The design's assessment of the current codebase (lines 32-42) is **correct on all points:**

- `src/agent/loop_.rs:62` — tools ARE created but assigned to `_tools` (underscore prefix = unused)
- `src/providers/traits.rs:4-17` — Provider trait returns `String`, not structured messages
- `src/tools/traits.rs:22-43` — Tool trait with `spec()`, `execute()`, `ToolSpec`, `ToolResult` exists and is well-designed
- `src/agent/loop_.rs:146-148` — Agent loop IS single-shot (`chat_with_system` called once, response printed)

---

## CRITICAL ISSUES (will not compile or will panic)

### C1. `ReliableProvider` not addressed — `ChatProvider` chain broken

**File:** `src/providers/reliable.rs:1-81`

The `ReliableProvider` wraps `Vec<(String, Box<dyn Provider>)>` and implements `Provider`. The design says "update factory to return `Box<dyn ChatProvider>`" (design line 114) but **never mentions `ReliableProvider`** at all.

`create_resilient_provider()` at `src/providers/mod.rs:116-149` returns `Box::new(ReliableProvider::new(...))`. If the factory return type changes to `Box<dyn ChatProvider>`, `ReliableProvider` must also implement `ChatProvider`, wrapping `Vec<(String, Box<dyn ChatProvider>)>` internally.

**Impact:** Won't compile. The agent loop creates its provider via `create_resilient_provider` (`loop_.rs:75-79`).

---

### C2. `ToolSpec` creates cross-module dependency

**Files:** `src/providers/traits.rs`, `src/tools/traits.rs`

The proposed `ChatProvider::chat_completion` signature takes `tools: &[ToolSpec]`, but `ToolSpec` lives in `src/tools/traits.rs:13-18`. Currently `providers` and `tools` modules are completely independent — neither imports the other. This design forces `providers` to depend on `tools`.

**Fix:** Either move `ToolSpec` to a shared `types` module, or define a provider-specific `ToolDefinition` type in `providers::traits` that's convertible from `ToolSpec`.

---

### C3. `all_tools()` signature change breaks all callers

**File:** `src/tools/mod.rs:39-44`

Current signature:
```rust
pub fn all_tools(
    security: &Arc<SecurityPolicy>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
) -> Vec<Box<dyn Tool>>
```

The design proposes adding `AgentManageTool` which needs `Arc<AgentRegistry>` and `Arc<AgentBus>` (design line 491-494). This changes the function signature. All callers must be updated:

- `src/agent/loop_.rs:62` (direct call)
- `src/channels/mod.rs:447-479` (builds tools inline, but would need updating if unified)

The design doesn't mention this signature change.

---

### C4. Daemon has no shutdown broadcast channel

**File:** `src/daemon/mod.rs:11-103`

The design shows `shutdown_tx.subscribe()` for agent runners (design line 566), but `daemon::run()` uses `tokio::signal::ctrl_c().await` directly (daemon/mod.rs:92) with no `tokio::sync::watch` or `broadcast` channel. The design assumes `shutdown_tx` exists — it doesn't.

**Required:** Add a `tokio::sync::watch::Sender<bool>` created at daemon startup, wired into ctrl_c handler, passed to all persistent agent runners.

---

### C5. Anthropic API format incompatibility

**File:** `src/providers/anthropic.rs:1-98`

The design's `ChatMessage` struct (design lines 59-66) uses OpenAI's format:
```rust
pub role: String,           // "system", "user", "assistant", "tool"
pub tool_calls: Option<Vec<ToolCall>>,
pub tool_call_id: Option<String>,
```

But Anthropic's Messages API uses a completely different structure:
- System prompt is a separate field, NOT a message with `role: "system"`
- Tool use returns `content` blocks of type `tool_use`, not a `tool_calls` array
- Tool results are sent as `content` blocks of type `tool_result`, not messages with `role: "tool"`

The current `AnthropicProvider` already handles the system-prompt-as-field pattern correctly (`anthropic.rs:19`, `system: Option<String>`). The design's unified `ChatMessage` format won't work for Anthropic without per-provider message translation.

**Impact:** Anthropic tool calling will fail silently or produce malformed API requests.

---

### C6. `CronJob` struct and SQLite schema incompatible with `CronAction`

**File:** `src/cron/mod.rs:11-19`, `src/cron/mod.rs:258-268`

The design proposes `CronAction` enum (design line 606-609):
```rust
pub enum CronAction {
    Shell(String),
    AgentRun(String),
    AgentMessage(String, String),
}
```

But `CronJob` has `command: String` and the SQLite schema has `command TEXT NOT NULL`. Adding action types requires:
1. Adding `action_type TEXT` column to `cron_jobs` table
2. Migration for existing data
3. Modifying `add_job`, `list_jobs`, `due_jobs`, `reschedule_after_run` — all 4 functions
4. Modifying `run_job_command` in `src/cron/scheduler.rs:136-179`
5. Modifying `CronCommands::Add` in `src/main.rs:193-198` to accept action type

None of this is addressed.

---

### C7. `ollama.rs` provider not listed for `ChatProvider` implementation

**File:** `src/providers/ollama.rs` (exists per glob, referenced in `src/providers/mod.rs:22-24`)

The design lists 4 providers to implement `ChatProvider` (design lines 110-113): openrouter, anthropic, openai, compatible. But `ollama.rs` exists and needs `ChatProvider` too. The factory at `src/providers/mod.rs:22-24` creates Ollama providers.

---

### C8. `run_with_tools` uses wrong provider type

**File:** `src/agent/loop_.rs:75-79`

The design's `run_with_tools` (design line 125-175) calls `provider.chat_completion(...)`, but the provider is created as:
```rust
let provider: Box<dyn Provider> = providers::create_resilient_provider(...)?;
```

`Box<dyn Provider>` doesn't have `chat_completion`. Either the local variable type must change to `Box<dyn ChatProvider>`, or `create_resilient_provider` must be updated (ties back to C1).

---

### C9. Undefined helper functions in `runner.rs`

**Design lines 542-543**

The persistent agent runner calls three functions that are never defined anywhere in the design:
- `run_agent_turn(&definition, &system_prompt, &msg.payload, ...)` — what is this?
- `build_agent_prompt(&definition, &skills)` — how does it differ from `channels::build_system_prompt`?
- `load_agent_skills(&definition.skills, &config.workspace_dir)` — what does this return?

These are non-trivial functions that need full specification.

---

### C10. `description` field in `AgentCommands::Create` is positional + conflicts

**Design lines 280-291**

```rust
Create {
    description: String,      // positional
    #[arg(long)]
    persistent: bool,         // flag
    ...
}
```

With clap, `description: String` is a required positional argument. But the design says (line 433-445):
```rust
if skills.is_some() || persistent {
    // Flag-based creation (power user)
} else {
    // Natural language creation
}
```

This means `description` is always required even in flag-based mode, but in flag-based mode it's used as the agent name. The ambiguity will confuse users.

**Fix:** Make `description` optional and add a `--name` flag, or use a different subcommand for each mode.

---

### C11. Modified files count mismatch

**Design lines 625-654**

The header says "Modified files (8)" but the table lists **14 files**. This is misleading for estimation.

---

### C12. `agents_dir` undefined in `run_persistent_agent`

**Design line 533**

```rust
let memory = memory::create_agent_memory(&config.memory, &agents_dir, &definition.name, ...)?;
```

`agents_dir` is referenced but never declared in the function parameters or local scope. It's not a field on `Config`. Where does it come from?

---

### C13. Agent bus `register` returns receiver but design needs sender reference

**Design lines 364-365**

```rust
pub async fn register(&self, name: &str) -> mpsc::Receiver<AgentMessage>
```

The `register` method creates a channel and returns the receiver. But the sender is stored internally. For the `delegate` action in `AgentManageTool` (design line 484), the tool needs to call `bus.send(msg)` which looks up the sender by name. This works IF the bus is `Arc<AgentBus>`. But the tool's `execute()` method is `async fn execute(&self, args: Value) -> Result<ToolResult>`, and it needs to await a response. The design shows fire-and-forget for delegation (send a message, return success), but the "delegate" action description says "sends task via bus" — how does the tool wait for and return the agent's response? No timeout, no response channel, no correlation ID matching.

---

### C14. `&config` borrowed across `.await` in daemon closure

**Design lines 562-568**

```rust
components.push(tokio::spawn(async move {
    agent::runner::run_persistent_agent(def, &config, bus, shutdown_rx).await
}));
```

`config` is borrowed by reference (`&config`) inside an `async move` block. This won't compile — `config` would need to be cloned and moved. The existing daemon code clones config for each closure (e.g., `daemon/mod.rs:29`: `let gateway_cfg = config.clone()`).

---

## SIGNIFICANT GAPS (compiles but wrong behavior)

### S1. No conversation history in interactive mode

**File:** `src/agent/loop_.rs:162-207`

The existing interactive mode processes messages one at a time. The design's `run_with_tools` (design line 125-175) builds a `Vec<ChatMessage>` for one exchange but doesn't show how conversation history persists across interactive mode iterations. Each user message would start a fresh conversation.

**Required:** The outer interactive loop (`while let Some(msg) = rx.recv().await`) must maintain a persistent `Vec<ChatMessage>` that grows with each turn.

---

### S2. `CompositeMemory` undefined for `shared-read` mode

**Design line 718**

The design mentions `CompositeMemory` in `src/memory/composite.rs` but doesn't define it. The `Memory` trait has 7 methods (`store`, `recall`, `get`, `list`, `forget`, `count`, `health_check`). Questions not answered:
- `list()` — list from both sources? How to merge/deduplicate?
- `forget()` — can you delete from the read-only source? Probably not, but then what does `forget` return?
- `count()` — sum of both? Only write target?
- `recall()` — merge results from both and re-rank? How?

---

### S3. Memory hygiene runs on every agent memory creation

**File:** `src/memory/mod.rs:26-28`

`create_memory()` calls `hygiene::run_if_due(config, workspace_dir)` at the start. The proposed `create_agent_memory()` (design line 517) delegates to `create_memory(config, &agent_dir, api_key)`, which means hygiene runs on every agent's memory directory individually. With 10 agents, that's 10 hygiene passes on startup. The hygiene pass may be throttled by a state file, but the state file path is relative to the passed `workspace_dir`, so each agent gets its own throttle — none will be skipped.

---

### S4. Security isolation not addressed

**Files:** `src/security/` module, `src/tools/mod.rs:30-36`

The design gives all agents access to all tools (shell, file_read, file_write). A "twitter agent" with shell access could execute arbitrary commands. The `SecurityPolicy` (referenced in `loop_.rs:43-46`) is workspace-scoped and applies the same rules to all agents. No per-agent tool filtering, no per-agent security policy.

**Missing:** An `allowed_tools: Vec<String>` field on `AgentDefinition` and filtering logic in the agent runner.

---

### S5. No enforcement of `delegates_to` or `channels` fields

**Design lines 203-205**

`AgentDefinition` has `delegates_to: Vec<String>` and `channels: Vec<String>`, but:
- The message bus `send()` accepts any `to` target (design line 367). No validation against `delegates_to`.
- No channel routing logic binds agents to specific channels. The current channel architecture uses a single message bus (`channels/mod.rs:578`).

These fields would be parsed from definitions but never enforced.

---

### S6. `max_tools_per_turn` ignored in tool loop

**Design line 211 (definition)** vs **design line 144 (loop)**

The definition has `max_tools_per_turn: usize` but the tool calling loop uses hardcoded `for _ in 0..20`. No integration between these.

---

### S7. Open Question #3 contradicts bus design

**Design line 727** says "Only through main for now" for agent-to-agent communication.
**Design lines 360-368** — the `AgentBus::send()` method routes to any agent by name with no main-agent gateway or routing restriction. These are contradictory.

---

### S8. `agents_dir` location inconsistency

Multiple references to where agents live:
- Design line 248: `~/.zeroclaw/agents/<name>.md`
- Design line 257: `AgentRegistry { agents_dir: PathBuf }` (no path specified)
- Design line 558: `AgentRegistry::new(&config.workspace_dir)` — workspace is `~/.zeroclaw/workspace/`
- Design line 515: `agents_dir.join(agent_name)` (undefined)

Is it `~/.zeroclaw/agents/`, `~/.zeroclaw/workspace/agents/`, or a separate top-level dir? This affects file paths, memory paths, and the registry.

---

### S9. No LLM output validation in agent generator

**Design lines 406-428**

`generate_definition()` calls the LLM and parses the response as `AgentDefinition`. LLM outputs are inherently unreliable — the YAML might be malformed, field values might be nonsensical, skill names might not exist. No validation, retry, or sanitization is designed.

---

### S10. Tool descriptions hardcoded in two places

**Files:** `src/agent/loop_.rs:88-113` and `src/channels/mod.rs:447-479`

Tool descriptions are duplicated as hardcoded string literals. The design's `run_with_tools` adds a third location. These should be derived from `tool.spec()` (which already exists on the `Tool` trait) rather than manually maintained.

---

### S11. Delegation response flow not designed

**Design line 484**

The `delegate` action in `AgentManageTool` sends a message via bus. But:
- How does the tool wait for a response? `execute()` must return a `ToolResult`.
- The bus is fire-and-forget (`send` returns `Result<()>`).
- There's no response channel, no correlation via `trace_id`, no timeout.
- The design shows the main agent relaying results (line 682-683) but doesn't show how the tool blocks on a response.

**Required:** Either a synchronous request-response pattern (oneshot channel per delegation) or a two-phase approach where the tool returns "task delegated" and the response comes later.

---

### S12. Existing `channels` field in `AgentDefinition` conflicts with existing `ChannelsConfig`

**Design line 203**

`AgentDefinition.channels: Vec<String>` — these are channel names like "telegram", "discord". But the current channel architecture doesn't support per-agent channel routing. `start_channels()` at `channels/mod.rs:425-658` routes ALL messages to a single LLM call. There's no mechanism to route a Telegram message to a specific agent.

---

## MINOR ISSUES

### M1. CLI naming convention

The design uses `Agents` (plural) as the `Commands` variant (design line 312). The existing pattern uses mixed: `Channel` (singular), `Skills` (plural), `Cron` (singular). The most Rust-idiomatic pattern for clap is singular. Consider `Agent` with subcommand.

---

### M2. WhatsApp missing from `has_supervised_channels`

**File:** `src/daemon/mod.rs:204-210`

Existing bug not flagged by design: `has_supervised_channels()` checks telegram, discord, slack, imessage, matrix — but **not whatsapp**. WhatsApp channels won't start the channel supervisor in daemon mode.

---

### M3. No streaming support mentioned

The tool calling loop prints the final response. For multi-turn conversations with long tool chains, streaming intermediate text would significantly improve UX. Not a blocker but worth noting for Phase 1.

---

### M4. `provider_override` and `model_override` naming

**File:** `src/agent/loop_.rs:32-37`

The existing `run()` uses `provider_override` / `model_override` params. The design's `run_with_tools` (line 126-130) uses the same names. Consistent, but if `run_with_tools` replaces `run`, the CLI argument mapping in `main.rs:301-306` needs updating.

---

### M5. `ChatMessage` default struct syntax incomplete

**Design line 139-140, 167-172**

The design shows `ChatMessage { role: "system".into(), content: Some(system_prompt), .. }` — the `..` syntax requires a `Default` implementation for `ChatMessage`, which isn't derived or implemented in the design.

---

### M6. `SkillIndexEntry` type undefined

**Design line 386-389**

`rebuild_index` uses `SkillIndexEntry` but this struct is never defined in the design. What fields does it have? The design shows `name`, `description`, `tools` but no formal struct definition.

---

### M7. Heartbeat worker calls `agent::run` not `run_with_tools`

**File:** `src/daemon/mod.rs:193`

The heartbeat worker calls `crate::agent::run(config.clone(), Some(prompt), None, None, temp)`. If `run_with_tools` replaces `run`, this call site needs updating. The design only mentions updating `main.rs` (design line 178).

---

### M8. No tests specified for any new code

The design proposes 8 new files and 14 modified files but includes zero test specifications. The existing codebase has extensive tests (every module has `#[cfg(test)]` blocks). The new code should follow the same pattern.

---

## DEPENDENCY CHAIN ISSUES

The design proposes 7 phases but has hidden dependencies that affect ordering:

1. **Phase 1 (tool calling) requires Provider refactor** — but `ReliableProvider` refactor is not in Phase 1
2. **Phase 3 (bus) needed by Phase 4** (agent management tool uses bus) — correct ordering
3. **Phase 5 (memory) needed by Phase 5c** (daemon integration) — but Phase 5c also needs Phase 3 (bus) and Phase 2 (registry)
4. **Phase 7 (cron) requires Phase 3** (bus for agent messages) — but also requires schema migration not mentioned anywhere

**Recommended insertion:** Add "Phase 0: Shared types extraction" to move `ToolSpec` and message types to a common module, and refactor `ReliableProvider` to be generic.

---

## SUMMARY TABLE

| # | Severity | Phase | Issue |
|---|----------|-------|-------|
| C1 | Critical | 1 | `ReliableProvider` needs `ChatProvider` impl |
| C2 | Critical | 1 | `ToolSpec` cross-module dependency |
| C3 | Critical | 4 | `all_tools()` signature change not addressed |
| C4 | Critical | 5 | Daemon has no shutdown broadcast channel |
| C5 | Critical | 1 | Anthropic API format incompatible with `ChatMessage` |
| C6 | Critical | 7 | `CronJob` schema incompatible with `CronAction` |
| C7 | Critical | 1 | `ollama.rs` missing from `ChatProvider` list |
| C8 | Critical | 1 | `run_with_tools` uses wrong provider type |
| C9 | Critical | 5 | Undefined helper functions in runner |
| C10 | Critical | 2 | CLI positional arg ambiguity |
| C11 | Critical | — | Modified files count wrong (says 8, lists 14) |
| C12 | Critical | 5 | `agents_dir` undefined in runner scope |
| C13 | Critical | 3/4 | Delegation response flow missing |
| C14 | Critical | 5 | `&config` borrow across await won't compile |
| S1 | Significant | 1 | No conversation history persistence |
| S2 | Significant | 5 | `CompositeMemory` undefined |
| S3 | Significant | 5 | Memory hygiene runs N times for N agents |
| S4 | Significant | 5 | No per-agent security isolation |
| S5 | Significant | 2 | `delegates_to`/`channels` fields not enforced |
| S6 | Significant | 1/2 | `max_tools_per_turn` not wired |
| S7 | Significant | 3 | Open question contradicts implementation |
| S8 | Significant | 2/5 | `agents_dir` location inconsistent |
| S9 | Significant | 4 | No LLM output validation |
| S10 | Significant | 1 | Tool descriptions hardcoded 3x |
| S11 | Significant | 4 | Delegation awaits response with no mechanism |
| S12 | Significant | 2 | Per-agent channel routing nonexistent |
| M1 | Minor | 2 | CLI naming convention |
| M2 | Minor | — | WhatsApp missing from daemon supervisor check |
| M3 | Minor | 1 | No streaming support |
| M4 | Minor | 1 | Parameter naming when replacing `run()` |
| M5 | Minor | 1 | `ChatMessage` needs `Default` derive |
| M6 | Minor | 4 | `SkillIndexEntry` type undefined |
| M7 | Minor | 1 | Heartbeat worker calls old `run()` |
| M8 | Minor | all | No test specifications |
