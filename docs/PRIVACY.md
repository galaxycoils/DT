# Grok Muse Privacy & Security Architecture

Grok Muse is designed as a **local-first, privacy-conscious personal assistant and autonomous coworker**. Your host system, private data, credentials, and conversation transcripts remain under your complete control.

---

## 1. Local-First Data Storage

- **Zero Cloud Telemetry**: Grok Muse does not transmit usage telemetry, crash dumps, or background metrics to any central server.
- **Local SQLite Stores**: Multi-agent channels, task queues, durable routines, and personal memory clusters are stored exclusively in SQLite databases located in your user application directory:
  - macOS: `~/Library/Application Support/GrokMuse/`
  - Linux: `~/.local/share/grok-muse/`
  - Windows: `%APPDATA%\GrokMuse\`
- **Permissions**: Storage directories and configuration files are created with private `0700` / `0600` permissions.

---

## 2. Secret Redaction & Scrubbing

Grok Muse incorporates an automated `SecretScrubber` that operates across all execution pipelines:

1. **Prompt Sanitization**: Sensitive environment variables and vault secrets are replaced with opaque references (`[REDACTED:<ref>]`) before prompts are sent to external LLMs.
2. **Transcript & Log Masking**: Action transcripts, tool outputs, and audit events are scrubbed so that plain credentials never appear in readable logs.
3. **AXSecureTextField Filtering**: Accessibility snapshots automatically detect secure password fields and replace their values with `[REDACTED_SECURE_FIELD]`.
4. **Diagnostic Support Exports**: The `gm diagnostics export` command scrubs all detected keys and passwords, ensuring that technical support bundles can be shared safely.

---

## 3. Native Credential Storage

- Plaintext passwords and API keys are **never stored in cleartext configuration files**.
- API credentials are saved directly into the operating system's native secret manager:
  - **macOS**: Apple Keychain via `security-framework`.
  - **Linux**: Freedesktop Secret Service API via `libsecret`.
  - **Windows**: Windows Credential Manager.

---

## 4. Screenshot and Egress Controls

- **Screen Capture Privacy**: Computer-use screenshots are never uploaded to third-party endpoints unless screenshot egress is explicitly permitted in your configuration:
  ```json
  "allow_screenshot_egress_meta": false,
  "allow_screenshot_egress_xai": false
  ```
- **Local VLM Option**: For maximum privacy, Grok Muse can run offline using local open-weight vision models (e.g. Qwen2-VL, SmolVLM) without sending display frames over the internet.

---

## 5. Team Template & Import Safety

- **Credential Exclusion**: Team templates exported via `gm team export` validate that no API keys, tokens, or personal identifiers are bundled into the template.
- **Trigger Deactivation**: When importing a team template (`gm team import`), all scheduled routines and external triggers are forcefully initialized in a **disabled state**. Users must manually review instructions and enable routines before execution begins.

---

## 6. Muse Two-Phase Forget, Bridge Discretion & Static Spaces Isolation

- **Redacted Two-Phase `Forget` (`crates/gm-memory/src/forget.rs`)**:
  - `gm forget <query>` plans deletions across 9 local storage domains (`PersonalMemoryStore`, `ProvenanceStore`, `USER.md`, `MEMORY.md`, `PROACTIVE_PREFERENCES.md`, `FEEDBACK.md`, `memory/`, `dreams/`, `goals/`, `side-chats/`, `skills/`, `openbot.sqlite`, `audit.jsonl`, and `feed.sqlite`).
  - The staged `.forget-pending.json` file and audit entries store only a 16-character SHA-256 `request_fingerprint`, record IDs, and line SHA-256 digests — never the raw query string.
  - Executing a deletion (`ForgetConfirm`) is gated by `ActionTier::Irreversible` confirmation and explicitly discloses excluded archives (`migration/` receipts and visible channel transcripts).
- **Per-Chat Bridge `DISCRETION.md` (`RULE-PRIVACY-DISCRETION`)**:
  - External Slack, Telegram, and Discord bridge inputs are wrapped in `[BEGIN EXTERNAL CONTENT]` fences so third-party messages are treated strictly as untrusted data.
  - Outbound bridge replies are evaluated against `side-chats/<chat-id>/DISCRETION.md` and `SecretScrubber` to block unrequested replies or private `USER.md` / credential disclosures.
- **Static Spaces Two-Port Origin Isolation**:
  - User/agent-authored Static Spaces are served exclusively from a second loopback port (`127.0.0.1:<spaces_port>`, default `companion_port + 1`) on a separate browser origin from the authenticated Companion API (`127.0.0.1:<companion_port>`). See [`docs/MUSE.md`](MUSE.md) for full details.

