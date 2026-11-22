# DT — Native Hybrid Desktop, Web, Mobile & Self-Hostable Agent (`v1.0.3`)

<p align="center">
  <strong>Fast computer-use · Proactive personal agent · OpenBot multi-agent coworker suite · Unified DT Doctor recovery</strong><br/>
  Native Rust desktop (macOS, Windows 11, Ubuntu 24.04 X11/Wayland) · React Web · Expo Mobile · Voice/SIP/Meetings · Hosted Multi-Tenant Control Plane
</p>

<p align="center">
  <a href="https://galaxycoils.github.io/DT/"><strong>🌐 Live Interactive Front Page</strong></a> ·
  <a href="https://github.com/galaxycoils/DT/releases/latest"><strong>📦 Latest Release & Binaries</strong></a> ·
  <a href="docs/release/coverage-inventory.json"><strong>✅ 30/30 Coverage Inventory</strong></a> ·
  <a href="docs/OPERATIONS.md"><strong>🛠️ Operations Guide</strong></a> ·
  <a href="docs/SUPPORT_MATRIX.md"><strong>🖥️ Support Matrix</strong></a> ·
  <a href="docs/PRIVACY.md"><strong>🔒 Privacy & Discretion</strong></a>
</p>

---

## 📦 Download Prebuilt Release Binaries

