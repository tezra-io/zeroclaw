# Upstream Merge Plan — TEZ-15

*Merging 139 commits from theonlyhennygod/zeroclaw into our fork*
*Created: 2026-02-16*

---

## Problem

Upstream zeroclaw has diverged significantly from our fork. A naive `git merge` produces 11 file conflicts and 33+ compilation errors because both sides modified core types and interfaces.

## Key Architectural Differences

### Provider Trait System

**Our fork (HEAD):**
- Kept original `Provider` trait (returns `String`)
- Added separate `ChatProvider` trait (returns `ChatResponse` with structured tool calls)
- `ChatProvider` implemented on all providers alongside `Provider`
- `ChatResponse` defined in `src/types.rs` with `MessageRole`, `ChatMessage`, `ToolSpec`, `ToolCall`

**Upstream:**
- Extended `Provider` trait with `chat_with_history()` method
- Added `ChatResponse` struct in `src/providers/traits.rs` (text + tool_calls)
- Added `ChatMessage`, `ToolCall`, `ConversationMessage`, `ToolResultMessage` in traits.rs
- NO separate `ChatProvider` trait — everything is on `Provider`
- `ChatMessage` has `role: String` + `content: String` (simpler than ours)

**Resolution strategy:** Adopt upstream's `Provider` trait extensions AND keep our `ChatProvider` trait. Our `ChatProvider` becomes the structured API for multi-agent tool calling. Upstream's `chat_with_history()` serves legacy/simple paths. Reconcile type definitions — align `ChatMessage` and `ChatResponse` between both.

### Agent Loop

**Our fork:**
- `run()` — original simple chat loop
- `run_with_tools()` — our addition with tool calling, max 20 iterations

**Upstream:**
- `run()` — completely rewritten with full tool execution loop, conversation history, memory auto-save, observer integration, `agent_turn()` helper, history trimming (max 50 messages), 851 lines
- `MAX_TOOL_ITERATIONS = 10`, `MAX_HISTORY_MESSAGES = 50`

**Resolution strategy:** Adopt upstream's `run()` entirely (it's more mature). Port our `run_with_tools()` additions into upstream's pattern — specifically the multi-agent-aware tool execution and `ChatProvider`-based calling. Our `run_with_tools()` can be refactored to wrap upstream's `agent_turn()`.

### Tools

**Our fork:**
- `all_tools(security, memory, composio_key, browser_config)` — 4 args
- Added `agent_manage.rs` and `all_tools_with_agents()` with registry + bus

**Upstream:**
- `all_tools(security, memory, composio_key, browser_config, agents, fallback_api_key)` — 6 args
- Added `delegate.rs`, `screenshot.rs`, `image_info.rs`
- Added `all_tools_with_runtime()` for pluggable runtime adapter
- Added `DelegateAgentConfig` for agent-to-agent delegation

**Resolution strategy:** Adopt upstream's `all_tools()` signature (6 args). Merge our `agent_manage.rs` + `all_tools_with_agents()` on top. Their `DelegateTool` is config-driven delegation; our `AgentManageTool` is LLM-driven creation/management — they're complementary, not conflicting.

### Main.rs / CLI

**Our fork:**
- Added `AgentSubCommands` enum and `Commands::Agent` subcommands
- Agent management CLI (list, create, edit, remove, status, skill-add, skill-remove)

**Upstream:**
- Added `DelegationConfig`, identity support, new channel wiring
- Changed `Commands::Agent` struct slightly

**Resolution strategy:** Keep our `AgentSubCommands` additions. Incorporate upstream's new config fields and wiring.

---

## Implementation Steps (for Claude Code)

### Step 1: Preparation
```bash
cd /Users/sujshe/projects/zeroclaw
git checkout main
git fetch upstream
```

### Step 2: Start merge
```bash
git merge upstream/main
# Conflicts will appear — that's expected
```

### Step 3: Resolve conflicts file by file

#### 3a. Cargo.lock
```bash
rm Cargo.lock
# Will regenerate after all source conflicts resolved
```

#### 3b. src/providers/traits.rs (auto-merged, but verify)
- Upstream added `ChatMessage`, `ChatResponse`, `ToolCall`, `ConversationMessage`, `ToolResultMessage`, `chat_with_history()`, `warm_up()`
- Our `src/types.rs` has overlapping types — need to reconcile
- **Action:** Keep upstream's traits.rs. Update our code to use upstream's `ChatMessage` (role: String) OR add conversion functions between our `ChatMessage` (role: MessageRole enum) and theirs.

#### 3c. src/providers/anthropic.rs
- Both sides modified: we added `apply_auth()` + `ChatProvider` impl, upstream added `chat_with_history()`, `warm_up()`, tool call parsing
- **Action:** Keep ALL from both sides. Our `ChatProvider` impl is separate from their `Provider` impl changes. `apply_auth()` stays.

#### 3d. src/providers/compatible.rs
- Upstream heavily refactored — added response parsing, tool call support, multi-model router
- We added `ChatProvider` impl
- **Action:** Accept upstream's refactor. Re-add our `ChatProvider` impl on top, adapted to their new structure.

