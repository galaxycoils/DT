# v0.7.0 implementation status

Authorized scope: implement the accepted native/browser Rakazo layout and provider connection plan, then push and publish an ad-hoc Apple Silicon prerelease. Work is on `codex/v0.7.0-rakazo-providers` in the attached checkout. Preserve the separate, dirty original checkout.

The reference is pinned to Rakazo `8aa0f787557697ee7fb5e2f16fb39f4249b35ad4`. Shared semantic colors are generated from `ui/design-tokens.json`. The current implementation uses one remembered sidebar, conversation previews and avatars, a centered header, measured message virtualization, rounded composer, navigation under Activity/Pages, resizable details, and a provider settings page. Native system fonts retain egui fallbacks. Existing explicit theme preferences remain valid.

Provider setup owns the OAuth lifecycle in the daemon. It separates durable credential storage, authentication, catalog refresh, and selected-model reply verification. Revision checks reject stale completion and disconnect races. Provider-only startup and live routing use the current settings. Binary lookup now prefers the running app over an older installation.

Files are indexed by responsibility:

- Config and model metadata: `crates/gm-core/src/config.rs`.
- Provider transport, account identity, renewal: `crates/gm-llm/src/{client,chatgpt_auth,providers_oauth,responses_api}.rs`.
- Daemon-owned connection actions: `crates/gm-openbot/src/provider_connections.rs`.
- Native shell and provider UI: `crates/gm-ui/src/ui/`.
- Browser source: `crates/gm-openbot/src/companion_ui.source.{js,html}` and `companion_layout.css`.
- Shared token generation: `tools/generate-ui-tokens.py`.

The companion remains framework-free and now has a 64 KiB combined raw CSS/JS ceiling, including the expanded provider UI. HTML contains inert static form/copy data. Generation must run before Cargo embeds assets.

Verification commands:

```sh
npm run build --prefix tools/companion
npm test --prefix tools/companion
node --test tools/tests/companion-events.test.cjs
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
```

Local validation logs and isolated native/browser test storage live under `/tmp/gm-v070/`; this path is disposable and is not release evidence. The full workspace run passed 352 tests, including catalog, group preview, contrast, and relocated-bundle coverage. Targeted connection tests were repeated after the final OAuth resource/destination checks. Final checks, visual review, GitHub CI, and tagged packaging must use the final source before publishing.

Account access is absent. [Provider validation](../provider-validation.md) records pending live checks. Full native certification, signed updater delivery, and other existing platform gates remain in [release readiness](../release-readiness.md).
