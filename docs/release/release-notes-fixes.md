# Historical & Current Release Notes Corrections

This note records verified release-status clarifications for GitHub Releases `v0.3.0`, `v0.4.0`, `v0.5.0`, and `v0.6.0`.

## `v0.6.0` (Full Rakazo Parity & Multi-Provider Suite)
- Includes full workspace parity, multi-provider OAuth/Responses routing, outbound HTTPS relay & mapped bridges, honest capability probing, local voice/SIP/Zoom/Meet adapters, `/api/v1` + hosted tenant isolation + marketplace/Stripe sandbox + React/Expo clients, PR #2 database/patch/updater hardening, and the `v0.5.0` Mac QA & privacy/data-loss fixes.
- Signed release DMGs/installers built in self-hosted CI with Developer ID signing/notarization and offline Ed25519 publisher keys are required for public auto-updater binary verification; local builds on machines without Developer ID certificates are ad-hoc signed.

## `v0.5.0` (Source Release — `GrokBot Workspace Parity`)
- Source-only GitHub Release (`master`). Do **not** upgrade an existing `v0.3.0` binary expecting an automatic migration without closing active writers and following [`docs/release-readiness.md`](../release-readiness.md).

## `v0.4.0` (Source Release — `Meta Muse Agent Integration`)
- Historical source-only release (`master`); no notarized macOS `.dmg` installer was published for `v0.4.0`.

## `v0.3.0` (`main` @ `6f60b76` — Known Issues Warning)
- **Known issues superseded in `v0.5.0` / `v0.6.0`**: Pre-fix `discretion.rs` substring rule ordering, unsafe raw-text redaction in `forget.rs`, unserialized concurrent mutations in `goal_workspace.rs`, and macOS Keychain blocking inside test loops. Upgrade to `v0.6.0+` or build from `master`.
