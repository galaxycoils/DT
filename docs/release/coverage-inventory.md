# DT v1.0.2 Release Coverage Inventory

This inventory maps every advertised feature, CLI entry point, API route group, settings control, provider, local runtime, workflow, and shipped skill/plugin to its supported surfaces, dependencies, expected outcomes, observable assertions, and verification test IDs. The machine-readable contract lives in [coverage-inventory.json](coverage-inventory.json) and is enforced by [verify-release-evidence.py](../../tools/verify-release-evidence.py).

## Supported Platforms & Release Assets

| Platform | Asset Name | Coverage | Certification Status |
|---|---|---|---|
| Apple Silicon | `DT-1.0.2-macos-aarch64.dmg` (contains `Install DT.pkg`) | macOS 14+ | Pending Developer ID signing, notarization, and 8-target native evidence |
| Intel Mac | `DT-1.0.2-macos-x86_64.dmg` (contains `Install DT.pkg`) | macOS 14 through Tahoe | Pending Intel hardware build, signing, and native evidence |
| Windows x64 | `DT-1.0.2-windows-x86_64-setup.exe` | Windows 11 25H2 and 26H2 | Pending NSIS 3.13 build, Authenticode signing, and native evidence |
| Windows ARM64 | `DT-1.0.2-windows-aarch64-setup.exe` | Windows 11 25H2 and 26H2 (ARM64) | Pending native ARM64 payload, Authenticode signing, and native evidence |
| Ubuntu x64 | `dt_1.0.2_amd64.deb` | Ubuntu Desktop 24.04 (X11 and Wayland) | Pending Ubuntu 24.04 `.deb` App Center smoke and X11/Wayland evidence |
| Ubuntu ARM64 | `dt_1.0.2_arm64.deb` | Ubuntu Desktop 24.04 ARM64 (X11 and Wayland) | Pending Ubuntu 24.04 ARM64 `.deb` App Center smoke and X11/Wayland evidence |

## Capability & Surface Matrix

