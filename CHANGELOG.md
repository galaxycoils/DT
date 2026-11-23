# Changelog

All notable changes to **DT** (formerly Grok-Muse) are documented in this file.

## [v1.0.9] — Desktop memory and component updates (2026-11-23)

- Share the macOS fallback font across text renderers and load its pages on demand.
- Use the patched ZIP reader already in the workspace for Windows model worker installation.
- Let Cargo fetch Darius at its pinned revision so fresh checkouts and dependency updates resolve the same source.

## [v1.0.8] — Reliability and recovery preview (2026-11-23)

- Preserve SQLite data on failed ordinary opens. Verify consistent nested memory snapshots and stream file checksums during recovery.
- Stop hosted requests on workspace initialization errors, scope provider settings and usage, and add revocable mobile device enrollment.
- Install a runnable local model configuration and verify offline inference before Doctor reports it ready.
- Record native dictation with explicit microphone consent and submit synthesized speech to the platform audio player.
- Explain manual upgrades in builds without the publisher key.

## [v1.0.6] — DT Doctor Memory Check (2026-11-23)

- Fixed a false `migration_state` failure on profiles that store their memory database at `memory.sqlite/memory.db`. Doctor now checks the nested database and still flags corruption.

## [v1.0.5] — Local Web Pairing and Live Reconnect (2026-11-22)

- Added memory-only Companion token pairing to the React client on loopback.
- Preserved the last durable event ID when reconnecting after transient chat deltas.
- Restored sandboxed Page previews at the canonical daemon origin and surfaced clear errors for incompatible origins and older daemons.
- Routed Caddy's HTTPS root to the daemon-served React app and refreshed installation, upgrade, and release download instructions.

## [v1.0.4] — Full Unification, Same-Origin `/app` Web Hosting, PostgreSQL CI Verification & Security Hardening (2026-11-22)

- **Unified `master` (`v1.0.2` Backend + `v1.0.3` UX)**: Merged `codex/dt-stable-release` (`v1.0.2`) into `master` as a 2-parent merge commit with zero backend regressions (line-by-line preservation guard verified): brought pinned 15-entry `component-catalog.json` (valid through `2027-11-19`), active PostgreSQL hosted control plane (`PostgresHostedStore` / `HostedPlaneBackend`), consent-gated `VoiceSessionManager`, redesigned native desktop UI, and `tools/renew-update-metadata.py` + `docs/OPERATIONS.md` onto `master` alongside `v1.0.3`'s interactive UX and CLI parity.
- **Same-Origin `/app` React Web Hosting & Dev Workflow (`crates/gm-openbot/src/web.rs`, `crates/gm-daemon/src/lib.rs`, `clients/web/vite.config.ts`, `xtask`)**: Served `clients/web/dist` directly from `dt serve` at `/app/` (308 redirect from `/app`, SPA fallback, immutable `/app/assets/*` caching, strict CSP/frame/nosniff headers, canonical traversal protection), bundled `/usr/share/dt/web` into Ubuntu `.deb` and Docker images, updated `Caddyfile` to proxy `/app*`, added Vite dev proxy (`localhost:5173`), and added `cargo run -p xtask -- dev`.
- **Real PostgreSQL 16 Integration Suite & CI Job (`crates/gm-openbot/tests/postgres_hosted.rs`, `.github/workflows/ci.yml`)**: Added 7 live PostgreSQL integration tests (migrations V1–V4 idempotency & advisory-lock concurrency, tenant isolation, AES-256-GCM secret vault tamper detection, SHA-256-verified SQLite migration, sessions/enrollment, backend kind) proven in CI (`postgres-integration` job) and enforced via coverage row `HOST-PG-01` (alongside `WEB-APP-01`).
- **First-Run Doctor Hints, Version Sync, Metadata Expiry Guard & Security Fixes (`doctor.rs`, `test_release_tools.py`, `clients/mobile/package.json`, `App.tsx`, `publish-release-binaries.yml`)**: Added 4 non-failing `dt doctor` first-run checks (`web_app_present`, `optional_components`, `hosted_database`, `metadata_expiry`), synchronized all workspace & client versions to `1.0.4` with `test_versions_are_in_sync` and 30-day `test_signed_metadata_not_expiring_soon` guards, resolved the `minimatch` Dependabot advisory (`0` audit vulnerabilities across web & mobile), fixed the `721px–768px` mobile drawer button cascade in `App.tsx`, and generalised `publish-release-binaries.yml` for any release tag.

## [v1.0.3] — Pre-Shipping Hardening, Interactive Cross-Device UX & 100% Coverage Inventory (2026-11-21)

