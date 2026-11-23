# DT release readiness

## v1.0.9 desktop and dependency update

The macOS UI maps the existing system fallback font into memory. The Windows model installer shares the patched ZIP reader used by the model runtime. Cargo fetches the reviewed Darius Git commit for local builds, CI, containers, and dependency scanning. The signing, native device, external account and soak requirements below still apply. [Release notes](releases/v1.0.9.md)

## v1.0.8 reliability prerelease

The GitHub prerelease provides six CI-built installers, a standalone Linux archive, and the React web bundle. Its files carry SHA-256 checksums. macOS uses ad hoc signatures and Windows installers are unsigned. Use the manual upgrade procedure. [Release notes](releases/v1.0.8.md) · [Installation and checksums](../README.md#downloads)

Native installer lifecycle across the target matrix, iOS/Android device flows, external provider and connector accounts, guest boot, and the seven-day soak still require recorded results. The repository lacks a configured publisher signing environment and native certification runners. See the audit and the machine-readable inventory for required evidence. [Audit and acceptance tests](plans/v1.0.6-reliability-audit.md) · [Coverage inventory](release/coverage-inventory.md)

On the Apple Silicon development host, the candidate measured 201.6 MiB with the native UI open across 30 samples. The v1.0.6 package measured 182.6 MiB on the same host. The 120 MiB UI target remains open. Snapshot repair stayed below 24 MiB across the 1 MiB, 64 MiB and 1 GiB attachment fixtures.

## Historical v1.0.1 notes

### Implemented in v1.0.1

- Ordinary OpenBot/usage SQLite opens fail without resetting corrupt data. Shared connection leases prevent explicit recovery while clients remain open. `gm diagnostics recover-database` also requires exclusive runtime ownership, distinguishes corruption from ordinary failures, verifies every copied database/WAL/SHM component and retains originals in a unique private quarantine directory. A later startup creates a new store.
- Queue/channel/routine, usage and pinned memory schemas have transactional version records. Existing stores are backed up with a consistent SQLite snapshot before upgrade. Newer component versions are rejected. Interrupted migrations leave the original schema/version intact. The wrapper establishes the pinned Darius schema transactionally before its idempotent initializer. Persistent-memory failures propagate rather than silently selecting an empty in-memory store. Configuration and roster also carry explicit schema versions, reject newer versions, and preserve verified legacy backups before format upgrades.
- `gm diagnostics prune-backups --keep N` explicitly removes older verified migration snapshots, retaining at least one. It never deletes corruption evidence, task records, replay/deduplication receipts, audit logs or memory. Default retention is preservation. Native conversations retain their existing bounded 500-message/2 MiB policy and clear-history control; memory upkeep remains explicit/configured.
- Host CLI invocation is disabled, including version probes. Discovery does not infer successful login from an environment variable. Screenshot routing to CLI providers is disabled. This is a safety gate, not completion of isolated CLI execution.
- `POST /api/routines/{id}/trigger`, `gm routines trigger <id> --delivery-id <id>` and `DaemonRequest::TriggerRoutine` all call the same admission logic. HTTP requires the existing companion bearer token; IPC requires the existing local peer identity. Payload is `{ "delivery_id": "1-128 visible ASCII characters" }`. Outcomes preserve `status` and nullable `task_id`: queued, duplicate, disabled, overlap_suppressed, skipped_quiet_hours. Unknown routines return HTTP 404; malformed/gate-rejected requests return 400. No schedule advancement occurs. Imported disabled routines remain inert.
- Diagnostics inspect the actual shared OpenBot database and perform bounded matching Ping/Pong probes for daemon and helper. Support exports remove URL credentials/query strings and scrub registered secrets across the entire bundle. The local companion token is included in registered secrets. `redaction_verified` means registered-secret removal only; unknown secrets/personal data still require review. Fingerprints hash compact serialized bundle JSON with `sha256_fingerprint` empty.
- Linux service management installs user systemd units tied to the graphical session. Windows uses per-user logon tasks with `InteractiveToken`, least privilege and bounded restart policy. Install/start failures propagate. Missing status reports are shown explicitly rather than reported as running. Native lifecycle certification remains required.
- Native UI has file import/save dialogs. Binary export reads only capability-scoped inbox/outbox files, at most 8 MiB; the daemon does not accept arbitrary host export destinations. UI saves only after the user selects a destination. Bot duplication copies configuration but no grants, history, files or credentials. Setup and profile fields have explicit accessibility label relationships. IME and screen-reader certification remain required.
- `gm patch <patch> --repo <worktree>` previews a bounded patch and its digest. Repeating with `--approve-sha256 <digest>` applies those exact in-memory bytes after `git apply --check`. Conflicts, path escapes, nested/non-Git roots and changed content fail. No automated agent tool can invoke host patch application. Reviewed VM patch generation/worktree execution is still required before the full coding workflow is certified.
- Updates select canonical exact-target names and independently validate signed version/OS/architecture/name/size/hash. Missing target does not select a package for another platform. Installation remains manual.

## Distribution and signing

`tools/package-release.py --version VERSION --output-dir target/release-candidate` packages six native executables from a clean committed checkout. macOS requires Developer ID Application and Installer identities plus a notarization profile. Windows requires a preconfigured Authenticode signing certificate. Ubuntu uses declared system runtime dependencies.

- `DT-VERSION-macos-aarch64.dmg`
- `DT-VERSION-macos-x86_64.dmg`
- `DT-VERSION-windows-x86_64-setup.exe`
- `dt_VERSION_amd64.deb`

Dispatch the signed release workflow with both run IDs empty to package a candidate on the protected `dt-release` runners. Certify those exact artifacts, then dispatch at the same commit with the packaging and evidence run IDs. Verification uses `GM_EVIDENCE_PUBLIC_KEY_HEX`. Publisher signing uses `GM_RELEASE_SIGNING_KEY_PEM` and `GM_RELEASE_PUBLIC_KEY_HEX`, matching the public key compiled into the release binaries. Signing compares all four installer SHA-256 digests against both the build receipts and verified certification evidence. The workflow uploads a signed candidate for publication.

The updater reads signed metadata from the `update-metadata` branch and remembers the highest verified sequence for each repository. The renewal workflow verifies the frozen tag, release signatures, installer hashes, and previous metadata before publishing a higher sequence.

The manual update procedure is: stop UI/services; close all writers; retain the old package and a complete closed-app data backup; install the verified matching package; restart services/UI; run diagnostics. If recovery is needed, stop all writers again and restore the old package **with its compatible data backup**. Never run an older binary against a newer schema. Clean-machine install/update/recovery/uninstall evidence remains outstanding.

## Guest images and CLI identities

`tools/sign-workspace-image.py` authenticates an already provisioned offline v1 qcow2 image and optional firmware using an independently pinned publisher key. It rejects backing files, encrypted images, symlink inputs and missing ARM firmware. It does not provision an image, certify a boot, implement a CLI-agent network profile, or establish firmware redistribution rights.

Official CLI package inputs are pinned with npm integrity records in `tools/workspace-guest/package-lock.json`. These are provisioning inputs only; no package was installed or executed on the host:

- Claude Code: `@anthropic-ai/claude-code@2.1.318`.
- Codex: `@openai/codex@0.166.0`.
- Grok Build: `@xai-official/grok@1.0.80`, from [xAI's official distribution](https://github.com/xai-org/grok-build/tree/main/crates/codegen/xai-grok-pager/npm/grok). [Headless documentation](https://docs.x.ai/build/cli/headless-scripting) specifies single-prompt mode and update suppression; [enterprise documentation](https://docs.x.ai/build/enterprise) identifies proxy behavior/hosts. This replaces the previously unspecified `grok` package assumption.

**Still required:** provision/test signed x64 and ARM guest images; implement the separate CLI-agent protocol and provider-only egress broker with isolated credentials and process-tree cancellation; integrate/test all three pinned CLI protocols against real accounts. The offline workspace profile must retain no network adapter and no host mounts. A provider proxy or network option must not be enabled merely because a CLI binary is present.

## Native evidence contract

`tools/verify-release-evidence.py --directory evidence --commit SHA --public-key-hex HEX` verifies the exact bytes of `evidence.json` against hex Ed25519 `evidence.sig`. Receipts expire after 30 days and must describe this exact committed source with `source: "native"`. Referenced logs/results are bounded, contained files with SHA-256 checksums. The publisher signature is an authorized reviewer attestation to the underlying real observations; the validator cannot prove that an operator collected those observations honestly.

Top-level fields: `schema: 1`, `commit`, `source`, timezone-bearing `recorded_at`, and `targets`. Every target has `os`, `architecture`, `session`, `checks`, `runs`, `measurements`, `artifacts`.

All five target/session combinations must appear. Each requires three consecutive runs of the 30 reviewed tasks in `docs/release/acceptance-tasks.json`, ten per workflow. Each task records `id`, `workflow`, boolean `success`, boolean `unapproved_irreversible`. Every run needs at least 21 successes and zero unapproved irreversible actions. All named capability/lifecycle checks in the verifier must pass. Measurements require at least 30 samples and the configured idle/UI memory, capture/AX/JPEG/dispatch/kill and step-latency targets. Artifacts are `{ "path": "relative/log", "sha256": "..." }`.

Fixtures, simulated latency and guest-protocol unit tests do not qualify. Authorized native runners or operators must collect these results, sign the reviewed receipt using the independently trusted publisher, and upload a `native-release-evidence` artifact. The candidate workflow accepts that run ID and refuses absent, stale, mismatched, incomplete, unsafe or fixture evidence.

## Remaining release blockers

1. CLI-agent VM profile, egress/credential broker, signed image provisioning and hardware boot certification.
2. End-to-end research/citation verification and isolated Git/worktree/test/patch production; host patch review/apply is implemented, complete agent workflow is not.
3. Versioned migration/retention coverage for audit/replay data; richer binary previews and remaining native integration.
4. Complete runtime dependency distribution, platform signing identities, actual signed packages and clean-machine lifecycle evidence.
5. Native desktop/accessibility/IME/multi-monitor tests, real providers/checkpoints, three-run task acceptance and performance targets on all targets.

Local v0.6.1 baseline validation passed all-feature workspace Clippy and the full locked workspace test suite on macOS, including the native platform test target. The OAuth fixture uses a per-test memory store and requires no desktop credential service. Both release-profile app bundles and the versioned Apple Silicon DMG are checked before preview publication. GitHub CI separately checks macOS, Ubuntu and Windows. These checks cover the development preview. Publisher-signed evidence, clean-machine lifecycle tests, paid model quality and the remaining native release gates above still require certification.

The v0.7.0 preview adds the native/browser layout and provider lifecycle described in [release notes](releases/v0.7.0.md). Account-specific inference remains pending as recorded in [provider validation](provider-validation.md).
