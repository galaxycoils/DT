# GrokBot parity implementation

The implementation is on `feat/grokbot-ui-parity`, PR #5, targeting v0.5.0.
The original design and handoff are archived in `grokbot-ui-parity.md` and
`astra-continuation.md`; their absolute links refer to the original machine.

The legacy companion has been replaced by the responsive rail/sidebar/chat,
Details, Split Space and modal architecture. Native UI data contracts, buffered
editing, routines/history/webhooks, group metadata, memory workflows, settings,
attachments, navigation, avatar/media loading and workspace controls are wired to
local backend operations. Bridge-dependent actions expose setup states.

The companion build stays below the strict 45 KiB raw CSS+JS budget. It uses inert
HTML metadata for static forms/copy and a generated, framework-free script.
`tools/companion/README.md` documents generation and real-browser verification.

Validation includes full workspace Clippy/tests, WGPU compilation, formatting,
DOM/SSE tests and an isolated daemon browser smoke. CI runs the platform-specific
Linux capture and macOS packaging checks. See PR #5 checks for the final commit.

The release does not claim that headless tests certify process RSS, idle CPU,
macOS TCC permissions, or real cloud adapters. Those require the configured
native-certification runner and connected adapters described in the design.
