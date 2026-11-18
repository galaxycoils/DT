# Changelog

All notable changes to **Grok-Muse** are documented in this file.

## [v0.7.0] — UI and Provider Connections (2026-11-17)

- Shared Rakazo design tokens, remembered single sidebar, message previews, measured transcript rows, pill composer, Pages/Activity navigation, and provider settings across native and browser interfaces.
- Daemon-owned ChatGPT registration/callback, OIDC validation, Nous device authorization, token renewal, cancellation, account switching, and protected credential destinations.
- Real catalogs and automatic, budgeted model reply checks; separate credential, catalog, and inference states; live provider routing and durable credential errors.
- Relocated app bundles prefer their own daemon/helper instead of an older installed copy.
- Release-profile, ad-hoc Apple Silicon preview. Real cloud account checks and full native certification remain pending; see the release notes.

## [v0.6.1] — Apple Silicon Preview (2026-11-17)

- Isolate OAuth fixtures from OS credential stores and environment secrets.
- Keep Unix service-path assertions on Unix test hosts.
- Add explicit release-profile DMG packaging and tag-time checksum verification.
- Synchronize package versions and clarify platform certification and preview installation status.
- Publish an ad-hoc Apple Silicon prerelease with a checksum matching its download filename.

## [v0.6.0] — Full Rakazo Parity & Multi-Provider Suite (2026-11-17)

### Added & Fixed
- **Privacy, Data-Loss & Security Fixes (`discretion.rs`, `forget.rs`, `goal_workspace.rs`, `enable_skills`)**: Enforced deny-wins rule precedence on `DISCRETION.md` evaluation, JSON-aware Two-Phase Forget across `goals.json`, `SharedFileStore` mutex serialization on `goals.json` mutations, strict `muse.enable_skills` gating across agent/API/IPC, and `UPSTREAM.md` sync for `zod` `4.6.6`.
- **Multi-Provider Stack (`crates/gm-llm`, `crates/gm-core`, `crates/gm-ui`, `crates/gm-daemon`)**: Added ChatGPT PKCE OAuth & `/v1/responses` SSE streaming, Kilo Gateway, OpenRouter, Nous Portal OAuth, Custom Chat Completions/Responses endpoints, 5-tier precedence (`Routine -> Bot -> Workspace -> Account -> Legacy`), fail-in-place behavior, `/models` discovery, separate ChatGPT included-allowance accounting, and endpoint origin-change revalidation (`NeedsRevalidation`).
- **Relay, Bridges, Honest Probing & Voice/Meetings (`crates/gm-openbot`)**: Outbound-only HTTPS webhook relay, persisted `bridge_channel_mappings_v1` & deduplication, honest capability probing (`connectors.rs`), local Whisper/TTS (`voice.rs`), Asterisk ARI SIP (`sip_asterisk.rs` with Tier-3 carrier-cost gate), Zoom RTMS (`meetings_zoom.rs`), and Google Meet REST/Events (`meetings_google.rs`).
- **Hosted `/api/v1`, Marketplace, Stripe Sandbox & Web/Mobile Clients (`clients/web/`, `clients/mobile/`)**: Scoped `/api/v1/workspaces/:ws_id/events` SSE, PostgreSQL control-plane DDL + AES-encrypted tenant secrets + dry-run import/backup/restore, scope-confirmed plugin marketplace, idempotent Stripe webhook processing preserving free self-hosted BYOK, and React/Expo clients styled with Rakazo UI tokens.
- **Release Readiness**: Merged PR #2 durable `database.rs`, `patch.rs`, `user_services.rs`, and Ed25519 manifest updater verification.

---

## [v0.5.0] — GrokBot Workspace Parity (2026-11-17)

### Added
- **GrokBot Workspace Parity UI (`crates/gm-ui`, `crates/gm-openbot`)**:
  - Replaced polling companion with modular native `egui` and lightweight companion workspaces (`rail`/`sidebar`/`chat`, `Bot Details` 4-tab drawer, `Space` split view, command palette, settings, Vault/connector flows).
  - Authenticated incremental SSE (`/ag-ui/events`) with persisted direct/channel history, stale-bot response guards, attachments, custom bot avatars/media, and cached rich Markdown rendering.
  - Scripts-only Static Spaces served on an isolated loopback port (`spaces_port`), signed update checks via the daemon updater, and Glow default rendering with optional WGPU support.
  - Note: Cloud messaging bridges require configured adapters/relay; RSS/CPU/TCC targets remain uncertified by headless CI.

---

## [v0.4.0] — Meta Muse Agent Integration (2026-11-15)

### Added
- **Muse Agent Integration (`crates/gm-muse`, `crates/gm-memory`, `crates/gm-goals`, `crates/gm-openbot`, `crates/gm-daemon`, `crates/gm-ui`)**:
  - Verbatim upstream `Meta/muse-agent` reference prompt, workspace templates, docs, and 9 bundled skills (`crates/gm-muse/assets/upstream/`) with SHA-256 integrity checks (`UPSTREAM.md`) and automated zero-PII tests.
  - Two-message cache-friendly `MusePromptBuilder` (`[stable_prefix, dynamic_tail]`), `SharedFileStore` with `O_NOFOLLOW` + SHA-256 compare-and-swap + `SecretScrubber`, progressive-disclosure `SkillRegistry`, and rich/plain response directive parser (`<options>`, `<message_break/>`, `【source:...】`, `sandbox://`).
  - SQLite `ProvenanceStore`, two-phase redacted `ForgetPlanner`/`ForgetExecutor` (`.forget-pending.json` with zero raw request text and `ActionTier::Irreversible` confirmation), and fenced portable `MemoryImporter` (`migration/`).
  - `GoalWorkspaceManager` (`goals/<slug>/GOAL.md`), `ProactivePrefsStore`, `HeartbeatWorker`, `FeedStore`, `MemoryUpkeepJob`, and `SelfImprovementJob`.
  - Per-chat Bridge `DiscretionStore` (`side-chats/<chat-id>/DISCRETION.md`, `RULE-PRIVACY-DISCRETION`), two-port Static `SpacesManager` (`127.0.0.1:<spaces_port>`), new CLI commands (`gm skills`, `gm goals`, `gm spaces`, `gm forget`, `gm import-memory`, `gm dream`), Companion Web UI tabs, and native `egui` Muse management windows.