| ID | Category | Capability | Surfaces | Observable Assertion | Required Evidence |
|---|---|---|---|---|---|
| `feat-doctor-check-repair-resume` | Installer / Doctor | `dt doctor check/repair/resume --json`, `dt-doctor`, `gm diagnostics` compat | CLI, Native, Web, Mobile | Schema v1 report with run ID, max 2 repair attempts, sanitized web/mobile status | Fault-injection & lifecycle logs |
| `feat-installers-macos-pkg-dmg` | Installer / Doctor | `Install DT.pkg` inside ARM64 & Intel DMGs | macOS installers | Gatekeeper & notarization pass; `DT.app` + `DTCUHelper.app` registered | Clean-install & upgrade logs |
| `feat-installer-windows-nsis` | Installer / Doctor | NSIS 3.13 installer, per-user tasks, SID pipes, WHPX reboot resume | Windows installer | Authenticode verified; remote & cross-SID named pipe clients rejected | Windows 25H2/26H2 install logs |
| `feat-installer-ubuntu-deb` | Installer / Doctor | Ubuntu 24.04 `.deb`, desktop entry, user systemd graphical units | Linux `.deb` | App Center installs deps cleanly; Secret Service/AT-SPI/X11/PipeWire/Wayland verified | X11 & Wayland install logs |
| `feat-migration-rollback-components` | Installer / Doctor | v0.7.0 migration, dual-store conflict preservation, rollback, chunked component catalog | CLI, Daemon | Consistent SQLite snapshot; dual-store prompt; chunked SHA-256 verification | Migration/rollback & download fault logs |
| `cli-dt-and-gm-alias` | CLI | `dt` primary CLI + `gm` compatibility alias + `dt-doctor` | CLI | Full command parity across `dt` and `gm` | CLI help & smoke test output |
| `provider-chatgpt` .. `provider-custom-local` | Providers | ChatGPT, OpenAI API, Meta, xAI, Kilo, OpenRouter, Nous, Custom/Local (Chat Completions + Responses) | Native, Companion, Web, Mobile, Daemon | Separate `CredentialsSaved`, `Authenticated`, `CatalogLoaded`, `CapabilityVerified`; fail-in-place | Real-account live receipts + loopback logs |
| `runtime-local-vlm` | Local Runtime | Qwen2-VL-2B, SmolVLM, SmolVLM2 (`llama.cpp` MTMD & ONNX DirectML) | Daemon, Native, CLI | Demand start/unload, explicit CPU fallback status, no mock inference in release | Real checkpoint inference logs |
| `runtime-voice-managed` | Local Runtime | Managed Whisper STT & local TTS runtimes | Daemon, Native, Companion | Loopback auth, bounded buffers, consent gate, barge-in & revocation | Voice lifecycle & revocation logs |
| `workflow-desktop-automation` | Workflow | Capture, accessibility, input, clipboard, notifications, Take the Wheel | Helper, Daemon, Native | Mixed scaling, negative origins, hotplug, revocation, <100 ms stop | 3×30/30 acceptance + >=30 hot-path samples |
| `workflow-isolated-coding` | Workflow | Offline VM + job-scoped CLI broker (Claude Code, Codex, Grok Build) + `dt patch` | Agent, Daemon, CLI | Zero host mounts/net in offline guest; egress allowlist; digest-checked patch apply | Signed guest boot & CLI broker logs |
| `workflow-research-workspace` | Workflow | Provenance citations, channels, Pages/Spaces, memory, forget, goals, routines, bounded previews | Daemon, All UIs | DST & restart safety, routine `delivery_id` dedup, bounded PDF/image previews | Research acceptance & routine logs |
| `workflow-connectors-bridges` | Workflow | Gmail, Calendar, GitHub, email, 1Password, EventKit, MCP stdio/HTTPS; Slack/Teams/Discord/Telegram | OpenBot, UIs | Honest capability probing, replay rejection, deny-wins discretion | Dedicated-account connector/bridge logs |
| `workflow-voice-sip-meetings` | Workflow | Voice, Asterisk SIP, Zoom RTMS, Google Meet live notes + REST | OpenBot, Voice | Consent gate, Tier-3 PSTN transfer gate, Zoom RTMS, Google Meet capture+API | Telephony & meeting test logs |
| `workflow-marketplace-billing` | Workflow | Marketplace lifecycle & server-side Stripe billing | API v1, OpenBot, Web | Scope confirmation, server-side Stripe secrets/price allowlist, webhook replay safety | Marketplace & Stripe test-mode logs |
| `hosted-postgresql-sessions-tls` | Hosted | SQLx PostgreSQL, membership RBAC, AES-256-GCM + HKDF, HttpOnly Secure sessions, CSRF, digest-pinned deploy | API v1, Web, Mobile, Deploy | Per-route/SSE membership check, no browser bearer storage, GCM tamper rejection | PostgreSQL tenant isolation & TLS logs |
| `ui-tokens-and-surfaces` | UI / Clients | Single-source tokens -> Native egui, Companion, React Web, Expo SDK 58 Mobile | All 4 UIs | 316 px sidebar, 240–480 resize, 72 collapsed, drafts, 4.5:1/3:1 contrast | Responsive & a11y screenshots + CI checks |
| `shipped-skills-9` | Skills | Bundled Muse skills (`artifacts`, `device-data`, `forget`, `goals`, `self-awareness`, `skill-creator`, `spaces`, `wide-research`) | Daemon, UIs | SHA-256 manifest verification & progressive disclosure | Skill manifest test log |
| `updater-immutable-and-renewable` | Release | Immutable release assets + signed `update-metadata` branch | Updater, Doctor, CI | Rejects expired, rolled-back, revoked, wrong-origin, or wrong-target metadata | Expiry, renewal, key transition, offline tests |
