# DT Operator & Support Guide (v1.0.4)

This guide covers installation, permissions, local component downloads, spending controls, backup/restore, device revocation, update recovery, and security reporting for **DT** (`galaxycoils/DT`).

## 1. Support & Security Reporting

- **General Issues & Troubleshooting**: <https://github.com/galaxycoils/DT/issues>
- **Private Security Vulnerability Reporting**: <https://github.com/galaxycoils/DT/security/advisories/new>
- **Diagnostic Bundle Preview & Export**: Run `dt diagnostics export --output dt-support.json` only after reviewing the scrubbed JSON bundle locally. Support exports are generated from an allowlist and verified byte-for-byte against registered secrets and identifying home/storage paths.

## 2. Clean Installation & Upgrades (Including v1.0.0 -> v1.0.1)

### Desktop Targets
- **macOS (Apple Silicon & Intel)**: Open `DT-1.0.1-macos-aarch64.dmg` or `DT-1.0.1-macos-x86_64.dmg` and run `Install DT.pkg` (installs `DT.app` and `DTCUHelper.app`).
- **Windows 11 (x64 & ARM64)**: Run `DT-1.0.1-windows-x86_64-setup.exe` or `DT-1.0.1-windows-aarch64-setup.exe`. User data under `%APPDATA%\DT` / `%APPDATA%\GrokMuse` is preserved across upgrades and uninstalls.
- **Ubuntu 24.04 (amd64 & arm64)**: Install `dt_1.0.1_amd64.deb` or `dt_1.0.1_arm64.deb` via App Center or `sudo apt install ./dt_1.0.1_<arch>.deb`.

### Upgrading from v1.0.0
The public `v1.0.0` release lacked a detached `dt-release.sig` asset and predates Windows ARM64 and Linux ARM64 manifest entries. When upgrading an existing `v1.0.0` installation:
1. Quit the UI and stop background services (`dt service uninstall` or stop LaunchAgent/systemd/Task Scheduler).
2. Run `dt doctor check --json` and ensure no active writers hold `.dt-storage.lease`.
3. Download and verify the signed graphical installer for your OS/architecture (`Install DT.pkg`, NSIS `.exe`, or `.deb`) and verify its SHA-256 against `<asset>.sha256` and `<asset>.sig` using the pinned publisher key.
4. Install the new package and run `dt doctor repair --json`.

## 3. Permissions & Recovery (`dt-doctor` / `dt doctor`)

- Run read-only inspection: `dt-doctor check --json`
- Run serialized safe repairs (capped at 2 attempts per check): `dt-doctor repair --json`
- Resume an interrupted repair or rollback: `dt-doctor resume <RUN_ID> --json`
- List and inspect rollback snapshots:
  ```sh
  dt doctor snapshots --json
  dt doctor inspect-snapshot <SNAPSHOT_ID> --json
  ```
- Restore a snapshot (preserves newer state under `preserved-newer-*`, invalidates restored sessions/pending approvals, pauses external work, and preserves monotonic safety records such as revoked credentials, spent budgets, and forgotten memory):
  ```sh
  dt doctor restore <SNAPSHOT_ID> --json
  ```

## 4. Component Downloads & Spending Limits

- Local VLM, Whisper STT, TTS, and guest VM assets are verified against Ed25519-signed `docs/release/component-catalog.json` chunk hashes and whole-payload SHA-256 digests before atomic activation (`dt models pull <id>`).
- Spending is enforced in integer microdollars (`usage.sqlite3` locally, PostgreSQL when hosted) inside atomic transactions before dispatch. Unsettled reservations after a timeout or crash remain accounted until explicitly reconciled.

## 5. Hosted Server Backup, Restore & Device Revocation

- Hosted deployments run `gm-daemon serve` behind Caddy TLS (`deploy/docker/docker-compose.yml`) with PostgreSQL 16 (`dt_hosted`), row-level security policies (`app.current_ws_id`), and `/api/v1/health/live` + `/api/v1/health/ready` probes.
- **Device/User Revocation**:
  ```sh
  curl -X POST https://<host>/api/v1/auth/revoke \
    -H "Authorization: Bearer $DT_COMPANION_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"ws_id":"<ws_id>","user_id":"<user_id>"}'
  ```
  Revocation immediately invalidates the user's sessions and terminates open workspace SSE streams without affecting other workspaces.
- **Disk-Loss Recovery Drill**:
  1. Restore PostgreSQL dump and encrypted off-host workspace backup bundle onto the replacement host with `DT_HOSTED_MASTER_KEY_FILE` and `pg_password`.
  2. Start `postgres`, `dt-server`, and `tls-proxy` (`docker compose up -d`).
  3. Verify `GET /api/v1/health/ready` returns `200 OK` without invoking a model.
  4. Re-enroll a client (`POST /api/v1/auth/enroll`), verify attachments open and match checksums, verify workspace isolation (`403` cross-tenant), and review any held external jobs before resuming workers.
