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
- List rollback snapshot manifests and verify the files in a selected snapshot.
  ```sh
  dt doctor snapshots --json
  dt doctor inspect-snapshot <SNAPSHOT_ID> --json
  ```
- Restore a snapshot (preserves newer state under `preserved-newer-*`, invalidates restored sessions/pending approvals, pauses external work, and preserves monotonic safety records such as revoked credentials, spent budgets, and forgotten memory):
  ```sh
  dt doctor restore <SNAPSHOT_ID> --json
  ```

Ordinary database opens report corruption or an operational failure while preserving the store. After stopping DT, run `dt diagnostics recover-database --db <relative-database-path>` to quarantine a confirmed corrupt database and its SQLite sidecars. If interrupted, use the same command to resume. The next normal open creates an empty database. Keep the `.bak` directory for review or recovery.

## 4. Component Downloads & Spending Limits

- `dt models install smolvlm` or **Settings → Local Components** downloads the model, projector and worker from the bundled catalog with pinned SHA-256 digests. Use `dt models probe smolvlm image.jpg` to run offline inference and record its result for Doctor. Run the install again to reverify an older payload. The separate `dt models pull` command manages Candle safetensors.
- The default catalog is embedded in the release binary. A release-build catalog override requires a valid publisher signature and matching trusted key.
- Native dictation uses the operating system's default microphone after permission and consent. Configure a loopback Whisper service or install `whisper-cli` and the verified Whisper model. Local HTTP TTS must return WAV audio. Linux playback uses PipeWire and macOS can fall back to system speech.
- Spending is enforced in integer microdollars (`usage.sqlite3` locally, PostgreSQL when hosted) inside atomic transactions before dispatch. Unsettled reservations after a timeout or crash remain accounted until explicitly reconciled.

## 5. Hosted Server Backup, Restore & Device Revocation

- Hosted deployments run `gm-daemon serve` behind Caddy TLS (`deploy/docker/docker-compose.yml`) with PostgreSQL 16 (`dt_hosted`), row-level security policies (`app.current_ws_id`), and `/api/v1/health/live` + `/api/v1/health/ready` probes.
Create and enroll a first workspace after `/api/v1/health/ready` reports `postgres`. Use the configured operator token from a trusted terminal.

```sh
curl --fail-with-body "https://$DT_DOMAIN/api/v1/workspaces" \
  -H "Authorization: Bearer $DT_COMPANION_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"ws_id":"team","name":"Team","owner_user_id":"operator"}'

curl --fail-with-body "https://$DT_DOMAIN/api/v1/auth/enroll" \
  -H "Authorization: Bearer $DT_COMPANION_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"ws_id":"team","user_id":"operator","role":"owner"}'
```

Use the returned enrollment token once in React or the mobile app. Mobile stores a device session in SecureStore and requests revocation when disconnecting. Re-enroll after the session expires. Configure an API-key provider within the hosted workspace. Browser-based provider OAuth remains a desktop workflow.

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

## 6. Manual upgrades and signed metadata

Standard CI packages use manual upgrades. Run `dt update --check` to find the official release page. Quit DT, stop its services, back up the profile, verify the package checksum, and then install the matching package. Signed staging requires a build with the publisher key and signed metadata that matches its installer.


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

