# DT Troubleshooting Guide

This guide covers common issues, diagnostic procedures, and recovery paths for **DT** (`dt` / `dt-doctor`; legacy alias `gm`).

---

## 1. Running System Diagnostics

When encountering issues, first run DT Doctor and local diagnostics:

```sh
# Read-only readiness check (does not mutate storage)
dt doctor check --json

# Safe repairs (up to 2 attempts per check)
dt doctor repair --json

# Resume an interrupted repair or rollback run
dt doctor resume <RUN_ID> --json
```

If reporting an issue to support, generate and review a scrubbed diagnostic bundle before sharing:

```sh
dt diagnostics export --output dt-support.json
```

The export is built from an allowlist and verified against registered secrets via `SecretScrubber`.

---

## 2. IPC Sockets and Daemon Connectivity

### Issue: "path must be shorter than SUN_LEN" on macOS
- **Cause**: On macOS Darwin, Unix domain sockets (`sockaddr_un`) have a strict 104-byte length limit (`SUN_LEN`).
- **Fix**: Sockets are bound in `/tmp/` with short truncated hashes (e.g. `/tmp/gm-ses-*.sock`). Ensure `/tmp` is accessible and writable.

### Issue: Daemon not responding or socket disconnected
- Verify the background service is running:
  ```sh
  dt status
  dt service status
  ```
- Reinstall the user service (macOS LaunchAgent, Linux graphical user systemd unit, or Windows logon task):
  ```sh
  dt service install
  ```
- Remember that `dt service` and foreground `dt serve` share an exclusive storage lease; stop the background service before running `dt serve`.

---

## 3. Database Recovery and Snapshot Rollback

### Issue: SQLite database error or confirmed corruption
- **Fail-closed opens**: DT never silently resets or overwrites a corrupt SQLite database on ordinary opens. Operational errors (busy/locked/permissions) are reported without moving files.
- **Explicit quarantine recovery**: Stop DT UI and background services, then quarantine the confirmed corrupt store (preserving the database and its WAL/SHM sidecars in a private `.bak` directory):
  ```sh
  dt diagnostics recover-database --db <relative-database-path>
  ```
  The next startup initializes a fresh store while keeping the `.bak` directory for inspection.
- **Snapshot inspection & rollback**:
  ```sh
  dt doctor snapshots --json
  dt doctor inspect-snapshot <SNAPSHOT_ID> --json
  dt doctor restore <SNAPSHOT_ID> --json
  ```

---

## 4. macOS Privacy & Security Permissions

### Issue: "Screen Recording not granted" or "Accessibility permission missing"
- DT requires macOS TCC permissions for computer-use automation:
  - **Screen Recording**: Capturing display frames for visual grounding.
  - **Accessibility**: Inspecting UI elements via `AXUIElement`.
- **Remediation**:
  ```sh
  dt permissions --open-missing
  dt permissions --open screen
  dt permissions --open accessibility
  ```

---

## 5. Model Providers & Local Components

### Issue: Host CLI providers (`claude` / `codex` / `grok`) are disabled
- Host CLI execution, `--version` probes, and environment-variable login inference are intentionally disabled until isolated VM CLI-agent execution is certified.
- Configure an API key or OAuth provider profile in **Settings → Providers** (ChatGPT OAuth, OpenAI API, Meta, xAI, Kilo, OpenRouter, Nous Portal, or Custom/Local).

### Issue: Local VLM or Speech Runtime not ready
- Install pinned local components with SHA-256 verification and probe offline inference:
  ```sh
  dt models install smolvlm
  dt models probe smolvlm image.jpg
  ```
  Restart the daemon after installation so it picks up the newly verified model.

---

## 6. Workspace VM Isolation

### Issue: VM fails to launch
- **Linux**: Verify `/dev/kvm` exists and your user belongs to the `kvm` group (`ls -la /dev/kvm`, `sudo usermod -aG kvm $USER`).
- **macOS**: Ensure Hypervisor.framework (`kern.hv_support=1`) is available.
- **Windows**: Ensure Windows Hypervisor Platform (WHPX) is enabled under Windows Features.
