# Upstream Merge Plan — TEZ-15

*Selective cherry-pick of high-impact upstream commits*
*Created: 2026-02-16, Updated: 2026-02-16*

---

## Strategy

**NO bulk merge.** Cherry-pick only commits with significant impact: security fixes, core bug fixes, and improvements to features we use (providers, gateway, agent loop, tools, Telegram, Discord). Skip new channels we don't need, CI/Docker changes, and cosmetic stuff.

---

## MUST HAVE — Security Fixes

| Commit | Description |
|--------|-------------|
| `2ac571f` | fix: harden private host detection against SSRF bypass |
| `1e21c24` | fix: harden private host detection against SSRF bypass (#133) |
| `031683a` | fix(security): use path-component matching for forbidden paths |
| `73ced20` | fix(tools): check for symlinks before writing and reorder mkdir |
| `b722189` | fix: clear env vars in shell tool to prevent secret leakage |
| `641a5bf` | fix(skills): prevent path traversal in skill remove command |
| `6776373` | fix: constant_time_eq no longer leaks secret length |
| `b3bfbaf` | fix: store bearer tokens as SHA-256 hashes |
| `1f92470` | fix: replace UUID v4 key generation with direct CSPRNG |
| `6725eb2` | fix(gateway): use constant-time comparison for WhatsApp verify_token |
| `7c3f2f5` | fix(imessage): replace sqlite CLI path with rusqlite parameterized reads |
| `da453f0` | fix: prevent panics from byte-level string slicing on multi-byte UTF-8 |
| `7b5e77f` | fix: use safe Unicode string truncation to prevent panics (CWE-119) |
| `9aaa5bf` | fix: use safe Unicode string truncation (duplicate/related) |
| `80c599f` | fix: correct truncate_with_ellipsis to trim trailing whitespace |

## MUST HAVE — Core Bug Fixes

| Commit | Description |
|--------|-------------|
| `e04e719` | fix(agent): robust tool-call parsing for noisy model outputs |
| `b442a07` | fix(memory): prevent autosave key collisions across runtime flows |
| `9639446` | fix(memory): prevent autosave overwrite collisions |
| `8694c2e` | fix(providers): skip retries on non-retryable HTTP errors (4xx) |
| `64a64cc` | fix: ollama provider ignores api_key parameter to prevent builder error |
| `128b30c` | fix: install default Rustls crypto provider to prevent TLS error |
| `722c996` | fix(daemon): reset supervisor backoff after successful component run |
| `ef00cc9` | fix(channels): check response status in send() for Telegram, Slack, Discord |
| `1e19b12` | fix(providers): warn on shared API key for fallbacks + warm up providers |
| `1110158` | fix: propagate warmup errors, skip when no API key |
| `cc13fec` | fix: add provider warmup to prevent cold-start timeout |
| `efe7ae5` | fix: use UTF-8 safe truncation in bootstrap file preview |
| `a310e17` | fix: add missing port/host fields to GatewayConfig + apply_env_overrides |
| `dca95ca` | fix: add channel message timeouts, Telegram fallback, fix tests |

## SHOULD HAVE — Feature Improvements to Existing Stuff

| Commit | Description |
|--------|-------------|
| `89b1ec6` | feat: add multi-turn conversation history and tool execution |
| `7456692` | fix: pass OpenAI-style tool_calls from provider to parser |
| `c8ca6ff` | feat: agent-to-agent handoff and delegation |
| `9b2f900` | feat: add screenshot and image_info vision tools |
| `322f24f` | fix(tools): add 10 MB file size limit to file_read tool |
| `b0e1e32` | feat(config): make config writes atomic with rollback-safe replacement |
| `9e55ab0` | feat(gateway): add per-endpoint rate limiting and webhook idempotency |
| `91e17df` | feat(security): shell risk classification, approval gates, throttling |
| `49bb20f` | fix(providers): use Bearer auth for Gemini CLI OAuth tokens |
| `f8aef8b` | feat: add anthropic-custom prefix for Anthropic-compatible endpoints |
| `be135e0` | feat: add Anthropic setup-token flow |
| `1cfc638` | feat(providers): add multi-model router for task-based routing |
| `1eadd88` | feat: Support Responses API fallback for OpenAI-compatible providers |
| `3b7a140` | feat(telegram): add typing indicator when receiving messages |
| `021d03e` | fix(discord): add DIRECT_MESSAGES intent to enable DM support |
| `a04716d` | fix: split Discord messages over 4000 characters |
| `03c3ded` | fix(discord): enforce 2000-character message chunks |
| `2f78c5e` | feat(channel): add typing indicator for Discord |
| `a5241f3` | fix(discord): track gateway sequence number and handle reconnect |
| `8a30450` | fix: apply TimeoutLayer to gateway router for request timeouts |
| `35b63d6` | feat: SkillForge — automated skill discovery & integration |
| `b8c6937` | feat(agent): wire Composio tool into LLM tool descriptions |
| `3bb5def` | feat: add Google Gemini provider with CLI token reuse |
| `6899ad4` | feat: add GitHub Copilot as a provider |

## SKIP — Don't Need These

| Commit(s) | Why Skip |
|-----------|----------|
| WhatsApp + Email channels (`dc215c6`, `ced4d70`, `cc2f850`, etc.) | New channels we don't use |
| IRC channel (`b208cc9`) | Don't need |
| Lark/Feishu (`0e0b364`) | Don't need |
| Docker/CI changes (~20 commits) | We build locally, not Docker |
| AIEOS identity (`f1e3b11`) | Don't need identity system |
| OpenTelemetry (`0f6648c`) | Overkill for us right now |
| Docker runtime (`be6474b`, `b462fa0`) | We run native |
| Dev container (`20f857a`) | We develop locally |
| Docs/README cosmetic changes | No functional impact |
| Merge commits | No content |
| GLM/Z.AI/MiniMax endpoint fixes | Providers we don't use |
| Windows-specific fixes | We're on macOS |
| Benchmark docs | Just docs |

---

## Implementation Approach

### Option A: Cherry-pick groups (Recommended)
Cherry-pick in logical groups to minimize conflicts:

1. **Security batch** — all security fixes first (15 commits)
2. **Core fixes batch** — bug fixes (14 commits)  
3. **Provider improvements** — new providers + improvements (8 commits)
4. **Agent/tool improvements** — tool calling, delegation, vision tools (6 commits)
5. **Channel fixes** — Telegram/Discord improvements only (6 commits)
6. **Gateway/config** — rate limiting, atomic config, timeouts (3 commits)

For each batch: cherry-pick, resolve conflicts, `cargo check`, fix, continue.

### Option B: Selective merge with file filter
```bash
# Merge but only accept changes to specific paths
git merge upstream/main --no-commit
git checkout HEAD -- src/channels/email.rs src/channels/whatsapp.rs ...  # revert unwanted
```

### Recommended: Option A
Cherry-picking gives us full control over what lands. More work upfront but cleaner result and no unwanted code.

---

## Conflict Risk Assessment

**High risk (will conflict with our code):**
- `src/providers/traits.rs` — upstream added types that overlap with our `types.rs`
- `src/providers/*.rs` — upstream added `chat_with_history()`, we added `ChatProvider`
- `src/agent/loop_.rs` — upstream rewrote entirely, we added `run_with_tools()`
- `src/tools/mod.rs` — upstream changed `all_tools()` signature
- `src/main.rs` — both sides added to Commands enum

**Low risk (no overlap):**
- Security fixes in `src/security/`, `src/tools/file_write.rs`, `src/tools/shell.rs`
- New tool files (`screenshot.rs`, `image_info.rs`, `delegate.rs`)
- Config changes in `src/config/schema.rs`
- Channel fixes in `src/channels/telegram.rs`, `src/channels/discord.rs`

**Strategy for high-risk files:**
Apply the security/bug fix commits first (low conflict). Then tackle the structural changes (providers, agent loop) as a focused session where we adapt our code to coexist with upstream's additions.

---

## Estimated Effort
- Security + core fixes cherry-pick: ~1 hour
- Provider/agent structural changes: ~2-3 hours  
- Testing + cleanup: ~30 minutes
- **Total: ~4 hours of Claude Code work, split across 2 sessions**
