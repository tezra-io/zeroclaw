# Phase 0+1 Security Review

**Scope:** `git diff 61c36a8..20de485` — shared types (`src/types.rs`), tool calling loop (`src/agent/loop_.rs`), provider ChatProvider implementations, OpenAI format layer, reliable provider wrapper.

**Reviewer:** Automated security review  
**Date:** 2025-02-15

---

## Findings

### 1. [HIGH] No per-tool authorization in the tool calling loop

**File:** `src/agent/loop_.rs` — `run_tool_loop()`

The tool loop executes any tool the LLM requests by name, as long as it exists in the `registered_tools` vec. There is no secondary authorization check at the loop level — the LLM can invoke `shell` with arbitrary commands in every iteration.

While `ShellTool` has its own `SecurityPolicy` check (command allowlist), the loop itself trusts tool dispatch entirely. If a new tool is added without internal guards, or if the shell allowlist is misconfigured, the LLM has direct shell access.

**Risk:** An LLM responding to a prompt injection in user content could chain tool calls (e.g., `shell` → exfiltrate data) within the 20-iteration budget.

**Recommendation:**
- Add a loop-level tool authorization hook (e.g., `security.authorize_tool_call(name, args)`) before execution.
- Consider a "dangerous tool" tier requiring explicit user confirmation.

---

### 2. [HIGH] Unbounded conversation history growth (memory exhaustion)

**File:** `src/agent/loop_.rs` — interactive mode in `run_with_tools()`

In interactive mode, `messages: Vec<ChatMessage>` grows indefinitely across turns. Each turn adds user message + assistant response + all tool call/result pairs. With the tool loop doing up to 20 iterations per turn, a single turn can add ~41 messages. Over a long session this will:
- Exhaust memory
- Exceed provider context windows (causing API errors or silent truncation)

**Recommendation:** Implement a sliding window or summarization strategy. At minimum, cap `messages.len()` and trim oldest messages.

---

### 3. [MEDIUM] Max iterations hardcoded at 20 — no per-session or per-user control

**File:** `src/agent/loop_.rs` — `run_with_tools()` calls `run_tool_loop(..., 20)`

The iteration cap of 20 is hardcoded. While it prevents infinite loops, 20 iterations × multiple tool calls per iteration is generous. A compromised or jailbroken LLM could execute up to 20 shell commands in a single user message.

**Recommendation:**
- Make `max_iterations` configurable via `Config`.
- Consider a separate `max_tool_calls_total` counter (not just iterations).
- Log/alert when max iterations are reached — it likely indicates anomalous behavior.

---

### 4. [MEDIUM] Malformed JSON arguments silently default to empty object

**File:** `src/agent/loop_.rs` — tool execution block

```rust
let args: serde_json::Value = serde_json::from_str(&tool_call.arguments)
    .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
```

If the LLM returns malformed JSON in `tool_call.arguments`, it silently becomes `{}`. This means:
- Tools receive empty args and may behave unexpectedly
- The error is hidden from both the LLM and observability
- A tool that interprets missing args as "use defaults" could perform unintended actions

**Recommendation:** On parse failure, return a `ToolResult` error to the LLM instead of silently defaulting. This lets the LLM self-correct.

---

### 5. [MEDIUM] Tool result output not size-bounded before feeding back to LLM

**File:** `src/agent/loop_.rs` — tool result message construction

Tool results (`result.output`) are pushed into `messages` without size limits. A `shell` command producing megabytes of output (e.g., `cat /dev/urandom | base64 | head -c 10000000`) would be fed back verbatim, potentially:
- Exceeding context windows
- Causing provider API errors
- Inflating token costs

Note: `ShellTool` has `MAX_OUTPUT_BYTES = 1MB` internally, but this isn't enforced at the loop level for all tools.

**Recommendation:** Truncate `result.output` at the loop level (e.g., 100KB) with a `[truncated]` marker.

---

### 6. [MEDIUM] Anthropic translation: System messages silently become user messages

**File:** `src/providers/anthropic.rs` — `to_anthropic_messages()`

```rust
MessageRole::System => {
    // System messages are handled via the top-level `system` field.
    // If one sneaks in here, treat as user message.
    if let Some(ref content) = msg.content {
        result.push(AnthropicMessage {
            role: "user".into(),
            content: AnthropicContent::Text(content.clone()),
        });
    }
}
```

If a system message ends up in the message list (e.g., through a bug or injection), it gets silently promoted to a user message. This could be exploited for prompt injection — an attacker who can inject a `System` role message into the conversation gets it treated as user input to Anthropic.

**Recommendation:** Either drop system messages in the message array (they should only be in the top-level field) or return an error.

---

### 7. [LOW] API keys passed through provider chain without redaction in error messages

**Files:** All provider implementations

API errors are propagated via `anyhow::bail!("Provider API error: {error}")` where `error` is the raw response body. Some providers echo request headers (including auth tokens) in error responses. These errors may be logged or displayed to users.

**Recommendation:** Sanitize error response bodies before including in error messages. At minimum, scan for and redact strings matching API key patterns.

---

### 8. [LOW] No timeout on individual tool executions at the loop level

**File:** `src/agent/loop_.rs` — `run_tool_loop()`

Tool execution (`t.execute(args).await`) has no timeout wrapper. While `ShellTool` has its own 60s timeout, other tools (e.g., `BrowserTool`, `ComposioTool`) may not. A hung tool blocks the entire loop indefinitely.

**Recommendation:** Wrap each `t.execute(args)` in `tokio::time::timeout()` at the loop level.

---

### 9. [LOW] `unwrap_or_else(std::sync::PoisonError::into_inner)` in ActionTracker

**File:** `src/security/policy.rs` (pre-existing, but used by new code paths)

Mutex poison recovery silently continues after a panic. This is intentional (and common in Rust), but worth noting: if a tool execution panics while holding the action tracker lock, the rate limiter continues with potentially corrupted state.

**Recommendation:** Acceptable for now; document the design decision.

---

### 10. [INFO] No `unsafe` blocks in new code

All new code is safe Rust. No `unsafe` blocks, no raw pointer manipulation. Memory safety is enforced by the compiler.

---

### 11. [INFO] Good: Tool calls only dispatch to registered tools

The loop correctly handles unknown tools by returning an error result to the LLM:
```rust
None => ToolResult {
    success: false,
    output: format!("Unknown tool: {}", tool_call.name),
    error: None,
}
```

This prevents the LLM from invoking arbitrary functions. The attack surface is limited to the registered tool set.

---

### 12. [INFO] Good: Tests cover loop termination and max iterations

The test suite includes:
- Loop exits on plain text response
- Loop executes tools and feeds back results
- Unknown tool handling
- Max iteration cap enforcement

---

## Summary

| Severity | Count | Key Themes |
|----------|-------|------------|
| CRITICAL | 0 | — |
| HIGH | 2 | No loop-level tool auth; unbounded history growth |
| MEDIUM | 4 | Hardcoded iteration cap; silent JSON defaults; unbounded tool output; system→user promotion |
| LOW | 3 | Error message leaks; no per-tool timeout; mutex poison |
| INFO | 3 | No unsafe; good dispatch safety; good test coverage |

**Overall assessment:** The foundation is solid — safe Rust, proper error handling, tool dispatch restricted to registered set, iteration cap in place. The main concerns are the lack of a secondary authorization layer in the tool loop (relying entirely on individual tool guards) and the silent handling of malformed inputs. These should be addressed before production use.
