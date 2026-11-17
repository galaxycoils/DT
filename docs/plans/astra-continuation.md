# Continuation Plan for GPT Astra: GrokBot UI/UX Parity & `v0.5.0` Release

> [!NOTE]
> **Checkpoint Pushed to GitHub**
> - **Repository:** `galaxycoils/grok-muse` (`https://github.com/galaxycoils/grok-muse`)
> - **Working Branch:** `feat/grokbot-ui-parity` (pushed to `origin/feat/grokbot-ui-parity`)
> - **Checkpoint Commit:** `2f5d3515358664a7863f14371c25476f62e88b0b`
> - **Base (`master`):** Untouched at `6283428` (`v0.4.0`), so `.github/workflows/ci.yml` on `master` remains green while the remaining `gm-ui` compile fix and `companion_ui` rewrite are completed.
> - **Approved Design Spec:** [grokbot_ui_plan.md](file:///home/antigravity/.gemini/jetski/brain/v1/default/jetski_remote_agents_3p/e033e246-5f78-4534-865f-2673fbe0c459/grokbot_ui_plan.md)

---

## 0. Start Here (Checkout & Environment Setup)

```bash
git fetch origin
git checkout feat/grokbot-ui-parity
git pull origin feat/grokbot-ui-parity
```

### Verified Environment Rules (Linux Sandbox)
- **Toolchain env var:** Always prefix Cargo/rustc calls with `RUSTUP_TOOLCHAIN=stable` when `/usr/local/rustup/tmp` is read-only (verified: `cargo 1.99.1` / `rustc 1.99.1`).
- **Wayland PipeWire linker constraint in headless VMs:**
  - `gm-cu-helper` depends on `gm-platform` with `features = ["wayland-pipewire"]` (`pipewire -> libspa-sys`), and `gm-daemon` has `gm-cu-helper` in `[dev-dependencies]`.
  - If `libpipewire-0.3-dev` is absent in your environment, unfiltered `cargo test --workspace --all-targets` or `cargo test -p gm-daemon` will fail in `libspa-sys/build.rs`.
  - Use split checks locally (`--exclude gm-cu-helper` / `-p gm-daemon --lib --bins` for `check`/`build`, and let GitHub Actions Ubuntu CI—which installs `libpipewire-0.3-dev` per [.github/workflows/ci.yml](file:///home/antigravity/galaxycoils/grok-muse/.github/workflows/ci.yml#L46)—run full `--workspace`).
- **Sibling dependency:** `Cargo.toml` references `../Darius/crates/*`. Keep sibling checkout at `../Darius` (`f47a4ce980abecb751c9f5c79e1c424a20100433` in CI).

---

## 1. Current State of What Is Already Done & Verified

### Completed: Phase 1 — Backend, Storage & API Parity
All backend stores, IPC types, attribution, config, and Companion HTTP/SSE endpoints have been implemented, pushed in `2f5d351`, and pass compilation & unit tests:

| Crate | Completed Work | Verified Status |
| :--- | :--- | :--- |
| `gm-core` | Added `UiConfig` in [config.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/config.rs) (`theme`, `accent_mode`, `hardware_acceleration`, `computer_execution_mode`, `primary_bot_id`, notifications/quiet hours, privacy/updates toggles) and new `DaemonRequest`/`DaemonEvent` variants in [ipc.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/ipc.rs) (`GetBootstrapState`, DM/reactions/search, Library pagination, Sections, Vault, Custom Rules, Routines, Connectors, Teach-a-Task, Live Computer frame capture). | **25/25 unit tests pass** (`cargo test -p gm-core --lib`) |
| `gm-llm` | Extended `usage_v1` schema idempotently with `bot_id` & `source`, added `record_spend_attributed` & `usage_breakdown` in [budget.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/budget.rs), wired attributed reservations in [client.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/client.rs). | **19/19 unit tests pass** (`cargo test -p gm-llm --lib`) |
| `gm-openbot` | Added/updated stores: [sections.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/sections.rs), [direct_chat.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/direct_chat.rs), [vault.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/vault.rs) (non-secret metadata + CSV importer for 1Password/Chrome/Edge/Safari/Firefox/Bitwarden/LastPass), [rules.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/rules.rs) (20 `Allow` + 20 `Ask first` caps), [connectors.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/connectors.rs) (11 built-ins + Custom MCP), [library.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/library.rs), [interactions.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/interactions.rs), [roster.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/roster.rs), [routines.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/routines.rs), [channels.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/channels.rs), [templates.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/templates.rs), [supervisor.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/supervisor.rs) (`bootstrap_snapshot`), and [web.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs) routes & CSP (`frame-src http://127.0.0.1:<spaces_port>`). | **46/46 unit tests pass** (`cargo test -p gm-openbot --lib`) |
| `gm-goals` & `gm-platform` | Added `delete_deliverable` in [goal_workspace.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-goals/src/goal_workspace.rs); gated `pipewire` behind `wayland-pipewire` feature in [gm-platform/Cargo.toml](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-platform/Cargo.toml) & [portal.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-platform/src/linux/portal.rs) (`gm-cu-helper` opts in). | Verified via `gm-daemon` build check |
| `gm-daemon` | Wired all new `DaemonRequest` / `ManageSystem` ops and runtime deps into [control_server.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-daemon/src/control_server.rs) & [gm-daemon/src/lib.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-daemon/src/lib.rs). | **`cargo check -p gm-daemon --lib --bins` passes cleanly** |

### Partially Complete: Phase 2 (`gm-ui`) & Phase 3 (`companion_ui`)
- **`gm-ui`:** Switched [gm-ui/Cargo.toml](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/Cargo.toml) to `glow` default + opt-in `hardware-wgpu`, updated [native.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs), and split UI modules into `crates/gm-ui/src/ui/` (`mod.rs`, `state.rs`, `theme.rs`, `cache.rs`, `hotkeys.rs`, `rail.rs`, `sidebar.rs`, `chat.rs`, `details_drawer.rs`, `split_page.rs`, `workspace_views.rs`, `modals.rs`). **Currently fails to compile with 1 error (`E0599` for `ActiveModal::RoutineEditor` at [modals.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/modals.rs#L1101))** plus several contract mismatches and missing GrokBot parity items listed in Section 3 below.
- **`gm-openbot` Web Companion:** Added [companion_ui.css](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.css) (`1,137` bytes of CSS custom property tokens only). [companion_ui.html](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.html) (`21,505` B) and [companion_ui.js](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js) (`45,655` B) are **still the legacy v0.3.0 files** with `setInterval(..., 3000)` at [companion_ui.js](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js#L1304) and exceed the CSS+JS size budget (`1,137 + 45,655 = 46,792 B > 46,080 B`).

---

## 2. Exact Backend Contracts (Do Not Misread Keys)

Verify any handler against [supervisor.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/supervisor.rs#L556-L585) and [config.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/config.rs), because existing `gm-ui` code currently reads wrong keys in multiple places:

1. **`bootstrap_snapshot` JSON keys (`supervisor.rs:556-585`):**
   - `version` (string)
   - `status`: `{ helper_connected, paused, daily_spend_usd, daily_cap_usd, per_job_cap_usd, companion_port, spaces_port, allow_host_desktop }`
   - `ui`: serialized `UiConfig` (`/api/bootstrap` also merges `ui_settings`)
   - `bots`, `sections`, `channels`
   - **`recent_tasks`** (NOT `tasks`)
   - `routines`: `[RoutineState]` = `{ routine, next_run_utc, last_task_id }`
   - **`unread_counts`**: **flat object** with keys `"dm:<bot_id>"` and `"channel:<channel_id>"` (NOT nested `dms[id]` / `channels[id]`)
   - `bot_statuses`, `connected_app_count`
   - **`integrations`** (11 built-in `BuiltInIntegrationStatus`) + **`custom_connectors`** (`[CustomConnectorRecord]`) (NOT `connectors`)
   - **`feed`**: `{ posts, ideas, brief_prompt }` (NOT `activity`)
   - **`goals`**: flat `[Goal]` in bootstrap (whereas `ListGoals` / `GET /api/goals` returns `[{ goal, files }]` or flattened `GoalWithFiles` — normalize both!)
   - **`spaces`**: `[{ meta, url }]` in bootstrap (whereas `ListSpaces` returns `{ spaces, urls, spaces_port, enable_spaces }` — normalize both!)
   - `skills`, **`custom_rules`** (NOT `rules`), `vault_items`
2. **`UiConfig` enum values (`config.rs`):**
   - `theme`: `"system" | "dark" | "light"`
   - `accent_mode`: `"bot_color" | "black"`
   - `computer_execution_mode`: **`"ask" | "always" | "never"`** (NEVER send `"ask_first"`!)
3. **`BotProfile` UI fields (`roster.rs`):**
   - Color key is **`avatar_color`** (read `avatar_color` with fallback to `color`; always write `avatar_color` in `SetBotUiPrefs`). Also `avatar_shape`, `avatar_asset`, `accent_color`, `is_primary`, `pinned`, `section_id`, `hidden_from_sidebar` (NOT `hidden`), `notifications_enabled`, `drafts_enabled`, `task_mode_default`, `preferred_provider`, `preferred_model`.
4. **`CustomConnectorInput` (`connectors.rs`):**
   - Expects `name`, `transport` (`"remote_https" | "stdio_command"`), `url_or_command`, `args`, `auth_header_name`, `auth_secret`, `account_label`, `enabled`, `bot_ids` (NOT `kind` / `endpoint_url`).
5. **Library filters (`library.rs` & `web.rs`):**
   - Valid `filter` strings: **`"all" | "pinned" | "pages" | "media" | "links" | "files" | "apps"`** (`details_drawer.rs` currently sends invalid `"outputs"` and `"uploads"`).
6. **Web Companion Auth & SSE (`web.rs:331-358`):**
   - `GET /`, `GET /companion.css`, `GET /companion.js`, `GET /admin/audit` are unauthenticated.
   - All `/api/*` and `/ag-ui/*` routes enforce `Authorization: Bearer <token>`. Because browser `EventSource` cannot send `Authorization` headers, `companion_ui.js` MUST connect to `/ag-ui/events` using `fetch('/ag-ui/events', { headers: { Authorization: 'Bearer ' + token } })` and parse SSE frames incrementally from `response.body.getReader()`.

---

## 3. Remaining Work: Step-by-Step Execution Plan

### Step 1: Fix & Complete Native Desktop UI (`crates/gm-ui`)

Work through the modules in `crates/gm-ui/src/` to fix compilation, eliminate bootstrap/field bugs, and finish all GrokBot parity items:

1. **`crates/gm-ui/src/ui/state.rs`**
   - **Compile blocker:** Add `ActiveModal::RoutineEditor { ... }` and `ActiveModal::EditChannel { ... }` variants to [`ActiveModal`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/state.rs#L146).
   - Add `NavHistory` stack (`push`, `back`, `forward`, `can_back`, `can_forward`) for `(WorkspaceSection, chat_kind, target_id)`.
   - Add buffered `BotProfileDraft` (so Details Drawer doesn't fire `SaveBot` per keystroke) plus drafts for routines, channel editing, providers/models, notifications/quiet hours, computer helper/egress, explain memory, side-chat discretion, and custom connector fields (`transport`, `url_or_command`, `args`, `auth_header_name`, `auth_secret`, `account_label`).
   - Fix [`apply_smart_composer_edit`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/state.rs#L470-L512) so `"->"` is **NOT** converted to `"→"` inside inline `` `...` `` or fenced ```` ```...``` ```` code spans.
   - Ensure `dismiss_escape_layer` clears any secret buffers with `zeroize`.
2. **`crates/gm-ui/src/native.rs`**
   - Fix [`apply_bootstrap`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L247-L293) to read `recent_tasks`, `custom_rules`, `integrations` + `custom_connectors`, `feed`, `status`, `ui`, `skills`, `bot_statuses`, `connected_app_count`, `version`, and normalize `goals` & `spaces`.
   - Fix [line 837](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L837) (`!b["hidden_from_sidebar"]` instead of `!b["hidden"]`) and [line 886](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L886) (`avatar_color` instead of `color`).
   - Gate `palette.apply_to_context(ctx)` at [line 895](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L895) behind a dirty style key (`applied_style_key`) rather than mutating egui `Style` every frame.
   - Wire missing IPC responses & events: `ListRoutines`, `ListRoutineRuns`, `TriggerRoutineTest`, `DeleteRoutine`, `SaveRoutine`, `QueryAudit`, `ExplainMemory`, `GetDiscretion`, `ManageFeed`, `ManageSkill`, `ManageSpace`, `CheckUpdates`, `StartTeachTask`/`StopTeachTask`, `install_update`, `get_bot_avatar`, `get_routine_webhook_key`, `update_channel`, `pin_library_item`, plus `DaemonEvent::{RoutineUpdated, RoutineRunRecorded, TaskUpdated, ApprovalExpired, SecretPromptRequested, QuestionRequested, OrderApprovalRequested, BudgetUpdated, NotificationAlert, BotUpdated}`.
3. **`crates/gm-ui/src/ui/cache.rs` & `hotkeys.rs` & `rail.rs`**
   - Add `ParsedMarkdownTable::to_slack()` in `cache.rs`.
   - Complete hotkeys in `hotkeys.rs`: `Cmd/Ctrl+N`, `Cmd/Ctrl+B`, `Cmd/Ctrl+Shift+F`, `Cmd/Ctrl+Shift+A`, `Cmd/Ctrl+[` / `]`; evict `"lightbox:"` / `"live_computer:"` textures on Escape.
   - In `rail.rs`, dispatch `QueryAudit` + `ListQueue` when switching to `Audit`, and surface `connected_app_count` tooltip/badge.
4. **`crates/gm-ui/src/ui/sidebar.rs`**
   - Fix unread lookup at [line 294](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/sidebar.rs#L294) and [line 398](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/sidebar.rs#L398) to read flat `unread_counts["dm:<id>"]` and `unread_counts["channel:<id>"]`.
   - Add `+` menu (`Create Bot` / `New Group Chat`), `CompactRail` unread/attention dots + `< 900px` narrow overlay drawer toggle, `bot_statuses` (`status_phrase`, `needs_attention`), `Mute notifications` & `Export template` context menu items, and bottom `$spent / $cap` + daemon status footer.
5. **`crates/gm-ui/src/ui/chat.rs`**
   - Read `"avatar_color"` with `"color"` fallback at [line 83](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/chat.rs#L83).
   - Add `<` / `>` `NavHistory` buttons, `☰` toggle, channel `#topic`/participants/`EditChannel` button in header.
   - Support plain `Enter` send (`!shift && !ctrl && !alt && !command && !ime_composing && !mention_popup_open && !slash_popup_open`) and `Shift+Enter` newline **while keeping `take_composer_shortcut` & `Cmd/Ctrl+Enter` behavior and tests intact**.
   - Expand `@` / `/` popups to include `@everyone`, `@app:<id>`, `@routine:<id>`, and live `skills_data`.
   - Add `[Copy for Slack]` to table toolbar, file path import in `+ Attach`, and explicit renderers for all 14 GrokBot cards (adding explicit UI for `terminal_approval`, `ask_first`, `draft`, `order_approval`/`shopping`, `phone_call`/`meeting`, `connect_app`/`reauth`, and `initiative`, plus `Always allow` `"Saved to Custom Rules"` feedback).
6. **`crates/gm-ui/src/ui/details_drawer.rs` & `split_page.rs`**
   - Fix [details_drawer.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/details_drawer.rs#L138-L189) to buffer edits in `BotProfileDraft` with explicit `Save Changes`, add `preferred_provider`, `preferred_model`, `task_mode_default`, `notifications_enabled`, `Share ⌄`, `Upcoming Meetings`, full **Routines** section/tab (enable toggle, `Test` -> `Running…`, `RoutineEditor`, `Delete`, webhook URL + Keychain `x-webhook-key` copy, 20-run history), **Library** with all 7 valid filters ([line 401](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/details_drawer.rs#L401-L406)), `Pinned/Today/This week/Older` groups, and **Memory** (`ExplainMemory`, Two-Phase Forget explicit confirm checkbox + verification query, Side-Chat Discretion).
   - Fix [split_page.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/split_page.rs#L210-L217) to load/save Static Space HTML via `ManageSpace { op: "read" | "edit" }` and add `"Publish (Confirmed)"` / `"Unpublish"`.
7. **`crates/gm-ui/src/ui/workspace_views.rs`**
   - Complete `Feed` (brief prompt editor, manual post composer, 1–5 stars, Ideas Generate/Create/Run/Dismiss), `Goals` (normalize flat vs `{ goal }` shape, `stop_conditions`, `Abandon`), `Spaces` (normalize `[{ meta, url }]` vs `{ spaces, urls }`, `Publish`), and `Audit` (`QueryAudit` filter + Weekly/Monthly reports + Task `Retry`/`Resume`).
8. **`crates/gm-ui/src/ui/modals.rs` & `ui/mod.rs`**
   - Implement `ActiveModal::RoutineEditor` (replacing the stub at [line 1101](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/modals.rs#L1101)) and `EditChannel`.
   - Fix `"ask_first"` -> `"ask"` at [line 363](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/modals.rs#L363).
   - Fix `ConnectApps` ([line 873](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/modals.rs#L873-L942)) to show the 11 built-in `integrations`, use valid `CustomConnectorInput` fields, and include a Skills tab.
   - Add keyboard navigation (`↑`/`↓`/`Ctrl+N`/`Ctrl+P`/`Enter`) + SQLite `"Search older history"` in `CommandPalette`; finish all 5 Settings panes (`hardware_acceleration` restart banner, notifications/quiet hours, computer helper buttons, Vault 7 sub-tabs + CSV import, Usage `by_bot`/`by_source` + providers/models/egress, Updates `CheckUpdates`/Support Bundle/Feedback); add custom image upload (`<= 25 MB`) in `AvatarPicker`, zoom/nav/video speed in `Lightbox`, and update `Shortcuts`.
   - Expand unit tests in [ui/mod.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/ui/mod.rs) covering code-span `->`, `to_slack()`, Escape secret clearing, 10k-msg virtualization `< 1 ms`, and zero idle repaints.

**Phase 2 Verification Gate (`gm-ui`):**
```bash
RUSTUP_TOOLCHAIN=stable cargo clippy -p gm-ui --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test -p gm-ui
RUSTUP_TOOLCHAIN=stable cargo check -p gm-ui --features hardware-wgpu
```

---

### Step 2: Rewrite Web Companion & Expand `web.rs` Tests (`gm-openbot`)

Replace [companion_ui.html](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.html), [companion_ui.css](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.css), and [companion_ui.js](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js) with full GrokBot UI/UX parity matching [grokbot_ui_plan.md](file:///home/antigravity/.gemini/jetski/brain/v1/default/jetski_remote_agents_3p/e033e246-5f78-4534-865f-2673fbe0c459/grokbot_ui_plan.md):

#### Non-Negotiable Constraints
1. **Strict Bundle Size Budget:** `companion_ui.css` + `companion_ui.js` **MUST be `< 45 * 1024` (`< 46,080`) bytes raw combined**. Check continuously with `wc -c`. Keep HTML lean as well, and **zero** third-party frameworks or external CDN scripts/fonts.
2. **Zero `.innerHTML` (`RULE-XSS`):** Build every DOM node strictly via `document.createElement` / `createTextNode` / `textContent` (verify `!js.contains("innerHTML")` programmatically and in Rust tests).
3. **Zero `setInterval` Polling:** Delete `setInterval(..., 3000)`. On startup / token connect, fetch `GET /api/bootstrap` once, then open a single authenticated SSE stream via `fetch('/ag-ui/events', { headers: { Authorization: 'Bearer ' + token } })` + `ReadableStream` chunk parser with jittered exponential backoff (`1s -> 2s -> 5s -> 15s`). Throttle/pause Live Computer frame refreshes when `document.hidden === true`.
4. **Origin-Isolated Split Space `<iframe>` (`RULE-CSP-001`):** Render `#space-iframe` with `sandbox="allow-scripts"`, `src` pointing strictly to `http://127.0.0.1:<spaces_port>/spaces/v2/<slug>/?token=...`, and push theme updates via `postMessage({ type: 'hatch:space:theme', theme }, '*')`.
5. **Security:** Secret Prompt & Vault password fields use `<input type="password" autocomplete="off">`, wipe JS variables immediately on submit/cancel, and never store tokens or secrets in `localStorage`.
6. **Full UI Parity:** 48px Workspace Rail (bottom bar `< 768px`) across `Bots | Feed | Goals | Spaces | Audit`, Adaptive Bot Sidebar (`<= 1` auto-collapsed, `2..=4` `60px` icon rail, `>= 5` `240px` + overlay drawer `< 900px`), Smart Composer (`->` -> `→` outside code spans, `Backspace` undo, `Enter`/`Shift+Enter` with IME guard, `@`/`/` autocomplete, `Voice <-> Send`, attachments), all 14 interactive cards, 340px `Bot Details` Drawer (4 tabs + Routines + Webhook/20-run history + Teach-a-Task + Library 7 filters + Memory CAS/Provenance/Forget/Discretion), Live Computer modal, and all 6 Global Modals (`Cmd/Ctrl+K`, 5-pane Settings, Connect Apps + Skills, Shortcuts, Avatar Picker, Lightbox + Expanded Table) + 5-tier Escape stack.

#### Expand `web.rs` Tests
In [crates/gm-openbot/src/web.rs](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs), add/expand unit tests asserting:
- `COMPANION_UI_CSS.len() + COMPANION_UI_JS.len() < 45 * 1024`
- `!COMPANION_UI_JS.contains("setInterval")` and `!COMPANION_UI_JS.contains("innerHTML")`
- `COMPANION_UI_HTML.contains("id=\"space-iframe\"")` and `sandbox="allow-scripts"`
- HTTP checks for `GET /companion.css`, `GET /companion.js`, CSP headers (`frame-src http://127.0.0.1:<spaces_port>`), and `/ag-ui/events` SSE event delivery + `401` when unauthenticated.

**Phase 3 Verification Gate (`gm-openbot`):**
```bash
node --check crates/gm-openbot/src/companion_ui.js
wc -c crates/gm-openbot/src/companion_ui.html crates/gm-openbot/src/companion_ui.css crates/gm-openbot/src/companion_ui.js
RUSTUP_TOOLCHAIN=stable cargo clippy -p gm-openbot --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test -p gm-openbot --lib
```

---

### Step 3: Bump to `0.5.0`, Full Verification, Live Smoke Test, Merge & GitHub Release `v0.5.0`

1. **Bump workspace version:** Edit [Cargo.toml](file:///home/antigravity/galaxycoils/grok-muse/Cargo.toml#L23) from `version = "0.3.0"` to `version = "0.5.0"`.
2. **Format & Full Checks:**
   ```bash
   RUSTUP_TOOLCHAIN=stable cargo fmt --all
   RUSTUP_TOOLCHAIN=stable cargo fmt --all -- --check
   # If libpipewire-0.3-dev IS installed on Astra's machine:
   RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
   RUSTUP_TOOLCHAIN=stable cargo test --workspace
   # If libpipewire-0.3-dev is NOT installed:
   RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --exclude gm-cu-helper --exclude gm-daemon --all-targets -- -D warnings
   RUSTUP_TOOLCHAIN=stable cargo clippy -p gm-daemon --lib --bins -- -D warnings
   RUSTUP_TOOLCHAIN=stable cargo test --workspace --exclude gm-cu-helper --exclude gm-daemon --lib --bins
   ```
3. **Live Two-Port `gm-daemon` Smoke Test (Isolated Temp Dir):**
   - Start `gm-daemon` in an isolated `/tmp` config/storage directory binding two distinct ephemeral loopback ports (`companion_port` + `spaces_port`).
   - Verify `curl` responses on:
     - `GET /` (status `200`, strict CSP with `frame-src http://127.0.0.1:<spaces_port>`)
     - `GET /companion.css` & `GET /companion.js` (`200`, correct MIME types)
     - `GET /api/bootstrap` (`401` without token, `200` with `Bearer <token>`)
     - `GET /ag-ui/events` SSE connection
     - Creating + serving a Static Space on `spaces_port`
   - Stop daemon cleanly and remove temp directory.
4. **Commit, Merge/Push `master`, & Deploy GitHub Release `v0.5.0`:**
   - Commit remaining changes on `feat/grokbot-ui-parity` and verify GitHub Actions CI passes (or fast-forward / merge `feat/grokbot-ui-parity` into `master` via PR / `git push origin master`).
   - Tag and publish GitHub Release `v0.5.0`:
     ```bash
     gh release create v0.5.0 --target master --title "v0.5.0 — GrokBot UI/UX Parity & Ultra-Lightweight Engine" --notes "..."
     ```
