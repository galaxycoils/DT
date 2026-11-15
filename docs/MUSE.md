# Muse Agent Integration (`gm-muse`)

Grok-Muse integrates the **Meta Muse Agent** architecture (`crates/gm-muse`) across its prompt engine, progressive-disclosure skill registry, provenance-backed memory and two-phase `Forget` workflow, autonomous Goal workspaces, Heartbeat/Feed/Self-Improvement background loops, per-chat Bridge `DISCRETION.md` policies, Static Spaces two-port loopback server, Companion Web UI, and native `egui` desktop UI.

---

## 1. Runtime File Layout (`MuseRuntimePaths`)

All Muse runtime state is stored locally inside `<storage_dir>` (or per-bot under `<storage_dir>/bots/<bot_id>/`) with private `0700` directory and `0600` file permissions and `O_NOFOLLOW` symlink rejection on Unix:

```text
<storage_dir>/
├── IDENTITY.md                     # Shared default agent identity (seeded on first boot if absent)
├── SOUL.md                         # Shared default voice & communication temperament
├── USER.md                         # Operator profile, preferences, and private context
├── MEMORY.md                       # Curated durable facts & distilled notes
├── PROACTIVE_PREFERENCES.md        # Quiet hours, muted categories, and proactive cadence
├── FEEDBACK.md                     # Distilled behavioral lessons from operator feedback
├── .forget-pending.json            # Ephemeral redacted two-phase Forget plan (no raw query text)
├── provenance.sqlite               # Claim-to-source provenance ledger (memory_claims, memory_citations)
├── feed.sqlite                     # Proactive Feed cards, ratings, and signal watermarks
├── docs/                           # Reference docs seeded from upstream assets
│   ├── CAPABILITIES.md
│   ├── HARNESS.md
│   ├──LIMITS.md
│   └── ...
├── skills/                         # Custom user/agent-authored progressive-disclosure skills
│   └── <skill-name>/
│       ├── skill.yaml
│       ├── SKILL.md
│       └── references/
├── connectors/                     # Custom connector manifests (<id>.yaml + <id>.md)
├── memory/                         # Dated memory logs (YYYY-MM-DD.md) and topic notes
├── dreams/                         # Nightly memory upkeep & dream consolidation summaries
├── goals/                          # Autonomous multi-step goal workspaces
│   └── <goal-slug>/
│       ├── GOAL.md                 # YAML frontmatter + narrative progress & stop conditions
│       ├── tasks/
│       └── deliverables/
├── side-chats/                     # Per-bridge external chat discretion policies
│   └── <chat-id>/
│       └── DISCRETION.md
├── spaces/                         # Static HTML/CSS/JS Spaces served on the second loopback port
│   └── <slug>/
│       ├── .space.json
│       └── index.html
├── migration/                      # Fenced portable memory import staging & SHA-256 audit reports
└── bots/
    └── <bot_id>/
        ├── IDENTITY.md             # Per-bot identity override (falls back to shared IDENTITY.md)
        ├── SOUL.md                 # Per-bot soul override (falls back to shared SOUL.md)
        ├── workspace/
        ├── inbox/
        └── outbox/
```

All mutations to shared Markdown and JSON files go through `SharedFileStore` (`crates/gm-muse/src/shared_files.rs`), which enforces path-traversal rejection, `O_NOFOLLOW` symlink guards on Unix, in-process per-path mutex locking, optional SHA-256 compare-and-swap (`expected_sha256`), `SecretScrubber` redaction, and atomic tempfile + `fsync` + `rename` persistence.

---

## 2. `MuseConfig` Feature Flags & Defaults

`GmConfig.muse` (`crates/gm-core/src/config.rs`) is backwards-compatible (`#[serde(default)]`) so existing configuration files without a `"muse"` block load cleanly with defaults:

| Field | Type | Default | Validation Constraint | Description |
|---|---|---|---|---|
| `enable_muse_prompt` | `bool` | `true` | — | Assemble the Muse system prompt (`SYSTEM_PROMPT.md` + `Runtime Adaptation` + persona/skills/memory). When `false`, legacy `profile.system_prompt` is used unchanged. |
| `enable_skills` | `bool` | `true` | — | Include the progressive-disclosure Skill & Connector auto-inventory in the prompt and enable `SkillSearch` / `LoadSkill` step actions. |
| `enable_heartbeat` | `bool` | `false` | — | Run the periodic `HeartbeatWorker` for autonomous goal progression and proactive Feed card generation. |
| `enable_dreams` | `bool` | `false` | — | Run the nightly `MemoryUpkeepJob` (`dreams/`) and weekly `SelfImprovementJob` (`FEEDBACK.md` + skill `AMENDMENTS.md`). |
| `enable_spaces` | `bool` | `true` | — | Enable Static Spaces creation, management, and the second loopback HTTP server. |
| `enable_bridge_discretion` | `bool` | `true` | — | Enforce per-chat `side-chats/<chat-id>/DISCRETION.md` outbound filtering and untrusted external content fencing on Slack, Telegram, and Discord bridges. |
| `forget_redact_audit` | `bool` | `true` | — | Redact audit log entries matching a confirmed `ForgetPlan` and omit raw request strings from Forget audit events. |
| `heartbeat_interval_secs` | `u64` | `300` | `>= 60` | Polling interval in seconds for `HeartbeatWorker`. |
| `heartbeat_daily_token_cap` | `u64` | `200000` | — | Maximum daily token budget allocated to background heartbeat/dream jobs. |
| `nightly_dream_hour` | `u32` | `3` | `0..=23` | Local hour (`0`–`23`) at which the nightly `MemoryUpkeepJob` runs when `enable_dreams = true`. |
| `default_timezone` | `String` | `"UTC"` | — | Default IANA timezone for Muse prompt metadata and scheduled jobs. |
| `context_tokens` | `usize` | `128000` | `>= 8000` | Context window budget passed to `ContextCompactor` and prompt assembly. |
| `subagent_fanout_depth` | `u8` | `1` | `<= 2` | Maximum A2A subagent delegation depth (`RULE-BOT-DEPTH`). Depth-1 subagents cannot spawn further subagents when set to `1`. |
| `spaces_port` | `Option<u16>` | `None` | — | Optional explicit loopback port for the Static Spaces HTTP server. Defaults to `companion_port + 1` (e.g., `7434` when Companion binds `7433`). |
| `artifact_require_publish_confirmation` | `bool` | `true` | — | Require `ActionTier::Irreversible` confirmation before publishing or deleting a Space. |

---

## 3. Verbatim Upstream Assets & `Runtime Adaptation` Model

- **Verbatim Upstream Preservation (`crates/gm-muse/assets/upstream/`)**:
  - Upstream reference files (`SYSTEM_PROMPT.md`, workspace templates, `docs/*.md`, and bundled `skills/*/SKILL.md` + `skill.yaml`) are stored verbatim and verified by SHA-256 checksums in `crates/gm-muse/assets/UPSTREAM.md` and `crates/gm-muse/tests/no_pii.rs`.
  - The sole modification to upstream text is the removal of the repository uploader's personal name from `workspace/USER.md`.
- **Two-Message Cache-Friendly Prompt Assembly (`MusePromptBuilder`)**:
  - `MusePromptBuilder::build_messages` returns two `role: "system"` messages `[stable_prefix, dynamic_tail]`:
    1. **`stable_prefix` (byte-identical across turns)**:
       - Verbatim upstream `SYSTEM_PROMPT.md` (`<identity_Lock>`, `<core_behavior>`, `<memory>`, `<skills_and_connectors>`, `<proactive_behavior>`, `<security_and_integrity>`, `<formatting_guidelines>`).
       - `## Runtime Adaptation (Grok-Muse)` block mapping upstream container paths (`/home/mash/workspace` -> `<storage_dir>`, `/etc/mash/skills` -> bundled skills + `<storage_dir>/skills`) and enforcing Grok-Muse's governance invariants (`ActionTier::Irreversible`, `SecretScrubber`, `allow_host_desktop = false` default, `workspace_vm.enabled = false` default).
       - `SOUL.md` and `IDENTITY.md` persona sections.
       - Sorted progressive-disclosure Skill & Connector inventory (`name`, `title`, `category`, `description` only — full `SKILL.md` bodies load on demand via `LoadSkill`).
    2. **`dynamic_tail` (per-turn context)**:
       - Untrusted-fenced `USER.md`, `MEMORY.md`, `PROACTIVE_PREFERENCES.md`, `FEEDBACK.md`, active Goal summaries, channel instructions, and surface/model metadata.
- **Explicit Overrides**:
  - Passing `gm chat --system "..."` or setting `muse.enable_muse_prompt = false` bypasses `MusePromptBuilder` completely.

---

## 4. Two-Phase `Forget` Scope & Excluded Disclosures

Muse's discretionary `Forget` workflow (`crates/gm-memory/src/forget.rs`) operates in two strict phases:

1. **Phase 1 — Redacted Planning (`gm forget <query>` / `POST /api/memory/forget/plan` / `ForgetPlan`)**:
   - Scans 9 storage domains:
     1. `PersonalMemoryStore` SQLite FTS5 records (`DariusRecord`)
     2. `ProvenanceStore` claims and citations (`ProvenanceClaim`)
     3. `USER.md`, `MEMORY.md`, `PROACTIVE_PREFERENCES.md`, `FEEDBACK.md` line matches
     4. `memory/` corpus files
     5. `dreams/` consolidation files
     6. `goals/` workspaces
     7. `side-chats/` logs and rules
     8. `skills/` custom skill files
     9. `openbot.sqlite` queue/routine rows, `audit.jsonl` lines (when `forget_redact_audit = true`), and `feed.sqlite` signal watermarks.
   - Persists `.forget-pending.json` containing **only** a 16-hex-character SHA-256 `request_fingerprint`, record/claim IDs, and line numbers + line SHA-256 digests. **Raw request text is never written to `.forget-pending.json` or audit logs.**
2. **Phase 2 — Confirmed Execution (`gm forget --confirm <plan_id>` / `POST /api/memory/forget/confirm` / `ForgetConfirm`)**:
   - Classified as `ActionTier::Irreversible` in `AgentLoop` (requires interactive operator approval).
   - Verifies line SHA-256 digests before removing matching lines or standalone files so unrelated memories on adjacent lines are preserved intact.
   - Runs a post-deletion verification sweep across all stores and reports `remaining` counts (`Completed` vs `PartialFailure`).
3. **Excluded Disclosures**:
   - Every `ForgetPlan` and `ForgetReport` explicitly discloses two excluded categories that are intentionally left untouched:
     - `migration/`: Immutable fenced import archives and SHA-256 import receipts.
     - `channel_transcripts`: Visible multi-user/bot conversation history in `#general` and other channels (use channel/session history controls to clear chat transcripts).

---

## 5. Static Spaces Two-Port Loopback Origin Model

To prevent user-authored or agent-generated HTML/JS in a Space from accessing the privileged Companion API (`/api/*`) or reading operator tokens:

- **Port 1 — Companion Web Server (`127.0.0.1:<companion_port>`, default `7433`)**:
  - Serves the Companion Web UI (`/`), `/api/*` REST endpoints, and `/ag-ui/events` SSE stream.
  - Enforces `Authorization: Bearer <token>` on all `/api/*` and `/ag-ui/*` routes and sends strict `Content-Security-Policy: frame-ancestors 'none'` headers.
- **Port 2 — Static Spaces Server (`127.0.0.1:<spaces_port>`, default `companion_port + 1` = `7434`)**:
  - Separate `axum` router bound strictly to `127.0.0.1:<spaces_port>` (loopback only; non-loopback binds are rejected).
  - Serves published/previewed static assets under `/spaces/<slug>` and `/spaces/<slug>/*path`.
  - Runs on a **distinct browser origin** (`http://127.0.0.1:7434` vs `http://127.0.0.1:7433`), isolating `localStorage`, `sessionStorage`, cookies, and same-origin `fetch()` calls from the Companion API.
  - Sends `Content-Security-Policy: default-src 'self' 'unsafe-inline' blob: data:; connect-src 'self'; frame-ancestors 'self' http://127.0.0.1:<companion_port> http://localhost:<companion_port>` so the Companion UI can embed `<iframe sandbox="allow-scripts" src="http://127.0.0.1:7434/spaces/<slug>/index.html">` without granting same-origin privileges.
  - Only `SpaceKind::Static` (`index.html`, CSS, client-side JS, SVG, JSON, images) is supported. `SpaceKind::Ts` (TypeScript/Bun server spaces) is rejected with `"TS Spaces are not supported yet"`.

---

## 6. Editing Persona, Proactive & Bridge Discretion Files

You can view and edit Muse's Markdown control files through three surfaces, all backed by SHA-256 compare-and-swap (`expected_sha256`) to prevent lost updates:

1. **Native Desktop UI (`gm-ui`)**:
   - Open **🪪 Identity & Memory** from the top navigation bar to edit `SOUL.md`, `IDENTITY.md`, `USER.md`, `PROACTIVE_PREFERENCES.md`, and `FEEDBACK.md`, run Portable Memory Import (dry-run preview + apply), execute two-phase `Forget` (plan + confirm), or edit per-chat Bridge `DISCRETION.md` rules.
   - Open **📰 Feed**, **🎯 Goals**, **🌐 Spaces**, and **🧩 Skills** windows from the top bar to manage proactive cards, goal workspaces, static spaces, and custom skills.
2. **Companion Web UI (`http://127.0.0.1:7433`)**:
   - Use the **Feed**, **Goals**, **Spaces**, **Skills**, and **Identity** tabs in the left navigation rail.
3. **CLI (`gm`)**:
   - `gm skills list | show <name> | amend <name> --note "..."`
   - `gm goals list | create <title> | show <slug> | pause <slug> | complete <slug>`
   - `gm spaces list | create <slug> | publish <slug> | delete <slug>`
   - `gm forget "<query>"` and `gm forget --confirm <plan_id>`
   - `gm import-memory <path> [--apply]`
   - `gm dream --now`