- **Interactive Cross-Device React Web & Expo Mobile Experience (`clients/web/`, `clients/mobile/`, `tools/generate-ui-tokens.py`)**: Added responsive 3-tier layout hierarchy (drag-resizable desktop sidebar `240px–480px`, `72px` collapsed icon rail, mobile slide-over drawer), geometric coworker avatars with animated eyes, Command Palette (`⌘K`/`Ctrl+K`), per-target draft isolation + historical transcript loading, smart `->` to `→` composer with single-Backspace undo, inline Tier-3 Approval Cards, 1-click Routine `Trigger Now`, split-view `sandbox="allow-scripts"` Space preview, 4-stage Provider Connection pipeline + Connector prober, and DT Doctor readiness scorecard.
- **CLI & `/api/v1` Parity (`crates/gm-daemon`, `crates/gm-openbot`)**: Exposed `dt patch`, `dt routines trigger`, `dt diagnostics prune-backups`, and `dt diagnostics recover-database` in the top-level CLI (`dt` and `gm`); added `POST /api/v1/routines/:id/trigger`, returned active workspace memberships in `GET /api/v1/workspaces`, and updated `POST /api/v1/marketplace/:id/update` to resolve built-in catalog items by ID.
- **100% Release Coverage Inventory Contract (`docs/release/coverage-inventory.json`, `tools/test_release_tools.py`)**: Implemented all 30/30 required `test_ids` across all 26 required capability rows, enforced by `test_coverage_inventory_test_ids_exist_in_codebase`, with 384/384 workspace tests passing under `cargo nextest`.
- **Packaging, Docker & Front Page (`tools/installers/linux/build-deb.sh`, `deploy/docker/Dockerfile`, `docs/index.html`)**: Fixed container `umask` directory permissions in `build-deb.sh`, added `dt` and `dt-doctor` symlinks in `xtask install`, added cross-platform JPEG encoding benchmark in `xtask bench-hotpath`, updated `Dockerfile` with Linux build dependencies and `gm` binary, and added the interactive DT Front Page (`docs/index.html`).

## [v1.0.2] — Cross-Surface Repairs, Hosted PostgreSQL, Mobile Entry, Real Voice & Component Catalog (2026-11-19)

- **Mobile Client Startup & API Parity (`clients/mobile/`)**: Added `"main": "expo-router/entry"` and iOS/Android Expo export checks (`export:ios`, `export:android`) in CI, fixed nested routine `{ routine, next_run_utc }` and activity `body` rendering, added `client_msg_id` idempotent reply retry + `"Message saved, reply failed"` + Provider Setup recovery, rendered persisted inline approval cards with live Approve/Deny/Expired updates, synced dark/light system theme tokens, and cleared/aborted all in-flight state cleanly on disconnect.
- **Hosted Control Plane, Active PostgreSQL & Workspace Isolation (`hosted.rs`, `web.rs`, `gm-daemon`)**: Activated `PostgresHostedStore` via `HostedPlaneBackend` when `DT_DATABASE_URL`/`DATABASE_URL` is configured (with `POSTGRES_MIGRATION_V4` and verified `VACUUM INTO` + SHA-256 backed-up SQLite-to-PostgreSQL migration), added operator `POST /api/v1/workspaces`, isolated `/api/v1/*` routes and workspace SSE streams per authenticated `worker_dir`, and enforced honest `"expired"` updates on stale approvals.
- **Hosted React Web & Companion Web UI (`clients/web/`, `companion_ui.source.js`)**: Rebuilt `clients/web` with zero-overflow mobile drawer (`<= 390px` viewport in both Dark & Light themes), clean signed-out connect screen, full state wipe & request abort on sign-out, persisted approval cards, `"Message saved, reply failed"` + idempotent `Retry reply`, nested routine/run history & activity `body` rendering, and Channels/Goals/Pages/Providers/Connectors/Marketplace/Billing/Doctor tabs. Updated Companion Web UI (`63,788` bytes, strictly under `64 KiB`) with `client_msg_id` reply-failure recovery, `Cmd/Ctrl+Shift+M` -> Studio/Templates, active Dictation toggle, and Local Components management.
- **Desktop Controls, Voice/Dictation & Compact Sidebar (`gm-ui`, `voice.rs`, `control_server.rs`)**: Fixed `Cmd/Ctrl+Shift+M` -> Studio, `Cmd/Ctrl+B` sidebar toggle + persistence, `Cmd/Ctrl+Shift+F` expand + search, `Cmd/Ctrl+D` dictation, and Shortcuts cheat-sheet parity; wired composer Voice & Dictation to a real consent-gated `VoiceSessionManager` (with immediate child-process teardown on stop/barge-in/consent revocation); added full compact-sidebar parity (Create, Search, Activity, Pages, Bots + Channels, Connect apps, Settings, Workspace); updated `ApprovalExpired`/`ApprovalResolved` to clear pending DM cards; and surfaced Connector probing, Marketplace scope-confirmed installs, Billing status, and Local Components in Settings/Connect Apps.
- **Local Components Catalog & On-Demand Installer (`docs/release/component-catalog.json`, `components.rs`, `doctor.rs`)**: Populated all 8 optional components (`qwen2-vl-2b`, `smolvlm`, `smolvlm2`, `vlm_worker`, `whisper-stt-base-en`, `speech_tts`, `vm_image`, `vm_firmware`) across all 6 OS/arch targets with verified upstream URLs, exact byte sizes, and SHA-256 digests; added streaming resumable `ComponentManager` with post-install header/SHA-256 verification and live Doctor capability readiness reporting.

