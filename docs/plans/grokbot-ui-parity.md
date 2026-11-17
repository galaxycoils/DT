# Grok Bot UI/UX Parity & Ultra-Lightweight Architecture Plan

> [!IMPORTANT]
> **Planning Scope & Locked Architectural Decisions**
> - **Parity Surfaces:** Matching lightweight Grok Bot UX across **both** the Native Desktop app ([`gm-ui`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs)) and the Local Web Companion ([`companion_ui.html`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.html) / [`companion_ui.js`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js)).
> - **Native Renderer:** [`eframe`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/Cargo.toml#L17) switched to **OpenGL ES / Desktop GL (`glow`) default** with optional **`wgpu` feature** controlled by Settings → `"Use hardware acceleration"` (requires restart), paired with **100% event-driven repaint** (`~0%` idle CPU) and zero polling timers.
> - **Navigation Layout:** Exact Grok Bot 3-pane responsive Bot/Group chat chrome, plus a **slim 48px left workspace icon rail** for switching cleanly between **Bots**, **Feed**, **Goals**, **Static Spaces**, and **Governance Audit** (plus bottom triggers for **Connect Apps**, **Shortcuts**, **Settings**, and **Emergency Kill**).
> - **Feature Completeness:** Every Grok Bot card, drawer, shortcut, custom approval rule, Vault form/CSV importer, Routine webhook/20-run history, Live Computer view, Teach-a-Task recorder, and Split Page workflow is built and backed by local stores + OS Keychain, displaying explicit `"Setup / Connect bridge required"` states wherever cloud adapters (Meetings, Phone/Voice, Payments, Slack/Teams OAuth) are not yet linked.

---

## 1. Root-Cause Audit of Current Bloat & Efficiency Targets

### Why the Current UI Consumes Excessive CPU, RAM, and GPU
1. **Unconditional Background Polling ([`native.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L1901-L1971)):**
   `background()` spawns a 3-second `tokio::time::interval` loop firing **8 full JSON RPCs** (`ListQueue`, `ListRoutines`, `ListChannels`, `ChannelHistory`, `ListBots`, `ListActivity`, `ListGoals`, `ListSpaces`) continuously—even when idle, minimized, or when popup windows are closed.
2. **Duplicate Web Polling ([`companion_ui.js`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js#L1304)):**
   Despite `/ag-ui/events` SSE already existing in [`web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs#L402), the browser companion still runs `setInterval(..., 3000)` and reconstructs entire DOM subtrees via `replaceChildren()` on every tick.
3. **Heavyweight `wgpu` All-Backends Default ([`crates/gm-ui/Cargo.toml`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/Cargo.toml#L16-L17)):**
   Compiles `dx12`, `metal`, `gles`, `vulkan`, and `wgsl` via `wgpu 27` unconditionally, allocating large GPU staging buffers and swapchain heaps at startup (`~90–160 MB` RSS baseline).
4. **Per-Frame Allocations & Re-Parsing ([`ViewState`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L114-L157) & [`NativeApp::render_muse_parts`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native.rs#L339-L419)):**
   Clones entire `Vec<Value>` and `Vec<(String, String)>` lists under mutex, runs `parse_message_bubbles()` every frame on every visible transcript message, and exposes cluttered floating `egui::Window` popups instead of one virtualized layout.

### Concrete Resource Budgets & Enforcement Rules

| Metric | Target | Enforcement Mechanism |
| :--- | :--- | :--- |
| **Idle CPU (`gm-ui`)** | **`< 0.1%` (0 Hz repaints)** | Never call `request_repaint()` inside `update()`; invoke `ctx.request_repaint()` exclusively on daemon push events, user input, active token streaming, or open live computer stream. Never spawn periodic UI polling timers. |
| **Idle Resident RAM (`gm-ui`)** | **`35–55 MB` RSS** | `eframe` `glow` default backend; capped **`16 MB`** LRU decoded texture cache (`max 32` textures); vector-painted Bot shapes (`Circle`, `Hex`, `Square`, `Diamond`, `Pill`, `Spark`); lazy pagination (`limit=5` Library batches). |
| **Frame Time (10,000-msg chat)** | **`< 1.0 ms` draw CPU** | `egui::ScrollArea::vertical().show_rows(...)` virtual windowing + memoized `ParsedMessageCache` keyed by `(message_id, content_sha_or_len, width_bucket)`. |
| **Screen Capture Overhead** | **`0%` when viewer closed** | `CaptureLatestFrame` / `CaptureTaskFrame` runs **only** while the Live Computer viewport is visible (adaptive `2–5 FPS` JPEG delta, texture freed on close). |
| **Web Companion Bundle** | **`< 45 KB` raw CSS+JS** | Zero third-party JS/CSS frameworks; single `companion_ui.html`, `companion_ui.css`, `companion_ui.js` bundle; single SSE `/ag-ui/events` connection with incremental DOM node keying and `document.hidden` throttling. |
| **Tray Mode Memory** | **`< 20 MB` RSS** | [`mac_ui.m`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/mac_ui.m) tray/hotkey mode launches without initializing OpenGL/`eframe` until the main window is summoned. |

---

## 2. Exact Visual Chrome, Responsive Layout & Design Tokens

```text
┌───┬──────────────────────────┬─────────────────────────────────────────┬──────────────────────────────┐
│ R │  BOTS & GROUPS (240px)   │  ACTIVE WORKSPACE / CHAT (Fluid)        │  BOT DETAILS / SPLIT PAGE    │
│ A │ ┌──────────────────────┐ │ ┌─────────────────────────────────────┐ │  (Slide-over 340px / 48%     │
│ I │ │ Grok-Muse  [+] [🔍]  │ │ │ [<][>] ☰  [●] Muse Operator ⌄       │ │   when Page Split active)    │
│ L │ ├──────────────────────┤ │ │           [🖥 Computer: Idle] [◫][ⓘ] │ │ ┌──────────────────────────┐ │
│   │ │ ★ PINNED             │ │ ├─────────────────────────────────────┤ │ │      ( Vector Avatar )   │ │
│ 4 │ │  [⚡] Muse Operator  │ │ │ ⚠ Budget / Offline / App Alert Bar  │ │ │  Muse Operator  [Share⌄] │ │
│ 8 │ │      Checking mail.. │ │ ├─────────────────────────────────────┤ │ │  Desktop Coordinator ✎   │ │
│ p │ │  [#] #general    ●2  │ │ │   (760px centered virtual scroll)   │ │ ├──────────────────────────┤ │
│ x │ │                      │ │ │                                     │ │ │[Overvw][Routns][Lib][Id] │ │
│   │ │ ▾ WORK               │ │ │  [⚡] Summary ready [1]             │ │ │                          │ │
│[⚡]│ │  [🔭] Nova Research  │ │ │   ┌─────────────────────────────┐   │ │ │ ▸ Teach a task (Record)  │ │
│[📰]│ │  [🛠️] Atlas Eng  🟡  │ │ │   │ Approval: Delete old logs?  │   │ │ │ ▸ Capabilities (GUI/Sh)  │ │
│[🎯]│ │                      │ │ │   │ [Allow once][Always allow]  │   │ │ │ ▸ Upcoming Meetings (0)  │ │
│[◫]│ │ ▸ HIDDEN BOTS (1)    │ │ │   │ [Deny]                      │   │ │ │ ▸ Routines (2)  [+ New]  │ │
│[🛡️]│ │                      │ │ │   └─────────────────────────────┘   │ │ │   • Morning Brief 08:30  │ │
│   │ │ 🔌 Connect apps  (4) │ │ │                       ╭───────────╮ │ │ │   • Inbox Triage  Webhook│ │
│ : │ ├──────────────────────┤ │ │                       │ Do it now │ │ │ │ ▸ Library [All|Pages|..] │ │
│   │ │ [$1.24 / $5.00]      │ │ ├───────────────────────┴───────────╯─┤ │ │   Pinned / Today (5 max) │ │
│[🔌]│ │ ● Daemon  ⟳ Update   │ │ │ ⚠ Request failed [Copy req ID] [✕] │ │ │ ▸ Identity / Memory CAS  │ │
│[?]│ └──────────────────────┘ │ │ [📄 Page: Q4 Plan ✕] [📎 doc.pdf ✕] │ │ │   SOUL / USER / Forget   │ │
│[⚙]│                          │ │ │ ┌─────────────────────────────────┐ │ │ └──────────────────────────┘ │
│[🛑]│                          │ │ │ │ Message @muse (/ skill, @ app)  │ │ │                              │
└───┴──────────────────────────┴─│ │ [+] [Queue|Chat] [🎙]  [∿∿∿ Voice] │ │──────────────────────────────┘
                                 └─────────────────────────────────────┘
```

### 2.1 Responsive Collapse Rules (Exact Grok Bot Parity)
- **Workspace Rail (`48px`):** Always pinned on far left on desktop (`>= 768px`); collapses into a clean bottom tab strip (`Bots | Feed | Goals | Spaces | Audit`) on mobile/narrow viewports (`< 768px`).
  - Rail Icons:
    1. `Bots` — Grok Bot workspace (primary default).
    2. `Feed` — Proactive patrol posts, topic prompt, star ratings, Ideas (`Run`/`Dismiss`).
    3. `Goals` — Hierarchical Goals (`max depth 1`), milestone checklist, deliverables preview.
    4. `Spaces` — Static Spaces gallery + split-pane live HTML/Space viewer (`http://127.0.0.1:<spaces_port>`).
    5. `Audit` — Governance audit feed (`All | Permitted | Refused | Failed`), Weekly/Monthly report generator.
  - Bottom Rail Utility Icons: `Connect Apps` (`Cmd/Ctrl+Shift+M`), `Shortcuts` (`Cmd/Ctrl+/`), `Settings` (`Cmd/Ctrl+,`), `Kill Switch` (`Cmd+Alt+Escape`).
- **Bot Sidebar Adaptive States:**
  - `total_visible_chats <= 1`: Auto-collapsed behind `☰` toggle (`Cmd/Ctrl+B`).
  - `2..=4` visible chats (on desktop): `60px` icon-only rail showing shape/color avatars + attention dots; expands to `240px` on `Cmd/Ctrl+B` or hover-expand pin.
  - `5+` visible chats (or when sections/groups exist): Full `240px` sidebar; overlays on narrow widths (`< 900px`).

### 2.2 Color Palette & Theme Tokens (Exact Dual-Theme + Accent System)
Apply identical CSS custom properties (`companion_ui.css`) and `ThemeTokens` structs (`crates/gm-ui/src/theme.rs`):
- **Dark Theme (Grok Bot crisp Obsidian-Slate):**
  - `--bg-app`: `#0B0C0E` | `--bg-rail`: `#08090B` | `--bg-sidebar`: `#101215` | `--bg-surface`: `#16191D` | `--bg-elevated`: `#1E2228` | `--bg-card`: `#14171C`
  - `--border-subtle`: `#242932` | `--border-strong`: `#343B48`
  - `--text-primary`: `#F3F4F6` | `--text-secondary`: `#9CA3AF` | `--text-muted`: `#6B7280`
- **Light Theme (Clean Paper-White iMessage Contrast):**
  - `--bg-app`: `#FFFFFF` | `--bg-rail`: `#F4F5F7` | `--bg-sidebar`: `#F8F9FB` | `--bg-surface`: `#FFFFFF` | `--bg-elevated`: `#F1F3F6` | `--bg-card`: `#F9FAFB`
  - `--border-subtle`: `#E5E7EB` | `--border-strong`: `#D1D5DB`
  - `--text-primary`: `#111827` | `--text-secondary`: `#4B5563` | `--text-muted`: `#9CA3AF`
- **Accent Preference (`Settings -> General -> Accent`):**
  - `"bot_color"`: Outgoing user bubbles & active pills tint to active Bot's palette color (`Electric Blue #2F7CF6`, `Emerald #10B981`, `Amber #F59E0B`, `Violet #8B5CF6`, `Rose #F43F5E`, `Teal #14B8A6`, `Indigo #6366F1`, `Coral #FB7185`, `Slate #64748B`, `Cyan #06B6D4`).
  - `"black"` (Monochrome): Outgoing user bubbles use solid Ink `#1C1F26` (Light mode) / `#272B36` (Dark mode) with crisp `#FFFFFF` text.
- **Status Semantics:**
  - `Unread` dot: `#2F7CF6` (8px) | `Needs attention` badge: `#F59E0B` | `Working` indicator: animated shimmer phrase | `Permitted`: `#22C55E` | `Refused / Kill`: `#EF4444` | `Warning / Expired`: `#F59E0B`.

---

## 3. Complete Grok Bot Feature & Option Parity Matrix

Every single feature from Grok Bot docs + Muse extensions must work identically across Native Desktop (`gm-ui`) and Web Companion (`companion_ui`):

### 3.1 Sidebar, Bots, Sections & Group Chats
1. **Create Bot (`+` → `Create a Bot`, `Cmd/Ctrl+N`):**
   - Name, Role/Subtitle, Vector Shape (`Circle`, `Hex`, `RoundedSquare`, `Diamond`, `Pill`, `Spark`), Color from 10-swatch palette (OR custom image `Generate` / `Upload <= 25MB`), Capabilities (`Allow GUI`, `Allow Shell`, `Allow Network`), Model/Provider override (`Default` / `Meta Muse` / `xAI Grok` / `Claude` / `Codex`).
   - Also supports **`Browse Templates / Import Bot`** (import/export shareable JSON Bot template, strips any secret refs).
2. **Primary Bot:** Marked with `★` star (`muse` by default, switchable to any owned Bot via context menu `"Set as primary Bot"`). Initiative / check-in cards route here.
3. **Organization Controls (Right-click / `⋯` menu on any Bot or Group):**
   - `Pin to top` / `Unpin`
   - `Move to <Section>` / `Move to new section` (creates collapsible sidebar folder stored in SQLite)
   - `Duplicate Bot` (clones prompt/identity template & settings, empty history)
   - `Hide from sidebar` / `Show in sidebar` (populates collapsible `Hidden Bots (N)` footer section)
   - `Mute notifications` toggle
   - `Share template / Export JSON` (`Create a copy` / `Update template` / `Delete`)
   - `Delete Bot` (disabled for primary `muse` default safety guard or reassign primary first)
4. **Group Chats (`+` → `New Group Chat`):**
   - Multi-bot room backed by [`ChannelStore`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/channels.rs#L67); participant avatar stack in header, topic & instructions editor, `@bot` & `@everyone` mention autocomplete, group approval rule ("only requestor/owner can confirm, `Group` card styling without per-user Edit/Reset hooks"), pause/resume/archive/restore controls.
5. **Sidebar Bot Filter (`Cmd/Ctrl+Shift+F`):** Instant local fuzzy filter across visible + hidden bots & groups.

### 3.2 Chat Workspace, Composer, Threading & Shortcuts
1. **Navigation History:** In-memory stack `(view, chat_id)` navigable via `Cmd/Ctrl+[` (Back) & `Cmd/Ctrl+]` (Forward) and header chevron buttons.
2. **Message Rendering & Tables:**
   - All messages pass once through [`parse_message_bubbles`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/muse_render.rs#L9) (`<message_break/>`, `sandbox://` attachments, `【...】` citations, `<options>` quick replies) plus GFM markdown parser (headings, bold/italic, inline code, fenced code blocks with `Copy` button, links, blockquotes, and **Tables**).
   - **Table Toolbar (Grok Bot v1.0 parity):** Hovering any Markdown table reveals `[Copy Markdown]`, `[Copy for Slack]` (tab/Slack-formatted plain text), and `[Expand ⤢]` (full-screen table modal with sticky headers & horizontal scroll).
   - **Unclosed / Streaming Safety:** Unclosed `<options>` or code fences stream smoothly without raw flicker; `Stop generating` pill cancels in-flight stream cleanly.
3. **Hover Message Actions:**
   - `Reply in thread` (opens threaded reply drawer or inline reply reference)
   - `React` (6-emoji quick bar `👍 ❤️ 😂 🎉 👀 🚀` + full picker backed by [`ChannelStore`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/channels.rs#L109) + DM reaction table)
   - `Copy text / Markdown`
   - `Copy request ID` (links to audit/trace entry)
4. **Smart Composer:**
   - **Auto-Arrow Conversion:** Typing `->` outside fenced/inline code converts immediately to `→` (undoable via single Backspace).
   - **IME Composition Safety:** Strictly ignores `Enter` submit when IME composition is active (`is_composing` in JS / `Ime::Preedit` in egui).
   - **`Enter` vs `Shift+Enter`:** `Enter` sends (if not composing), `Shift+Enter` adds newline.
   - **Dynamic Right Button:** Displays **`Start Voice Chat` waveform pill** when composer text is empty and no attachments staged (opens Voice overlay / audio state, or shows `"Configure STT/TTS voice bridge"` setup prompt when unlinked); switches smoothly to accent **`Send ↑`** arrow as soon as text or attachments are added.
   - **`@` Mention Popover & `/` Skill Picker:**
     - Typing `@` lists `@bot`, `@everyone` (in groups), `@routine:<name>`, `@app:<connector>`.
     - Typing `/` queries [`SkillRegistry`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-muse/src/skills.rs) (both built-in Muse skills and custom user skills).
   - **Dictation (`Cmd/Ctrl+D`):** Toggles speech-to-text dictation pill inside composer (bridges to OS Speech / Web Speech API where available, or shows `"Enable Microphone permission / Local Whisper adapter"` notice).
   - **Attachments (`+` / `Cmd+Shift+A` / Drag-and-Drop / Clipboard Paste):** Stages local files via [`DaemonRequest::ImportAttachment`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/ipc.rs#L130) with removable preview chips (`<25 MB` image preview support).

### 3.3 All 14 Inline Card Types (Interactive & Event-Driven)
All cards are stored as structured `ChatCardPayload` metadata alongside the message record so state (`pending`, `approved`, `denied`, `expired`) persists across app restarts:

| # | Card Type | Controls & Exact UX Copy | Backend / Security Contract |
| :--- | :--- | :--- | :--- |
| 1 | **Action Approval** | Title, tier badge (`Read` / `Reversible` / `Irreversible`), rule explanation, `[Allow once]`, `[Always allow]`, `[Deny]`. Clicking `Always allow` appends a pattern rule and shows inline toast `"Saved to Custom Rules"`. Shows `Expired` muted state if timed out. | Calls `RespondApproval`; if `Always allow`, persists rule via `ApprovalRulesStore` while never overriding [`BlocklistManager`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-safety/src/blocklist.rs) or [`ActionTier`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-safety/src/tiers.rs#L5) irreversible safety. |
| 2 | **Terminal / Shell Approval** | Monospace command block + CWD badge; `[Allow]`, `[Skip]`, `[Manage settings]` (deep-links to `Settings -> General -> Custom Rules`). | Evaluates Custom Rules first; logs to [`AuditLogger`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-safety/src/audit.rs#L22). |
| 3 | **Ask-First Rule Trigger** | Triggered by user `"Ask first"` rules: `[Allow]`, `[Not now]`, `[Edit rule]`. | Updates single-execution permission or opens rule editor. |
| 4 | **Take Control / Wheel Handoff** | Blocker reason + instructions, `[Take over]` (opens Live Computer view & pauses automation) → `[I'm done]` + optional handback note. | Calls [`TakeTheWheelResume`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/ipc.rs#L177). |
| 5 | **Secure Form & Secret Card** | Masked `●●●●●` input fields inside card (`Username`, `Password`, `API Key`, `2FA`), toggle reveal eye, optional checkbox `"Save securely to Bot's Vault"`, `[Submit securely]`, `[Cancel]`. | **Zero LLM Exposure:** Submitted out-of-band directly to `DaemonRequest::SubmitSecretPrompt` → stored in [`KeychainSecrets`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/secrets.rs#L86) and injected by `VaultSecretResolver`; only `[Secret provided]` enters chat history. |
| 6 | **Question Card** | Numbered multi-step or single-choice selector with keyboard `1-9` shortcuts + `"Something else"` IME-safe custom text box; works cleanly with Muse `<options>`. | Sends `RespondQuestion` or enqueues selected choice. |
| 7 | **Draft Card** | Email (`To`, `CC`, `Subject`, `Body`, `Attachments`, `Signature` preview) OR Slack/Channel post (`#channel`), `[Send]` / `[Send message]`, `[Edit inline]`, `[Discard]`, and `[Needs approval]` lock switch. | Routes to configured mail/channel adapter or shows `"Connect Gmail/Slack in Connect Apps"` CTA if adapter unlinked. |
| 8 | **Page / Space Card** | Icon + Page Title + `[Updated]` or `[Published]` badge + `[Open split view]` + `[Copy link]`. | Opens Split Page Workspace (`http://127.0.0.1:<spaces_port>/spaces/v2/<slug>/` on Web; safe Markdown/Space preview + external sandbox launch on Desktop). |
| 9 | **Media Card & Lightbox** | Inline image thumbnail / video player chip / file card (`sandbox://` validated). Clicking opens Lightbox modal (`←`/`→` gallery nav, Zoom `+`/`-`/`Reset`, `Download`, video speed `0.5x / 1x / 1.25x / 1.5x / 2x`). | Validated via [`sandbox_uri_to_bot_relpath`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/muse_render.rs#L15) (`RULE-PATH-002`). |
| 10 | **Shopping & Order Approval** | Product carousel (`Title`, `Price`, `Merchant`, `Image`) + Checkout approval card: `[Pay $XX.XX]`, `[Cancel]`, `[Check price again]`. | Uses Vault Default Payment Method summary; if no local checkout/Stripe bridge exists, displays `"Payment Checkout Adapter not connected"` setup state cleanly. |
| 11 | **Phone Call & Meeting Card** | Consent banner, call timer, live transcript stream, controls `[Take over]`, `[Send reply]`, `[Skip]`, `[End call]`, post-call summary & transcript drawer. | Displays active status if SIP/Meet adapter configured, or `"Connect Voice / Meeting Bridge in Connect Apps"` when unlinked. |
| 12 | **Subagent / A2A Exchange** | Delegated `@coder`/`@researcher` task card: collapsible step trace, status badge, files changed / deliverables pill, PR link chip. | Backed by [`A2aCoordinator`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/a2a.rs) & [`QueuedTask`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/queue.rs#L60). |
| 13 | **Connect App / Re-Auth Card** | App logo + reason (`"Gmail token expired"` / `"Link Slack account"`), `[Connect <App>]`, `[Dismiss]`. | Opens Connect Apps modal focused on that connector ID. |
| 14 | **Initiative / Check-In Offer** | Primary Bot proactive offer or Muse Feed card: `[Accept & Run]`, `[Edit prompt]`, `[Dismiss]`, `[Mute check-ins for Bot]`. | Connects to `gm-goals` Feed/Ideas + [`QuietGate`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-goals/src/quiet_gate.rs). |

### 3.4 Slide-Over Bot Details Drawer (`340px`, `Cmd+Alt+B` / `Ctrl+Alt+B`)
Triggered by clicking Bot Name in header or `ⓘ` toggle:
- **Header:** Clickable Avatar (`Avatar Picker Modal`: `Bot` [6 vector shapes × 10 palette colors], `Generate`, `Upload <= 25MB`), inline editable `Display Name` + `Role Label`, `Share` menu (`Create copy`, `Export JSON`, `Set primary`), `Mute Notifications` switch.
- **Tab 1 — `Overview`:**
  - Quick Capability Switches (`GUI`, `Shell`, `Network`), `Default Mode` (`Chat` / `Queue Task`), `Model / Provider` override.
  - **`Teach a Task`** button (launches 10-minute computer action/click recorder banner -> captures sequence of accessibility/GUI/shell steps -> synthesizes editable `SKILL.md` draft in Custom Skills modal!).
  - **`Upcoming Meetings`**: Countdown / `"Taking notes"` status with `Disable Interactivity (Muted)` / `Cancel` / `Quit` menu; if empty and calendar unlinked, shows `"Add to meeting"` starter chip & setup link.
- **Tab 2 — `Routines` (`50/bot` cap enforced):**
  - Routine list rows with enable switch, next run countdown, `Test` button (`Test` → `Running…` + instant toast), `Edit` button, `Delete` button.
  - **Routine Editor Form:** Plain-English schedule (`Interval >= 5 min`, `Daily at HH:MM`, `Weekdays`, IANA Timezone from `chrono_tz`, Quiet Hours), `Instruction`, **`Webhook Trigger` panel** (read-only copyable `POST` URL `http://127.0.0.1:<companion_port>/api/routines/<id>/trigger`, copyable `x-webhook-key` stored in Keychain, delivery dedup info).
  - **Run History Drawer (last 20 runs):** Lists [`RoutineRunRecord`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/routines.rs#L45) (`fired_at`, `status`: `queued` / `overlap_suppressed` / `skipped_quiet_hours`, linked `task_id`).
- **Tab 3 — `Library`:**
  - Filter tabs: `All` | `Pinned` | `Pages` | `Media` | `Links` | `Files` | `Apps`.
  - Grouped strictly under `Pinned`, `Today`, `This week`, `Older`.
  - Loads **5 items per batch** with `[Show more]` button (`0` eager bitmap decoding offscreen).
  - Supports Pin/Unpin item, preview in Split Page / Lightbox / Workspace File Viewer, reveal in Bot `outbox/`.
- **Tab 4 — `Identity & Memory` (Grok-Muse Native):**
  - File switcher: `SOUL.md`, `IDENTITY.md`, `USER.md`, `MEMORY.md`, `AGENTS.md`, `HEARTBEAT.md`, `TOOLS.md` with **CAS SHA-256** verification, conflict banner (`409` → `"Reload latest"`), and `SecretScrubber` guard.
  - **`Explain Memory`** input (`"Why does Muse know X?"` → provenance chain Viewer from [`ProvenanceStore`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-memory/src/provenance.rs)).
  - **`Two-Phase Forget`** workflow (`Plan` → anonymized entity/count preview → explicit checkbox + verification query → `Execute Confirm`).
  - **`Side-Chat Discretion Rules`** (`chat_id` selector, default non-editable rules + editable user rules + `loosens` safety check).

### 3.5 Live Computer Viewer & Split Page / Static Spaces View
- **Live Computer Drawer/Modal (`[🖥 Computer]` pill in Chat Header):**
  - Opens centered pop-over or docked upper pane.
  - Only **while open**: requests `CaptureLatestFrame` / `CaptureTaskFrame` at adaptive `2–5 FPS` (`max_long_edge: 1280, quality: 75`).
  - Toolbar: `[Watch Only]` vs `[Take Control / I'm Done]`, `[Emergency Stop]` (`Cmd+Alt+Escape`), `[Teach a Task ● REC (10:00 max)]`, active task step summary pill, VM vs Host Computer badge.
  - Closing the viewer immediately halts frame capture and frees the `ColorImage` GPU texture.
- **Split Page & Static Spaces (`[◫ Split Page]` or `[Spaces]` Rail View):**
  - In **Bots** view: opens side-by-side (`45%` right pane) showing either a Markdown Document Page (editable/viewable) or an isolated Static Space (`http://127.0.0.1:<spaces_port>/spaces/v2/<slug>/?token=...`).
  - **Origin Isolation Preserved (`RULE-CSP-001`):** Web Companion uses strict two-port `<iframe sandbox="allow-scripts">` matching [`SpacesManager`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/spaces.rs#L186) (`frame-src http://127.0.0.1:<spaces_port>` in [`web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs#L415)); Native Desktop renders Markdown pages directly and launches Static HTML Spaces in the system browser via tokenized URL + publishes `"Message about <Page Title>"` pill directly into the active composer!
  - Explicitly rejects `kind: "ts"` (`400 Bad Request`) and gates external publish behind explicit confirmation (`confirmed: true`).

### 3.6 Global Modals & Keyboard Palette

#### 1. Command & Global Search Palette (`Cmd/Ctrl+K`)
- Clean Spotlight-style modal with `Ctrl+N` / `Ctrl+P` (or `↑`/`↓`) navigation, `Enter` select, `Esc` dismiss:
  - Quick Jumps: Every Bot, Group, Workspace view (`Feed`, `Goals`, `Spaces`, `Audit`), Settings tabs, Connect Apps, New Bot, Browse Shared/Template Bots.
  - **2-Stage Message Search:** Initially searches cached recent messages locally (`"Searched recent messages"` subtle footer), plus actionable **`[Search older history]`** row that runs an indexed SQLite `FTS` / `LIKE` query in `openbot.sqlite`.

#### 2. 5-Pane Global Settings Modal (`Cmd/Ctrl+,`)
1. **`General`:**
   - **Account / Local Profile** & Multi-profile workspace path switcher.
   - **Appearance:** `Follow System` / `Light` / `Dark`; **Accent:** `Bot color` / `Black`; **Language** selector; **Timezone** (`chrono_tz` picker); **Spellcheck** toggle.
   - **Audio:** **Microphone input selector** + permission indicator.
   - **Performance:** **`Use hardware acceleration`** checkbox (`glow` OpenGL / `wgpu` vs software-friendly hint; displays `"Restart required to apply graphics backend change"` banner when changed).
   - **Notifications:** Global Desktop Alerts toggle, Alert sound toggle, Suppress when active toggle, Quiet Hours schedule.
   - **Approvals & Custom Rules:** **Auto-review** master toggle + **Custom Rules Manager** strictly validating max **20 Allow** + **20 Ask first** rules (`<= 1000 chars` each, rule pattern + scope + delete/edit). *(Note: immutable [`BlocklistManager`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-safety/src/blocklist.rs) & secret-leak guards always remain locked ON and displayed transparently.)*
   - **Import Memory (`USER.md` / `MEMORY.md`):** 2-step dry-run (`Report SHA-256`) + Apply box matching [`import_memory_portable_with_stores`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-memory/src/import.rs).
2. **`Computer`:**
   - Connected Computers table (`Host OS Helper`, `Local Workspace Sandbox / VM`, status pill, resolution/FPS).
   - **`Execution on this computer`:** Radio group (`Ask every time` / `Always` / `Never`), maps cleanly to [`SafetyConfig::allow_host_desktop`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/config.rs#L254).
   - **`Route traffic through this computer`** toggle + **Blocked Hosts / Egress Counter** (read from network policy & `AuditLogger`).
   - Lifecycle actions: `[Update helper]`, `[Recover sandbox]`, `[Reset Bot's computer]` (typed confirm modal).
3. **`Vault`:**
   - Masked Secret Stores backed strictly by [`KeychainSecrets`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/secrets.rs#L86) & [`VaultSecretResolver`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/secrets.rs#L23) — SQLite stores ONLY `{ id, kind, label, domain_or_last4, username, hint, secret_ref, bot_scope }`:
     - **Sub-tabs:** `Logins` | `Cards` | `Contact Info` | `Hints` | `Payments` (Default payment method selector) | `Password Managers` (`1Password CLI op://` toggle & status test) | `Import CSV` (`< 5 MB` parser supporting 1Password, Chrome, Edge, Safari, Firefox, Bitwarden, LastPass headers -> previews row count & stores passwords directly into Keychain).
     - `Hardware security keys` confirmation toggle (`Touch ID / WebAuthn / OS Prompt` before revealing or editing secrets).
4. **`Usage & Billing` (Mapped to Local [`BudgetManager`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/budget.rs#L21) & Providers):**
   - **Daily & Weekly Spend Bars:** Current `$X.XX / Daily Cap` and `7-Day Total`, Per-Job cap input, Remaining Daily Notifications counter.
   - **Charts / Breakdown Tables:** `By Bot` (`muse`, `researcher`, `coder`, `guardian`, custom bots) & `By Source` (`Direct Chat`, `Queued Tasks`, `Routines`, `A2A / Goals`), computed from extended `usage_v1` SQLite rows.
   - **Providers & API Keys:** Provider bases & model defaults for **Meta Muse**, **xAI Grok**, **Claude**, **Codex**, Screenshot Egress toggles (`allow_screenshot_egress_meta`, `allow_screenshot_egress_xai`), and Keychain API Key save/clear fields (`zeroize` cleared after submit).
5. **`Updates`:**
   - Current Version (`v0.4.0`+), `Check for Updates` / `Install & Restart to Update` wired to `crates/gm-daemon/src/updater.rs`, `Automatic updates when idle` toggle, Release notes link, plus **`Send Feedback`** modal with opt-in redacted diagnostics export (`crates/gm-daemon/src/diagnostics.rs`).

#### 3. Connect Apps & Skills Catalog (`Cmd/Ctrl+Shift+M`)
- Split tabs: **`Connected Apps (N)`** | **`Custom MCP Connectors`** | **`Skills Library (Built-in & Custom)`**.
- **Built-in Integrations & Bridges:** Shows status, multi-account switcher, and `Connect / Configure / Reconnect` modal for `Gmail`, `Google Calendar`, `Slack`, `Microsoft Teams`, `GitHub`, `Discord`, `Telegram`, `1Password`, `Email Inbox (@local / IMAP-SMTP bridge)`, `Voice / Meetings`, `Stripe / Payments`. Any unlinked integration displays clean setup steps + token/webhook inputs rather than hidden or broken buttons.
- **Custom Connector Builder:** Supports **`Remote HTTPS (MCP)`** (URL, auth header stored via `secret://keychain/...`) and **`Local Command (Stdio MCP)`** (binary/args validated cleanly), plus test-connection & toggle per Bot.
- **Skills Studio:** Searchable [`SkillRegistry`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-muse/src/skills.rs) + Create/Edit Custom Skill (`YAML + Markdown` with live `SecretScrubber` validation rejecting plaintext keys) + Import/Export `SKILL.md`.

#### 4. Keyboard Shortcuts Cheatsheet (`Cmd/Ctrl+/`) & Escape Layering Stack
- Implements Grok Bot's strict single-level **Escape Priority Stack**:
  1. Open Popover (`@` mention, `/` skill, emoji picker, context menu)
  2. Top Modal (`Lightbox`, `Avatar Picker`, `Command Palette`, `Settings`, `Connect Apps`, `Shortcuts`)
  3. Live Computer Viewer / Overlay
  4. Bot Details / Split Page Drawer
  5. Active Stream Generation (`Stop` on double-Esc or dedicated shortcut)
- Exact Desktop Keyboard Map:
  - `Cmd/Ctrl+N`: New Bot
  - `Cmd/Ctrl+K`: Command & Message Search Palette
  - `Cmd/Ctrl+B`: Toggle Sidebar Collapse
  - `Cmd+Alt+B` / `Ctrl+Alt+B`: Toggle Bot Details Drawer
  - `Cmd/Ctrl+,`: Open Global Settings
  - `Cmd/Ctrl+Shift+M`: Connect Apps & Skills
  - `Cmd/Ctrl+Shift+F`: Focus Sidebar Filter
  - `Cmd/Ctrl+Shift+A`: Attach File
  - `Cmd/Ctrl+D`: Toggle Dictation
  - `Cmd/Ctrl+[` / `]`: Back / Forward Navigation History
  - `Cmd/Ctrl+1..5`: Switch Workspace Rail (`Bots`, `Feed`, `Goals`, `Spaces`, `Audit`)
  - `Cmd/Ctrl+/`: Keyboard Shortcuts Cheatsheet
  - `Cmd+Alt+Escape`: Emergency Kill Switch (global OS hotkey preserved!)

---

## 4. Incremental Engineering & Crate-by-Crate File Plan

### Phase 1: Data Model, Budget/Vault/Rules Storage & Zero-Poll Event Bus (`gm-core`, `gm-llm`, `gm-safety`, `gm-openbot`, `gm-daemon`)

1. **[`crates/gm-core/src/config.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/config.rs):**
   - Add `UiConfig` with `#[serde(default)]` on `GmConfig` containing `theme`, `accent_mode`, `hardware_acceleration`, `sidebar_collapsed`, `language`, `timezone`, `spellcheck`, `notifications_enabled`, `notification_sound`, `suppress_notifications_when_active`, `auto_review_approvals`, `computer_execution_mode`, `route_traffic_through_computer`, `hardware_security_keys`, `auto_updates`, `microphone_device`.
   - Validate all enum strings safely in `GmConfig::validate()`.

2. **[`crates/gm-core/src/ipc.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-core/src/ipc.rs):**
   - Add batched **`DaemonRequest::GetBootstrapState`** returning compact initial snapshot in **1 IPC round-trip** (`status`, `bots`, `sections`, `channels`, `recent_tasks`, `routines`, `pending_approvals`, `unread_counts`, `usage_summary`, `connected_app_count`, `active_chat_summaries`).
   - Add targeted on-demand RPCs: `GetUiSettings`, `SaveUiSettings`, `ManageBotSection`, `SetBotUiPrefs`, `SearchMessages`, `MarkChatRead`, `ListLibraryPage { bot_id, filter, cursor, limit }`, `GetDirectChatHistory { bot_id, before_seq, limit }`, `PostDirectChat`, `ToggleMessageReaction`, `SubmitSecretPrompt`, `RespondQuestion`, `ManageCustomRule`, `ManageVault`, `ImportVaultCsv`, `GetUsageBreakdown`, `TriggerRoutineTest`, `DeleteRoutine`, `ListRoutineRuns`, `ListCustomConnectors`, `SaveCustomConnector`, `CaptureComputerFrame`, `StartTeachTask`, `StopTeachTask`, `CheckUpdates`.
   - Add delta **`DaemonEvent`** variants: `BotUpdated`, `BotStatusPhrase`, `DirectMessageAdded`, `MessageCardUpdated`, `MessageReactionUpdated`, `RoutineUpdated`, `RoutineRunRecorded`, `TaskUpdated`, `ApprovalExpired`, `SecretPromptRequested`, `QuestionRequested`, `OrderApprovalRequested`, `BudgetUpdated`, `ComputerFrameReady`, `NotificationAlert`.

3. **[`crates/gm-llm/src/budget.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/budget.rs):**
   - Migrate `usage_v1` schema idempotently to add `bot_id TEXT DEFAULT 'muse'` and `source TEXT DEFAULT 'chat'` columns + index `usage_v1_bot_day(day, bot_id)`.
   - Add `record_spend_attributed(&self, bot_id: &str, source: &str, cost_usd: f64)` + `usage_breakdown(&self, days: u32) -> Result<UsageBreakdown>` returning daily total, weekly total, breakdown `by_bot`, and `by_source` for `Settings -> Usage & Billing` (keeping existing `reserve`/`settle`/`persisted_total` tests 100% green).

4. **[`crates/gm-openbot/src/`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/):**
   - **[`roster.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/roster.rs):** Extend [`BotProfile`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/roster.rs#L14) with `#[serde(default)]` UI fields (`is_primary`, `pinned`, `section_id`, `hidden_from_sidebar`, `avatar_shape`, `avatar_color`, `avatar_asset`, `accent_color`, `notifications_enabled`, `drafts_enabled`, `task_mode_default`). Enforce single `is_primary` owner invariant (`muse` fallback). Add `save_bot_ui_prefs` & custom vector/image avatar path validation (`< 25 MB`, sandboxed under `<bot_root>/avatar/`).
   - **New [`crates/gm-openbot/src/sections.rs`]:** CRUD for custom sidebar sections (`id`, `name`, `sort_order`, `collapsed`) in `openbot.sqlite`.
   - **New [`crates/gm-openbot/src/direct_chat.rs`]:** Durable per-bot 1:1 DM transcript, thread replies (`reply_to_id`), reactions, unread/needs-attention cursors, `card_json` structured card state, and indexed message search (`recent 200` vs `"Search older"`).
   - **New [`crates/gm-openbot/src/vault.rs`]:** Non-secret SQLite metadata table (`vault_items_v1`: `id`, `kind`, `label`, `subtitle_or_last4`, `username`, `domain`, `hint`, `secret_ref`, `is_default_payment`, `updated_at`) paired with [`VaultSecretResolver`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/secrets.rs#L23) / [`KeychainSecrets`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-llm/src/secrets.rs#L86). Includes streaming CSV importer (`max 5 MB`, zero panics, maps 1Password / Chrome / Edge / Safari / Firefox / Bitwarden / LastPass columns, writes secret passwords strictly into OS Keychain, never logs raw secrets).
   - **New [`crates/gm-openbot/src/rules.rs`]:** Custom Auto-Review Rules (`approval_rules_v1`) capping strictly at **20 `Allow` rules** + **20 `Ask first` rules** (`<= 1000` chars). Integrated evaluator honors custom rules only for eligible non-blocklisted operations while preserving immutable `BlocklistManager` and secret protection.
   - **[`routines.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/routines.rs):** Enforce `50 routines per bot` cap, `>= 5 min` minimum interval guard, generate per-routine webhook secret stored in Keychain (`secret://keychain/routine_hook_<id>`), add `delete_routine`, `trigger_routine_test` (bypasses schedule wait for manual `"Test"` button), and emit `RoutineUpdated` / `RoutineRunRecorded`.
   - **New [`crates/gm-openbot/src/connectors.rs`]:** Persistent custom MCP connector definitions (`RemoteHttps`, `StdioCommand`), multi-account link metadata, and connection test helper.
   - **New [`crates/gm-openbot/src/library.rs`]:** Unified `LibraryQuery` aggregating pinned items, Spaces/Pages, `outbox/` files, extracted `sandbox://` media, links, and Bot apps grouped by `Pinned`, `Today`, `This week`, `Older` in `limit=5` cursor pages.

5. **[`crates/gm-daemon/src/control_server.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-daemon/src/control_server.rs) & [`crates/gm-openbot/src/web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs):**
   - Implement every new `DaemonRequest` variant in `control_server.rs` and mirror every endpoint as authenticated `/api/...` routes in [`web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs#L350-L439) (keeping strict `bearer_token` check, CSP `frame-src http://127.0.0.1:<spaces_port>`, `frame-ancestors 'none'`, `DefaultBodyLimit`, and `SecretScrubber` checks).
   - Broadcast all state mutations (bot created/moved, routine saved/fired/tested, task status changed, approval requested/resolved/expired, secret prompt, reaction toggled, budget updated) over both `DaemonEvent` (UNIX/Named Pipe IPC) and `/ag-ui/events` SSE so **no UI ever polls**.
   - Add `POST /api/routines/:id/trigger` authenticated via `x-webhook-key` header comparison in constant time (or bearer token).

---

### Phase 2: Native Desktop (`gm-ui`) Lightweight Refactor & Component Modules

1. **[`crates/gm-ui/Cargo.toml`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/Cargo.toml):**
   - Change default `eframe` features to **`["default_fonts", "glow", "x11", "wayland", "accesskit"]`** and make `wgpu` an optional Cargo feature (`hardware-wgpu = ["eframe/wgpu", "dep:wgpu"]`), cutting binary bloat and avoiding heavy WGPU device initialization unless requested.
   - At startup in [`main.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/main.rs#L507), read `config.ui.hardware_acceleration` + set `eframe::NativeOptions` (`renderer`: `Glow` vs `Wgpu` if compiled, `hardware_acceleration`:Preferred/Off, `vsync: true`). Never allocate `eframe` when launched under `gm-ui tray`.

2. **Split Monolithic `native.rs` (`2458` lines) into Lean, Single-Responsibility Modules under `crates/gm-ui/src/ui/`:**
   - `theme.rs`: Dark/Light + 10-color `AccentPalette` + monochrome `"Black"` tokens + crisp vector Bot shape renderer (`Circle`, `Hex`, `RoundedSquare`, `Diamond`, `Pill`, `Spark`).
   - `state.rs`: Reactive `AppStore` backed by `mpsc` event channel from daemon `SubscribeEvents` + single startup `GetBootstrapState` + dirty-versioned slices. **Zero polling loops**.
   - `cache.rs`:
     - `MemoizedMarkdown`: Caches parsed [`RenderedPart`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/muse_render.rs#L5), GFM blocks, and table grids per message ID.
     - `LruTextureCache`: `16 MB` / `32` texture budget for avatars, image attachments, and temporary live computer frames; auto-evicts oldest textures cleanly.
   - `rail.rs`: `48px` slim left workspace rail (`Bots`, `Feed`, `Goals`, `Spaces`, `Audit`, bottom `Connect Apps`, `Shortcuts`, `Settings`, `Emergency Kill`).
   - `sidebar.rs`: Adaptive Bot/Group/Section/Hidden Bots list, context menu (`Pin`, `Section`, `Mute`, `Duplicate`, `Hide`, `Delete`), `+` create menu, filter input, compact budget & update status strip.
   - `chat.rs`: Virtualized transcript (`ScrollArea::show_rows`), hover reaction/thread/table toolbar, all **14 interactive cards** (Approval, Terminal, Ask-First, Take Control, Secure Masked Secret Form, Question, Draft, Page/Space, Media/Lightbox, Shopping/Order, Phone/Meeting, Subagent/A2A, Connect App, Initiative), and multi-line auto-arrow (`->`→`→`), IME-safe composer with `@` / `/` popups & empty-state `Voice Chat` button.
   - `details_drawer.rs`: `340px` right slide-over drawer (`Overview` + `Teach a task` recorder banner + `Meetings`, `Routines` + webhook copy + `Test` + `20-run history`, `Library` 5-item pagination, `Identity & Memory` CAS + `Provenance/Forget` + `Discretion`).
   - `split_page.rs`: Side-by-side Page viewer / Markdown editor + Static Space status card + `"Message about <Title>"` composer hook + Live Computer Viewer (`CaptureLatestFrame` active **only** while visible!).
   - `workspace_views.rs`: Full Grok-styled views for Rail tabs **`Feed`**, **`Goals`**, **`Spaces`**, and **`Audit`** replacing all legacy floating popups (`show_feed`, `show_goals`, `show_spaces`, `show_identity`).
   - `modals.rs`:
     - `CommandPalette` (`Cmd/Ctrl+K`, 2-stage search + `Search older`)
     - `SettingsModal` (5 panes: `General`, `Computer`, `Vault` + CSV import, `Usage & Billing` + Providers, `Updates` + Feedback)
     - `ConnectAppsModal` (`Apps`, `Custom MCP Connectors`, `Skills Studio`)
     - `ShortcutsModal` (`Cmd/Ctrl+/`) & `AvatarPickerModal` & `MediaLightboxModal`.
   - `hotkeys.rs`: Global shortcut dispatcher + exact 5-tier **Escape Layering Stack**.

3. **Native macOS Tray & HUD Parity ([`mac_ui.m`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/mac_ui.m) & [`main.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/main.rs#L172-L316)):**
   - Update [`handle_daemon_event`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/main.rs#L172) to surface needs-attention badge count on Tray icon, open Quick-Summon directly into the Grok Bot native window (or spotlight prompt bar), and route `SecretPromptRequested` / `WheelHandoffRequested` cleanly to Native approval/handoff sheets.

---

### Phase 3: Web Companion (`companion_ui.html` / `companion_ui.css` / `companion_ui.js`) Full Parity & Zero-Polling Overhaul

Keep [`web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs#L34-L35) serving static inline assets (adding `/companion_ui.css` with `text/css; charset=utf-8` & updated strict `Content-Security-Policy` headers including `img-src 'self' data: blob:; media-src 'self' blob:; frame-src http://127.0.0.1:<spaces_port>`):

1. **Zero-Framework Vanilla Layout (`< 45 KB` total):**
   - Exact CSS Grid mirroring Desktop Chrome: `48px` Workspace Rail + `240px` Adaptive Sidebar (`<=1` collapsed, `2-4` icon mode, `5+` full, drawer on `< 900px`) + Fluid Center (`Chat` + optional `48% Split Page <iframe>` on `spaces_port`) + `340px` Slide-over `Bot Details` Drawer + Modal portal (`Cmd/Ctrl+K`, `Settings`, `Connect Apps`, `Shortcuts`, `Lightbox`, `Expanded Table`).
2. **True Event-Driven Sync (Zero `setInterval` Polling!):**
   - Delete `setInterval(..., 3000)` on [`companion_ui.js:1304`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/companion_ui.js#L1304).
   - Startup fetches **`GET /api/bootstrap`** once, then opens single **`EventSource('/ag-ui/events')`** stream.
   - Each typed SSE event (`BOT_UPDATED`, `MESSAGE_ADDED`, `CARD_UPDATED`, `TASK_UPDATED`, `ROUTINE_RUN`, `ACTIVITY_UPDATED`, `BUDGET_UPDATED`, `SECRET_PROMPT`, etc.) patches only the affected in-memory slice and keyed DOM nodes (using `data-id` diffing and `IntersectionObserver` / fixed-window transcript recycling for 10,000+ messages).
   - If SSE disconnects, reconnect cleanly with jittered exponential backoff (`1s → 2s → 5s → 15s`) and pause live video/computer preview frames immediately when `document.hidden === true`.
3. **Web Security & Fidelity Guarantees:**
   - Strictly `textContent` / safe DOM constructors everywhere (no `innerHTML` for untrusted bot output, markdown code, citations, or channel text — preserving existing XSS defenses).
   - Secure Form & Vault secret fields use `<input type="password" autocomplete="off">`, clear values immediately from memory on `Submit`, and never persist in `localStorage`.
   - Split Static Space `<iframe>` continues enforcing isolated second-port origin (`http://127.0.0.1:<spaces_port>`) + dark/light theme sync via `postMessage` matching [`spaces.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/spaces.rs#L168-L176).

---

## 5. Security, Governance & Invariant Preservation Checklist

| Invariant / Rule | Where Enforced in New UI |
| :--- | :--- |
| **`RULE-SECRET-001` (No Secrets in LLM/SQLite/Disk)** | Secure Form cards submit via `DaemonRequest::SubmitSecretPrompt` directly to `KeychainSecrets`; Vault CSV importer extracts passwords to OS Keychain before storing non-secret label/domain rows; Custom Skills & Runtime file editors check `SecretScrubber` before save. |
| **`RULE-TIER-003` & Publish/Forget Gates** | Custom Allow rules can automate low/medium reversible tool approvals, **never** bypass `BlocklistManager`, `safety.allow_host_desktop`, two-phase `ForgetConfirm`, or `SpacePublish(confirmed=true)` dialogs. |
| **`RULE-PATH-002` (Traversal Protection)** | `sandbox://` attachment previews, uploaded Bot avatars (`< 25 MB`), and `Library` file opens validate strictly through `sandbox_uri_to_bot_relpath()` and `O_NOFOLLOW`. |
| **`RULE-CSP-001` (Two-Port Static Space Isolation)** | Companion `<iframe>` only loads `http://127.0.0.1:<spaces_port>/spaces/v2/<slug>/?token=...`; Companion CSP header restricts `frame-src` to that exact loopback port + `'none'` ancestors. |
| **Multi-Window Data Integrity** | [`SessionStore`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-ui/src/native/session.rs#L32) file lease lock and [`SharedFileStore`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-muse/src/shared_files.rs) CAS SHA-256 checks remain authoritative. |

---

## 6. Verification & Automated Testing Plan

### A. Unit & Backend Invariant Tests (`cargo test`)
1. **`gm-core` Config & IPC:** Round-trip serialization and validation bounds checks for `UiConfig` and all new `DaemonRequest` / `DaemonEvent` variants.
2. **`gm-llm` Budget Attribution:** Test `record_spend_attributed()` daily/weekly aggregation by `bot_id` and `source` across parallel reservations and restarts.
3. **`gm-openbot` Vault, CSV & Custom Rules:**
   - Verify Vault CSV parser on sample 1Password/Chrome/Bitwarden/LastPass exports (`> 5 MB` rejected; secrets saved to test keyring/mock and NEVER present in SQLite dumps).
   - Enforce `20 Allow` + `20 Ask first` rule limits, `<= 1000` char cap, and confirm `BlocklistManager` / irreversible gates cannot be disabled by Allow rules.
   - Enforce Routine `50/bot` cap, `>= 5 min` interval validation, webhook key verification, and 20-run history paging.
   - Verify `DirectChatStore` reaction toggle, threaded reply links, and 2-stage search (`recent` vs `older`).

### B. Headless UI Logic, Render & Efficiency Tests (`crates/gm-ui` & `crates/gm-openbot`)
1. **Zero-Poll & Event-Driven Repaint Assertion (`gm-ui`):**
   - Instantiate headless `egui::Context` + `AppStore` connected to a mock IPC socket; drive 10 idle update ticks with no incoming events and assert **`repaint_requests == 0`** and **`rpc_calls_issued == 1` (`GetBootstrapState` only)**.
2. **Memoization & Virtualization Stress Benchmark (`gm-ui`):**
   - Populate a chat transcript with **10,000 messages** (including tables, `<message_break/>`, `sandbox://` attachments, citations); run `10` consecutive headless `egui` passes and assert:
     - `parse_message_bubbles` invoked **only on initial cache miss**, `0` times on frames 2–10.
     - Frame time & memory stay within budget, `LruTextureCache` stays strictly `<= 16 MB`.
3. **Interactive Card, Shortcut & Escape Stack Tests (`gm-ui`):**
   - Verify Escape closes Popover -> Modal -> Computer Viewer -> Details Drawer in strict layered order.
   - Verify `->` converts to `→` outside code blocks, does NOT convert inside `` `->` ``, and `Enter` is ignored when `ime_composing == true`.
   - Verify `Always allow` appends custom rule; `Always allow` count limit error surfaces inline notice.
4. **Web Companion E2E API, CSP, XSS & No-Poll Verification ([`web.rs`](file:///home/antigravity/galaxycoils/grok-muse/crates/gm-openbot/src/web.rs)):**
   - Assert `GET /` serves strict CSP (`frame-src http://127.0.0.1:<spaces_port>`, `frame-ancestors 'none'`).
   - Assert `companion_ui.js` contains **zero** `setInterval` polling loops (`!js.contains("setInterval")`), handles `/api/bootstrap` + `/ag-ui/events` SSE incremental updates, rejects unauthenticated requests (`401`), and preserves two-phase Forget (`403`) and Space publish confirmation (`403`).
   - If Node.js is available in CI/local shell, run a lightweight DOM/JS syntax & XSS escaping smoke test over `companion_ui.js`.

### C. Full Workspace & Manual Verification Gate
- Run `cargo clippy --workspace --all-targets -- -D warnings`
- Run `cargo test --workspace` (excluding OS-specific hardware GUI tests if headless, running all unit + headless `egui` + `web.rs` integration tests)
- Launch `gm-daemon` + `gm-ui` manually (and inspect companion `http://127.0.0.1:7433` via browser/curl) to verify cold-start RSS (`~35–55 MB`), `0.0%` idle CPU after 10s, Dark/Light/Accent switching, Split Page sandboxing, Live Computer toggle cleanup, and all 5 Rail tabs.
