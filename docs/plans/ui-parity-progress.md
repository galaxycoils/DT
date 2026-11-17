# GrokBot parity continuation checkpoint

Branch: `feat/grokbot-ui-parity`; continued from `2f5d351`.
The supplied design and original handoff are archived alongside this file as
`grokbot-ui-parity.md` and `astra-continuation.md`. Their absolute source links
refer to the original machine; use repository-relative paths here.

## Implemented in this continuation

- Restore native compilation with a usable routine editor: create/edit, weekdays,
  timezone, time and enable state; wire list/save/delete/test response refreshes.
- Align native bootstrap with backend keys, normalize Space metadata/URLs and
  flat/nested Goal records, use flat unread keys, avatar colors, sidebar visibility
  and all seven Library filters. Hydrate theme/accent/execution settings.
- Buffer bot profile changes until explicit Save/Discard. Include provider/model,
  notification/draft preferences and valid `chat`/`queue` defaults.
- Add bounded navigation history, back/forward shortcuts and header controls,
  sidebar and New Bot shortcuts, persistent IME composition state, plain Enter
  submission, code-aware arrow conversion, and Slack table export.
- Invalidate cached markdown on same-length edits. Apply native style only on
  changes; downscale computer textures through the bounded encoded-image cache.
  Respect the hardware preference in builds that include WGPU; add restart notice.
- Add built-in integration setup/status display, connector transport/auth/argument
  fields matching the backend schema, and custom avatar image upload.
- Add explicit native render paths for terminal/ask-first/order approvals, drafts,
  bridge setup and initiative suggestions. Some interactions still need the
  complete parity work below; these are not a claim of full card parity.
- Clear sensitive modal buffers on dismissal and deliver Vault reveal responses
  into the Settings view. Never persist companion authentication tokens.
- Replace the companion's timer polling with one bearer-authenticated fetch SSE
  connection, incremental frame parsing, cancellation and jittered reconnects.
  Startup and event-driven reconciliation use bootstrap snapshots; this does not
  yet implement the spec's per-slice delta DOM updates.
- Restrict the Space iframe to `sandbox="allow-scripts"`, validate its tokenized
  URL against the bootstrap Space port/path, and use opaque-origin theme messages.
- Enforce the CSS+JS byte budget and test SSE auth/delivery, asset MIME/CSP,
  code-span arrows, secret dismissal, navigation and cache invalidation.
- Format the previously unformatted scaffold; factor complex types for Clippy.

## Verification

Using Rust stable 1.100.0, with Darius pinned to the CI commit
`f47a4ce980abecb751c9f5c79e1c424a20100433`:

- `cargo test -p gm-ui -p gm-core -p gm-llm -p gm-openbot`: 117 tests passed.
- `cargo clippy -p gm-ui -p gm-openbot --all-targets -- -D warnings`.
- `cargo check -p gm-ui --features hardware-wgpu`.
- `cargo fmt --all -- --check`.
- `node --check crates/gm-openbot/src/companion_ui.js`.
- `node --test tools/tests/companion-events.test.cjs`: two parser tests passed.
- CSS+JS: 44,948 raw bytes, below the strict 46,080-byte limit.

The render test now specifies a real 1220×800 viewport and settles startup
animations before asserting idle behavior. It retains its existing 5 ms debug
threshold. This is not a measured release-mode <1 ms or process RSS/CPU result.

## Still required before v0.5.0

1. Full companion layout/composer/card/details/modal rewrite. It currently retains
   legacy controls and uses snapshot reconciliation on SSE, not keyed deltas.
   Replace legacy inline CSS with the complete shared theme tokens and responsive
   48/60/240/340 px chrome, and add native/mobile browser smoke coverage.
2. Complete native Routines drawer/history/webhook controls, channel editor,
   sidebar status/budget/attention/overlay behavior and full shortcut map. Current
   digit shortcuts still switch bots; the full design requests workspace jumps.
3. Finish card-specific operations, approval feedback after successful rule save,
   mention/skill catalogs, attachment picker, Lightbox media loading/controls,
   complete Settings/Updates/Usage/Vault panes and custom connector editing.
4. Finish Memory explain/discretion/forget verification UI, Static Space read/edit
   and publish flow, Feed/Goals/Audit controls and all missing response/event wiring.
5. Audit push coverage for every backend mutation, measure idle RSS/CPU and release
   frame time, run full cross-platform workspace CI and isolated daemon smoke test.
6. Only after parity and all gates pass: bump workspace/lockfile to 0.5.0, merge,
   tag and publish the release. No version bump, master merge or release was made
   at this checkpoint.