#### 3e. src/providers/mod.rs
- Upstream added new providers (copilot, bedrock, custom prefixes), `create_resilient_provider` changes
- We added `create_resilient_chat_provider()`
- **Action:** Accept upstream's new providers. Keep our `create_resilient_chat_provider()` function.

#### 3f. src/providers/openrouter.rs
- Upstream added `warm_up()`, `chat_with_history()`
- We added `ChatProvider` impl
- **Action:** Keep both.

#### 3g. src/providers/reliable.rs
- Upstream added `ReliableProvider` with `chat_with_history()` support
- We added `ReliableChatProvider` wrapper
- **Action:** Keep both. They're parallel wrappers for different trait hierarchies.

#### 3h. src/agent/loop_.rs
- Upstream completely rewrote this (851 lines with full tool loop)
- We added `run_with_tools()` (our tool loop)
- **Action:** Accept upstream's `run()`. Keep our `run_with_tools()` but refactor it to:
  1. Use upstream's `ChatMessage` type
  2. Call our `ChatProvider` for structured responses
  3. Integrate with upstream's history management pattern

#### 3i. src/main.rs
- Upstream changed config wiring, added identity, delegation config
- We added `AgentSubCommands`
- **Action:** Accept upstream's changes. Re-add our `AgentSubCommands` enum and match arm.

#### 3j. src/daemon/mod.rs
- Upstream added new channel wiring (email, whatsapp), heartbeat changes
- **Action:** Accept upstream's version. No multi-agent-specific code here.

#### 3k. src/lib.rs
- Upstream added `runtime` module, `util` module
- **Action:** Accept upstream's version. Add back our module exports if needed.

#### 3l. src/tools/mod.rs
- Upstream's `all_tools()` now takes 6 args + has `all_tools_with_runtime()`
- We added `agent_manage` module + `all_tools_with_agents()`
- **Action:** Accept upstream's `all_tools()` signature. Update our `all_tools_with_agents()` to match the new 6-arg pattern and add our `AgentManageTool` on top.

### Step 4: Type reconciliation
Our `src/types.rs` defines `ChatMessage`, `MessageRole`, `ChatResponse`, `ToolSpec`, `ToolCall`.
Upstream's `src/providers/traits.rs` defines `ChatMessage`, `ChatResponse`, `ToolCall`.

**Options:**
A. Delete our `types.rs`, use upstream's types everywhere, adapt our ChatProvider
B. Keep our `types.rs` as the "rich" types, add `From` conversions to/from upstream's types
C. Merge both into traits.rs

**Recommended: Option B** — Keep our types.rs as the multi-agent-aware types. Add `impl From<providers::ChatMessage> for types::ChatMessage` and vice versa. This minimizes changes to our existing Phase 0-4 code.

### Step 5: Build and fix
```bash
cargo generate-lockfile
cargo check 2>&1 | head -50
# Fix compilation errors iteratively
cargo test --lib
cargo clippy --all-targets
```

### Step 6: Commit and push
```bash
git add -A
git commit -m "merge: absorb 139 upstream commits

New from upstream:
- WhatsApp + Email channel integrations
- Agent delegation (DelegateTool)
- Multi-turn conversation history + tool execution
- Screenshot + image_info vision tools
- SkillForge automated skill discovery
- GitHub Copilot provider
- Multi-model router for task-based routing
- OpenTelemetry tracing + metrics
- Bearer auth for Gemini OAuth tokens
- Discord DM support + message splitting
- Various bug fixes (SSRF, symlinks, retry logic, file limits)

Preserved from our fork:
- ChatProvider trait + impls on all providers
- Agent definitions, registry, CLI commands (Phase 2)
- Inter-agent message bus (Phase 3)
- AI-powered agent creation + management tool (Phase 4)
- OAuth token support for Anthropic
- SkillIndexEntry for agent matching"

git push origin main
```

---

## Estimated Effort
- Conflict resolution: ~1 hour
- Type reconciliation + compilation fixes: ~2 hours
- Test fixes: ~30 minutes
- **Total: ~3.5 hours of focused Claude Code work**

## Risk
- Upstream's tool execution loop may behave differently than ours — test both paths
- Type mismatches between our ChatMessage and upstream's may cascade
- New upstream deps may conflict with our serde_yaml addition

---

## Files Changed by Upstream (for reference)

### New files upstream added:
- src/channels/email.rs, whatsapp.rs
- src/identity/mod.rs (AIEOS identity)
- src/observability/mod.rs (OpenTelemetry)
- src/providers/copilot.rs (GitHub Copilot)
- src/providers/router.rs (multi-model router)
- src/runtime/mod.rs (runtime adapter trait)
- src/skills/forge.rs (SkillForge)
- src/tools/delegate.rs, screenshot.rs, image_info.rs
- src/util.rs (string utilities)

### Modified files with our conflicts:
See Step 3 above for each file's resolution strategy.