## [v1.0.1] — Release Remediation, 6-Target Inventory, Full Recovery & Replay (2026-11-18)

- **6-Target / 8-Session Release Scope & Updater Compatibility (`release-inventory.json`, `updater.rs`, `tools/`)**: Added Windows ARM64 (`DT-1.0.1-windows-aarch64-setup.exe`) and Ubuntu ARM64 (`dt_1.0.1_arm64.deb`), unified release inputs, preserved v1-compatible `dt-release.json` / `dt-update-metadata.json` for historical v1.0.0 updaters, and added `dt-release-v2.json` / `dt-update-metadata-v2.json` for the full v1.0.1 inventory.
- **Release Signing, Packaging & Benchmarks (`release.yml`, `package-release.py`, `xtask`)**: Staged CI signing artifacts in `runner.temp`, added PE/ELF/Mach-O architecture validation in `package-release.py`, added `NSMicrophoneUsageDescription` and `audio-input` entitlement on macOS, added `!uninstfinalize` uninstaller signing on Windows, verified decoded JPEG dimensions in `xtask bench-hotpath`, added installed hash-verified binary checks to `xtask bench-rss`, and enforced per-metric `raw_samples` in `verify-release-evidence.py`.
- **Doctor Recovery (`REC-01`), Microdollar Budgets (`BUD-01`) & Approval Binding**: Recursive nested-attachment snapshots with `PRAGMA quick_check(1)`, journaled rollback resume in `dt doctor` / `dt-doctor`, monotonic safety preservation (`monotonic-safety.json`), integer microdollar atomic reservations, and exact `ActionApprovalGrant` binding.
- **Hosted Replay & Readiness (`HOST-01`)**: Gap-free SSE cursor replay + live broadcast bridging with immediate stream termination on membership revocation, PostgreSQL advisory migration locking + RLS policies, `/api/v1/health/live` and `/api/v1/health/ready` probes, and allowlist + byte-verified support exports.

## [v1.0.0] — DT Stable Release Candidate & Certification Infrastructure (2026-11-17)

- **Rename & Compatibility**: Branded user-facing surfaces to **DT**, exposed `dt` primary CLI and standalone `dt-doctor` recovery binary while preserving `gm` compatibility alias, crate names, credential namespaces, bundle IDs, OAuth identity, and legacy config inputs.
- **Unified Doctor Engine, Installers & Migration (`doctor.rs`, `migration.rs`, `components.rs`)**: Added `dt doctor check/repair/resume --json` with journaled runs, max 2 repair attempts, safe stale lock/socket ownership checks, consistent SQLite snapshot backups (`VACUUM INTO`), dual-store conflict preservation, rollback snapshots, chunked signed component delivery, macOS `Install DT.pkg`, Windows NSIS 3.13 installer, and Ubuntu 24.04 `.deb` packaging.
- **Windows Per-User Pipe & Task Isolation (`ipc.rs`, `user_services.rs`)**: Added SID-scoped pipe names, owner-only SDDL DACLs, `reject_remote_clients(true)`, peer SID checks, and `DT-Daemon-{sid}`/`DT-Helper-{sid}` tasks.
- **Hosted PostgreSQL, AES-256-GCM/HKDF & Cookie+CSRF Sessions (`hosted.rs`, `web.rs`, `clients/`)**: Added `sqlx` PostgreSQL migrations and membership RBAC, AES-256-GCM + HKDF-SHA256 tenant secret encryption, same-origin `HttpOnly; Secure` cookie sessions with `X-DT-CSRF-Token` validation, server-side Stripe secrets/price allowlist, Expo SDK 58 mobile `expo-secure-store`, single-source token generation for all 4 clients, and digest-pinned Docker/TLS deployment configs.
- **Local Runtimes, CLI Egress Broker & Certification (`local_vlm.rs`, `voice.rs`, `cli_driver.rs`, `xtask`, `tools/`)**: Excluded mock inference from release builds, added `llama.cpp` MTMD / DirectML worker specs with explicit CPU fallback reporting, managed Whisper/TTS lifecycle with bounded buffers, job-scoped CLI egress broker, bounded PDF/image attachment previews, `docs/release/coverage-inventory.json`, US\$100 `ReleaseBudgetLedger`, renewable update metadata verification, and `xtask certify`.
- **Certification Gate**: Signing identities, multi-OS hardware runs (Intel Mac, Windows 11, Ubuntu 24.04 X11/Wayland, iOS/Android), real account checks, and the >=7-day soak remain pending execution before public v1.0.0 tag/release publication.

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