---

## [v0.3.0] — OpenBot Multi-Agent & Coworker Suite (2026-11-03)

### Added
- **Persistent Multi-Bot Roster & Isolated Workspaces (`crates/gm-openbot`)**:
  - Default AI coworkers (`@muse`, `@researcher`, `@coder`, `@guardian`) plus custom bot profiles (`gm bots list|show|create|rm`).
  - Per-bot isolated workspace directories (`<storage_dir>/bots/<bot_id>/{workspace,inbox,outbox}`) with `0700` permissions and dedicated SQLite FTS5 memory stores.
- **Crash-Safe SQLite WAL FIFO Task Queues (`crates/gm-openbot/src/queue.rs`)**:
  - Durable priority + FIFO task scheduling (`gm queue list|add|pause|resume|cancel|reassign|backup|restore`), crash recovery of interrupted in-flight tasks, and JSONL backup/restore.
- **Multi-Bot Channels, A2A Handoffs & Context Compaction**:
  - Persistent `#general`, `#research`, and `#ops` channels (`gm channels`) with automatic `@<bot_id>` mention parsing and task spawning.
  - Structured Agent-to-Agent (`A2aCoordinator`) artifact handoffs copying deliverables from `<sender>/outbox/` to `<recipient>/inbox/` with loop-depth guards (`max_handoff_depth = 4`).
  - `ContextCompactor` token-budget thread summarization.
- **macOS Kernel Seatbelt (`sandbox-exec`) Confinement & Governance Gateway**:
  - `BotWorkspaceSandbox` (`ReadFile`, `WriteFile` with 200-line unified diff preview, `ListFiles`, `Shell` under `/usr/bin/sandbox-exec`).
  - `SecurityGateway` policy evaluation (`RULE-BLK-001` through `RULE-SEC-006`) and structured `GovernanceAuditRecord` logging (`Permitted`, `Refused`, `Failed`).
- ***"Take the Wheel"* Human Handoff & Opaque Vault Secrets**:
  - Automatic MFA/2FA/CAPTCHA blocker detection (`BlockerDetector`), `WaitingForHuman` state transition, and native Cocoa `🛞 Take the Wheel` sheet + HUD indicator.
  - `VaultSecretResolver` (`secret://keychain/<account>` and `secret://1password/<vault>/<item>/<field>`) + `SecretScrubber` ensuring zero plaintext secret leaks.
- **Loopback Companion Web / AG-UI SSE Server & Chat Bridges**:
  - Built-in `axum` server (`gm serve`, default `127.0.0.1:7433`) serving the Companion Web UI (`/`), Governance Audit Dashboard (`/admin/audit`), A2A Agent Card (`/.well-known/agent.json`), AG-UI SSE stream (`/ag-ui/events`), and HMAC-SHA256 verified Slack / Telegram / Discord webhook bridges.
- **`v0.3.0` OpenBot + Muse Evaluation Suite (`crates/gm-eval/src/openbot_suite.rs`)**:
  - 10-scenario automated verification suite (`ob_queue_01`–`ob_agui_05` + `muse_bridge_phishing_06`–`muse_legacy_prompt_10`) integrated into `xtask eval` and `xtask test-all`.

---

## [v0.2.0] — Deep Runtime, SoM Vision, Candle Metal VLM & MCP Bridge (2026-11-03)

### Added
- **Set-of-Marks (SoM) Vision Grounding & Stagnation Escalation (`crates/gm-agent`)**:
  - Annotated screenshot compositor (`#1..#N` badges) and automatic AX digest stagnation escalation to `CuVisionStep`.
- **Native Candle Metal Local VLM Engine (`crates/gm-vlm-candle`)**:
  - Offline inference and HuggingFace Hub model management (`gm models list|pull|rm`) for `Qwen2-VL-2B-Instruct` and `SmolVLM2`.
- **POSIX Shared Memory (`shm_open` + `mmap`) Frame Ring & Event-Driven AX Observer (`crates/gm-platform`)**:
  - Zero-copy frame transport and `AXObserverCreate` cache invalidation.
- **Dual-Socket IPC Control Server, Interactive Cocoa Sheets & MCP Bridge**:
  - Live token streaming, interactive `NSAlert` approval sheets for `ActionTier::Irreversible`, and `darius-tools` MCP tool bridge.

---

## [v0.1.0] — Initial Certified macOS Release (2026-11-03)

### Added
- Native macOS computer-use agent (`GrokMuse.app` + `GrokMuseCUHelper.app`) with ScreenCaptureKit, Accessibility (`AXUIElement`), CoreGraphics input synthesis, EventKit Calendar/Reminders, SQLite FTS5 personal memory, proactive Quiet Gate engine, and 8-gate `xtask test-all` certification.
