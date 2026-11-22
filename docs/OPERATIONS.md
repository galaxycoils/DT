# DT Operator & Support Guide

This guide covers installation, permissions, local component downloads, spending controls, backup/restore, device revocation, update recovery, and security reporting for **DT** (`galaxycoils/DT`).

## 1. Support & Security Reporting

- **General Issues & Troubleshooting**: <https://github.com/galaxycoils/DT/issues>
- **Private Security Vulnerability Reporting**: <https://github.com/galaxycoils/DT/security/advisories/new>
- **Diagnostic Bundle Preview & Export**: Run `dt diagnostics export --output dt-support.json` only after reviewing the scrubbed JSON bundle locally. Support exports are generated from an allowlist and verified byte-for-byte against registered secrets and identifying home/storage paths.

## 2. Installation and upgrades

Use the [release download table](../README.md#downloads) to find the package and checksum for your system. Follow the [local setup instructions](../README.md#run-dt-locally) for platform permissions and web bundle placement.

For manual upgrades, quit the UI and stop the background service or foreground server before installing. Run `dt doctor check --json` after installation, then start the service again. Use a web bundle that matches the installed daemon.

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

## 6. Renewing Signed Metadata

Signed update and component catalog metadata includes a mandatory `expires_at` timestamp. CI enforces a 30-day expiry guard (`test_signed_metadata_not_expiring_soon` in `tools/test_release_tools.py`) that fails before metadata approaches expiration.

- **When to renew**: Run metadata renewal before the 30-day CI threshold (or whenever promoting a newly certified release onto `update-metadata`).
- **Local / CLI renewal (`tools/renew-update-metadata.py`)**:
  ```sh
  python3 tools/renew-update-metadata.py --help
  GM_RELEASE_PUBLIC_KEY_HEX="<32-byte-ed25519-pubkey-hex>" \
  GM_RELEASE_SIGNING_KEY_PEM="$(cat release-signing-key.pem)" \
  python3 tools/renew-update-metadata.py \
    --repo galaxycoils/DT \
    --version 1.0.4 \
    --commit <40-char-commit-sha> \
    --sequence <strictly-increasing-integer> \
    --release-dir <path-to-verified-release-assets> \
    --metadata-dir <path-to-update-metadata-checkout> \
    [--promote-first]
  ```
  `--sequence` **must strictly increase** on every run (anti-rollback protection), and `--promote-first` is passed when promoting a new release version onto `update-metadata` for the first time.
- **GitHub Actions workflow (`.github/workflows/renew-update-metadata.yml`)**:
  Trigger `Renew signed update metadata` (`workflow_dispatch`) with required inputs:
  - `version`: Release version whose immutable assets remain on GitHub Releases (e.g., `1.0.4`).
  - `sequence`: Monotonically increasing metadata sequence number (must be greater than the currently published `sequence`).
  - `commit`: Frozen 40-character lowercase commit SHA matching `refs/tags/v<version>`.
  - `promote_first` (optional boolean): Set to `true` when first promoting a new release version.