Download verified release packages and prebuilt binaries from [**GitHub Releases**](https://github.com/galaxycoils/DT/releases):

| Platform / Architecture | Package / Binary Asset | Binaries & Components Included | Verification |
| :--- | :--- | :--- | :--- |
| **Ubuntu 24.04 LTS (x86_64)** | [`dt_1.0.3_amd64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/dt_1.0.3_amd64.deb) | `/usr/bin/{dt,gm,dt-doctor,gm-daemon,gm-cu-helper,gm-ui}`, systemd user units, desktop entry | `.sha256`, `.build.json`, `.sbom.json` |
| **Ubuntu 24.04 LTS (ARM64)** | [`dt_1.0.3_arm64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/dt_1.0.3_arm64.deb) | ARM64 Debian package with systemd user units and desktop entry | `.sha256`, `.build.json`, `.sbom.json` |
| **Linux Generic (x86_64)** | [`DT-1.0.3-linux-x86_64-binaries.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-linux-x86_64-binaries.tar.gz) | Standalone release binaries: `dt`, `gm`, `dt-doctor`, `gm-daemon`, `gm-cu-helper`, `gm-ui` | `.sha256` |
| **macOS Apple Silicon (ARM64)** | [`DT-1.0.3-macos-aarch64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-macos-aarch64.dmg) | `DT.app`, `DTCUHelper.app`, `Install DT.pkg` (`dt`, `gm`, `dt-doctor`, `gm-daemon`, `gm-cu-helper`, `gm-ui`) | `.sha256`, `.build.json`, `.sbom.json` |
| **macOS Intel (x86_64)** | [`DT-1.0.3-macos-x86_64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-macos-x86_64.dmg) | `DT.app`, `DTCUHelper.app`, `Install DT.pkg` (`dt`, `gm`, `dt-doctor`, `gm-daemon`, `gm-cu-helper`, `gm-ui`) | `.sha256`, `.build.json`, `.sbom.json` |
| **Windows 11 (x86_64)** | [`DT-1.0.3-windows-x86_64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-windows-x86_64-setup.exe) | NSIS per-user installer + SID-isolated pipe & scheduled task definitions | `.sha256`, `.build.json`, `.sbom.json` |
| **Windows 11 (ARM64)** | [`DT-1.0.3-windows-aarch64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-windows-aarch64-setup.exe) | NSIS per-user ARM64 installer + SID-isolated pipe & scheduled task definitions | `.sha256`, `.build.json`, `.sbom.json` |
| **React Web Client Bundle** | [`DT-1.0.3-web-client.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.3/DT-1.0.3-web-client.tar.gz) | Production Vite build (`index.html`, Rakazo token CSS, JS bundle, Front Page) | `.sha256` |

```bash
# Verify any downloaded release package with its SHA-256 sidecar:
shasum -a 256 -c dt_1.0.3_amd64.deb.sha256
```

---

## ✨ What's New in `v1.0.3` (Pre-Release Hardening, UX Elevation & 100% Contract Coverage)

1. **Interactive Cross-Device React Web & Expo Mobile Experience (`clients/web/`, `clients/mobile/`, `tools/generate-ui-tokens.py`)**:
   - **Responsive 3-Tier Workspace Hierarchy**: Drag-resizable desktop sidebar (`240px–480px`), 1-click collapse to a `72px` icon rail, and a touch-friendly mobile slide-over drawer (`<= 768px`) with backdrop blur.
   - **Geometric Coworker Avatars & Live Presence**: Distinct geometric avatar frames (`circle`, `squircle`, `hexagon`, `diamond`) for `@muse`, `@researcher`, `@coder`, and `@guardian` with animated eye-blink keyframes, live status dots, and active status phrases.
   - **Command Palette (`⌘K` / `Ctrl+K`)**: Instant modal fuzzy switcher across coworkers, `#channels`, views, routines, spaces, DT Doctor diagnostics, and theme switching.
   - **Per-Target Draft Isolation & Live Transcripts**: Fixed conversation switching so drafts remain strictly isolated per `dm:<bot_id>` / `channel:<id>` in `sessionStorage` while loading live historical transcripts from `/api/v1/direct/:bot_id/messages` and `/api/v1/channels/:id/messages`.
   - **Smart Typography Composer & Inline Approval Cards**: Automatic `->` to `→` smart arrow replacement outside inline/fenced code blocks with single-Backspace undo, contextual empty-state starter prompt chips, and non-blocking inline Tier-3 **Approval Cards** (`Approve` / `Deny`).
   - **Interactive Routines, Split-View Space Sandbox, Provider Pipeline & DT Doctor**:
     - **Routines**: 1-click **`Trigger Now`** execution (`POST /api/v1/routines/:id/test` & `/api/v1/routines/:id/trigger`).
     - **Spaces**: Split-view static Space creator + live isolated iframe preview (`sandbox="allow-scripts"`, no `allow-same-origin`).
     - **Providers & Connectors**: Visual 4-stage pipeline (`Unconfigured → Saved → Discovered → Verified`), live Connector capability prober, and scope-confirmed Marketplace lifecycle.
     - **DT Doctor**: Structured installation readiness scorecard with categorized diagnostic badges and 1-click JSON export.
2. **Full CLI & `/api/v1` Parity (`crates/gm-daemon`, `crates/gm-openbot`)**:
   - Exposed `dt patch <file> [--repo <dir>] [--approve-sha256 <hex>]`, `dt routines trigger <id> [--delivery-id <id>]`, `dt diagnostics prune-backups [--keep <N>]`, and `dt diagnostics recover-database <db>` on both `dt` and the `gm` compatibility alias.
   - Added `POST /api/v1/routines/:id/trigger`, returned active user workspace memberships in `GET /api/v1/workspaces`, and allowed `POST /api/v1/marketplace/:id/update` to resolve built-in catalog items by ID.
3. **100% Release Coverage Inventory Verification (`docs/release/coverage-inventory.json`, `tools/test_release_tools.py`)**:
   - All **30/30 required `test_ids`** across all **26 required capability rows** in `docs/release/coverage-inventory.json` are implemented and verified by `test_coverage_inventory_test_ids_exist_in_codebase`, alongside **384/384 passing workspace tests** under `cargo nextest`.
4. **Cross-Platform Packaging & Docker Hardening (`xtask`, `tools/installers/linux/build-deb.sh`, `deploy/docker/Dockerfile`)**:
   - Enforced deterministic `umask 022` and `0755` directory modes in `build-deb.sh`, added `dt` and `dt-doctor` symlinks to `xtask install`, added cross-platform JPEG encoding benchmarks in `xtask bench-hotpath`, and updated `deploy/docker/Dockerfile` with PipeWire/XCB build packages and the `gm` binary alias.

---

## What's in DT

- **Pinned Local Components**: 8 optional local runtimes and models (`qwen2-vl-2b`, `smolvlm`, `smolvlm2`, `vlm_worker`, `whisper-stt-base-en`, `speech_tts`, `vm_image`, `vm_firmware` — 15 OS/architecture entries pinned with Ed25519 signatures and SHA-256 checksums in [`docs/release/component-catalog.json`](docs/release/component-catalog.json)). Pull local models from the CLI with `dt models list` / `dt models pull <id>` or install and verify optional components from the Desktop / Companion Settings UI (`/api/system/components`).
- **PostgreSQL Hosted Mode**: Set `DT_DATABASE_URL=postgres://...` (or `DATABASE_URL`) when running `dt serve` to enable `PostgresHostedStore` (migrations V1–V4, row-level tenant isolation, and SHA-256-verified SQLite-to-PostgreSQL migration).
- **Consent-Gated Voice & Dictation**: Managed local Whisper STT and TTS sessions via `VoiceSessionManager`, requiring explicit user consent with immediate child-process teardown on stop, barge-in, or consent revocation.
- **Desktop Keyboard Shortcuts**: `Cmd/Ctrl+Shift+M` (Studio — Templates, Skills, Routines & Sections), `Cmd/Ctrl+B` (toggle sidebar collapse), `Cmd/Ctrl+Shift+F` (expand sidebar & search conversations), and `Cmd/Ctrl+D` (start/stop dictation), alongside `Cmd/Ctrl+K` (Command Palette), `Cmd/Ctrl+,` (Settings), `Cmd/Ctrl+/` (Shortcuts cheat-sheet), and `Cmd/Ctrl+Alt+B` (Bot Details drawer).
- **Operations & Recovery**: See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for installation, `dt-doctor` recovery & snapshot restore, component downloads, spending controls, hosted backup/restore, and device revocation.

---

## 🎯 Goal State & Performance Budgets (G1–G8)

| Gate | Target Budget | Verification & Architecture |
| :--- | :--- | :--- |
| **G1 Native** | **Zero Electron / Chromium** | Native Rust `egui`/Glow/WGPU desktop UI (`gm-ui`) + per-user CU helper (`gm-cu-helper`) + unprivileged daemon (`gm-daemon`) + `dt` / `dt-doctor` CLIs. |
| **G2 Footprint** | **`< 50 MB` idle RSS** | Whole process tree idle RSS (`< 120 MB` with desktop UI open), measured via `footprint` / `ps`. |
| **G3 Hot Path** | **`< 2 ms` capture, `< 30 ms` AX** | Frame fetch `< 2 ms` p99, AX snapshot `< 30 ms` p95, downscale + JPEG `q=70` `< 15 ms` p95, action dispatch `< 5 ms` p99. |
| **G4 Agent Step** | **`< 1.5 s` median, `< 4.0 s` p95** | Fast-route end-to-end agent step latency including streaming model inference (`xtask bench-llm`). |
| **G5 Quality** | **`≥ 70%` pass, `0` violations** | 25-task evaluation suite + v0.2.0 SoM/MCP suite + 10-scenario OpenBot/Muse governance suite (**100% / 40 of 40 passing** in `xtask eval`). |
| **G6 Proactive** | **`≥ 80%` precision, `≤ 3`/day** | Quiet Gate (22:00–08:00), max 3 notices/day, `$0.50`/day proactive spend cap, `done`/`later`/`stop` feedback semantics (`xtask simulate-dogfood`). |
| **G7 Safety** | **`< 100 ms` emergency kill** | 3-tier action classification (`Read`, `Reversible`, `Irreversible`), blocklist & workspace confinement (`RULE-BLK-001`..`RULE-SEC-006`), `DISCRETION.md` deny-wins privacy guard, and append-only `audit.jsonl`. |
| **G8 Routing** | **5-Tier Provider Precedence** | `Routine → Bot → Workspace → Account → Legacy` across ChatGPT OAuth (`/v1/responses`), OpenAI API, Kilo Gateway, OpenRouter, Nous Portal OAuth, Meta Muse, xAI Grok, Custom Endpoints, and Local VLM. |

---

## 🏗️ System Architecture

```text
 ┌────────────────────────────────────────────────────────────────────────────────┐
 │  Clients: Native Rust Desktop (gm-ui) · Companion Web · React Web · Expo Mobile │
 │  • Shared Rakazo Tokens (ui/design-tokens.json)   • Geometric Coworker Avatars │
 │  • Command Palette (⌘K / Ctrl+K)                  • Split-View Space Sandbox   │
 └───────────────────┬──────────────────────────────────────────┬─────────────────┘
                     │ Local UDS / SID Named Pipe               │ HTTPS /api/v1 + SSE
 ┌───────────────────▼──────────────────────────────────────────▼─────────────────┐
 │                         gm-daemon / dt (Unprivileged)                          │
 │  ┌──────────────────────┐  ┌──────────────────────┐  ┌──────────────────────┐  │
 │  │ OpenBot Supervisor   │  │ 5-Tier LLM Router    │  │ Security & Privacy   │  │
 │  │ • @muse @researcher  │  │ • ChatGPT PKCE OAuth │  │ • Tier 1/2/3 Gateway │  │
 │  │   @coder @guardian   │  │ • Nous / Kilo / OR   │  │ • DISCRETION.md      │  │
 │  │ • SQLite WAL Queue   │  │ • Muse / Grok / Local│  │ • Two-Phase Forget   │  │
 │  │ • Routines & Bridges │  │ • $100 Budget Ledger │  │ • Opaque Secret Vault│  │
 │  └──────────┬───────────┘  └──────────┬───────────┘  └──────────┬───────────┘  │
 │             │                         │                         │              │
 │  ┌──────────▼───────────┐  ┌──────────▼───────────┐  ┌──────────▼───────────┐  │
 │  │ Hosted Control Plane │  │ Voice & Meetings     │  │ DT Doctor & Updater  │  │
 │  │ • PostgreSQL / SQLite│  │ • Local Whisper/TTS  │  │ • Journaled Repairs  │  │
 │  │ • AES-256-GCM + HKDF │  │ • Asterisk ARI SIP   │  │ • SQLite VACUUM INTO │  │
 │  │ • Cookie + CSRF Auth │  │ • Zoom RTMS & Meet   │  │ • Ed25519 Manifests  │  │
 │  └──────────────────────┘  └──────────────────────┘  └──────────────────────┘  │
 └───────────────────────────────────────┬────────────────────────────────────────┘
                                         │ Local UDS / SID Named Pipe + POSIX SHM Ring
 ┌───────────────────────────────────────▼────────────────────────────────────────┐
 │                 gm-cu-helper (Per-User Native Desktop Helper)                  │
 │  • macOS: ScreenCaptureKit, Accessibility (AXUIElement), CoreGraphics, EventKit│
 │  • Windows 11: Windows.Graphics.Capture, UI Automation (UIA), SendInput        │
 │  • Ubuntu 24.04: X11 XShm/XTest + Wayland XDG Desktop Portal / PipeWire / EIS  │
 └────────────────────────────────────────────────────────────────────────────────┘
```

---

## 🧩 Workspace Crates

| Crate | Responsibility |
| :--- | :--- |
| **[`crates/gm-core`](crates/gm-core)** | Domain types (`AxSnapshot`, `ComputerAction`, provider profiles), length-prefixed IPC protocol, Windows SID SDDL validation, config schemas. |
| **[`crates/gm-platform`](crates/gm-platform)** | Cross-platform `Platform` trait (`capture`, `ax_snapshot`, `act`, `windows`, `permissions`), macOS/Windows/Linux native backends, POSIX SHM frame ring. |
| **[`crates/gm-cu-helper`](crates/gm-cu-helper)** | Per-user native desktop helper holding TCC / desktop capture permissions across daemon upgrades. |
| **[`crates/gm-llm`](crates/gm-llm)** | Multi-provider client (`/v1/chat/completions` & `/v1/responses` SSE), ChatGPT & Nous PKCE/OAuth, 5-tier router, budget manager, release budget ledger. |
| **[`crates/gm-safety`](crates/gm-safety)** | Action tier classifier, `< 100 ms` kill switch, blocklist manager, path/symlink confinement, secret scrubber, untrusted data fencing, `AuditLogger`. |
| **[`crates/gm-memory`](crates/gm-memory)** | SQLite FTS5 personal memory store, `ProvenanceStore` citation ledger, two-phase redacted `ForgetPlanner`/`ForgetExecutor`, fenced `MemoryImporter`. |
| **[`crates/gm-muse`](crates/gm-muse)** | Bundled upstream Muse prompt & 9 core skills (`UPSTREAM.md` SHA-256 verified), cache-friendly `MusePromptBuilder`, CAS `SharedFileStore`, `SkillRegistry`. |
| **[`crates/gm-goals`](crates/gm-goals)** | Durable goals & 1-level subgoals (`GOAL.md`), `QuietGate`, `HeartbeatWorker`, `FeedStore`, dogfood tracker, nightly review & self-improvement jobs. |
| **[`crates/gm-agent`](crates/gm-agent)** | Autonomous computer-use loop, negative-origin & DPI-aware Set-of-Marks (SoM) compositor, stagnation detector, MCP bridge, *"Take the Wheel"* handoff. |
| **[`crates/gm-openbot`](crates/gm-openbot)** | Multi-agent supervisor, persistent `BotRoster`, crash-safe SQLite `DurableTaskQueue`, channels, routines, `DiscretionStore`, Static Spaces, Voice/SIP/Zoom/Meet, Hosted Control Plane, `/api/v1` + Companion Web server. |
| **[`crates/gm-vlm-candle`](crates/gm-vlm-candle)** | Native Rust local Vision-Language Model inference (`Qwen2-VL-2B-Instruct`, `SmolVLM2`) and safetensors model registry. |
| **[`crates/gm-daemon`](crates/gm-daemon)** | Background orchestrator, IPC control server, unified `dt-doctor` engine, SHA-256 patch reviewer, Ed25519 signed updater, chunked component installer, `dt` / `gm` CLI. |
| **[`crates/gm-ui`](crates/gm-ui)** | Native Rust `egui`/Glow/WGPU desktop shell with virtualized 10k-message transcript, 5-tier Escape stack, smart arrow composer, and LRU texture cache. |
| **[`crates/gm-eval`](crates/gm-eval)** | 25-task G5 evaluation suite, v0.2.0 SoM/MCP suite, and 10-scenario OpenBot/Muse governance evaluation suite. |
| **[`xtask`](xtask)** | Build & release automation (`bundle`, `dmg`, `verify-dmg`, `install`, `notarize`, `bench-hotpath`, `bench-rss`, `bench-llm`, `eval`, `simulate-dogfood`, `budget-ledger`, `certify`, `test-all`). |

---

## 🚀 Quick Start & Installation

### 1. Ubuntu 24.04 LTS (`.deb` Package)
```bash
sudo apt install ./dt_1.0.3_amd64.deb
dt doctor check
dt service install
```

### 2. macOS Apple Silicon & Intel (`.dmg` / `Install DT.pkg`)
1. Download `DT-1.0.2-macos-aarch64.dmg` (or `x86_64`) and verify its `.sha256` sidecar.
2. Open the disk image and run `Install DT.pkg` (or drag `DT.app` to `/Applications`).
3. Run onboarding and grant Accessibility / Screen Recording permissions:
   ```bash
   dt onboard
   dt permissions --open-missing
   dt service install
   ```

### 3. Windows 11 (`x86_64` & `ARM64`)
1. Run `DT-1.0.2-windows-x86_64-setup.exe` (installs per-user under `%LOCALAPPDATA%\Programs\DT\bin`).
2. Verify installation readiness and user background tasks:
   ```powershell
   dt doctor check
   dt service status
   ```

### 4. Self-Hosted Docker Compose + Automatic Caddy TLS
```bash
cd deploy/docker
docker compose up -d --build
```
The production multi-stage [Dockerfile](deploy/docker/Dockerfile) runs unprivileged as `USER dt` (`uid 10001`) with a built-in `dt-doctor check --json` healthcheck and internal loopback binding behind `tls-proxy`.

### 5. Building from Source
DT expects the pinned sibling `Darius` checkout (`f47a4ce980abecb751c9f5c79e1c424a20100433`) alongside the repository:
```bash
git clone https://github.com/galaxycoils/darius.git Darius
git -C Darius checkout f47a4ce980abecb751c9f5c79e1c424a20100433

git clone https://github.com/galaxycoils/DT.git DT
cd DT
./tools/bootstrap.sh

# Build all native binaries (dt, gm, dt-doctor, gm-daemon, gm-cu-helper, gm-ui)
cargo build --release --locked --workspace --bins
```

---

## 💻 CLI Command Reference (`dt` & `gm` alias)

All commands work identically via `dt` (primary CLI) and `gm` (backwards-compatible alias):

| Command | Arguments | Description |
| :--- | :--- | :--- |
| `dt doctor` | `[check\|repair\|resume <run_id>] [--json]` | Run unified DT Doctor checks, journaled safe repairs (max 2 attempts), or resume an interrupted run. |
| `dt do` | `"<instruction>"` | Execute an autonomous computer-use task via native Accessibility + SoM vision fallback. |
| `dt daemon` | `[--config <path>]` | Start the unprivileged background daemon and proactive scheduler. |
| `dt serve` | `[--bind 127.0.0.1:7433]` | Launch the Companion Web UI, `/api/v1/*` REST + SSE server, and `/admin/audit` governance dashboard. |
| `dt chat` | `[--system "<prompt>"]` | Interactive terminal REPL with `/memory`, `/status`, `/do`, `/history`, and live token streaming. |
| `dt status` | — | Display live daemon health, CU helper connectivity, active task, and daily spend. |
| `dt kill` | — | Trigger the emergency `< 100 ms` kill switch halting all active agent work. |
| `dt permissions` | `[--open-missing] [--open <pane>] [--json]` | Inspect and request OS privacy permissions (`screen`, `accessibility`, `calendar`, `reminders`). |
| `dt service` | `<install\|status\|uninstall>` | Manage per-user background services (macOS LaunchAgents, Linux systemd user units, Windows SID tasks). |
| `dt bots` | `<list\|show\|create\|rm> [--json]` | Manage the OpenBot persistent coworker roster (`@muse`, `@researcher`, `@coder`, `@guardian`, custom bots). |
| `dt queue` | `<list\|add\|pause\|resume\|cancel\|reassign\|backup\|restore>` | Manage the crash-safe SQLite WAL FIFO task queue across coworkers. |
| `dt channels` | `<list\|post\|history\|assign\|set-instructions\|archive\|restore>` | Manage multi-bot channels (`#general`, `#research`, `#ops`) and `@bot` mention dispatch. |
| `dt routines` | `<list\|runs\|trigger <id> [--delivery-id <id>]> [--json]` | Inspect scheduled routines, run history, or trigger an immediate deduplicated routine delivery. |
| `dt skills` | `<list\|search <query>\|show <name>> [--json]` | Search and inspect bundled Muse skills and connectors via progressive disclosure. |
| `dt goals` | `<list\|create\|subgoal\|complete\|delete> [--json]` | Manage durable personal goals and 1-level subgoals (`goals/<slug>/GOAL.md`). |
| `dt spaces` | `<list\|create <slug>\|url <slug>> [--json]` | Create and inspect isolated Static Spaces served on the dedicated loopback Spaces origin. |
| `dt forget` | `<plan "<request>"\|confirm <plan_id>> [--json]` | Two-phase redacted discretionary Forget across SQLite memory, files, and goal workspaces. |
| `dt import-memory` | `[--file <path>\|--input "<text>"] [--apply --report-sha256 <hex>]` | Import portable memory notes with untrusted fencing and SHA-256 dry-run approval. |
| `dt patch` | `<patch_file> [--repo <dir>] [--approve-sha256 <hex>]` | Inspect a unified diff patch and apply it atomically after SHA-256 digest approval. |
| `dt diagnostics` | `<check\|export\|prune-backups [--keep <N>]\|recover-database <db>>` | Run local health checks, export redacted support bundles, prune snapshots, or recover corrupt SQLite DBs. |
| `dt models` | `<list\|pull <id>\|rm <id>> [--json]` | Manage local Vision-Language Models (`qwen2-vl-2b`, `smolvlm2-2.2b`). |
| `dt update` | `[--check] [--force] [--json]` | Verify Ed25519-signed release manifests and renewable update metadata with rollback protection. |

---

## 🧪 Verification & Release Certification Suites

Run the full pre-shipping verification battery locally:

```bash
# 1. Rust formatting & zero-warning workspace linter
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings

# 2. Full 384-test workspace unit & integration suite
cargo nextest run --locked --workspace

# 3. Real headless X11 desktop capture, focus, keyboard & pointer test (Linux)
tools/test-native-x11.sh

# 4. Shared token generation, Companion Web UI (< 64 KiB cap) & headless Chromium smoke test
python3 tools/generate-ui-tokens.py
npm run build --prefix tools/companion
npm test --prefix tools/companion
node --test tools/tests/companion-events.test.cjs
npm run smoke --prefix tools/companion

# 5. React Web Client & Expo Mobile Client build and contract tests
npm run typecheck --prefix clients/web && npm run build --prefix clients/web && npm test --prefix clients/web
npm test --prefix clients/mobile

# 6. Release tools, 30/30 coverage inventory contract & workspace guest agent tests
python3 tools/test_release_tools.py -v
python3 tools/workspace-guest/test_agent.py -v

# 7. xtask evaluation, latency, budget ledger & dogfood gates
cargo run --locked -p xtask -- eval
cargo run --locked -p xtask -- bench-llm
cargo run --locked -p xtask -- simulate-dogfood
cargo run --locked -p xtask -- budget-ledger --json
cargo run --locked -p xtask -- certify all --allow-incomplete --json
```

---

## 📄 License

MIT License. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for details.
