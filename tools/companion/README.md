# Companion development

The served HTML/CSS/JS are checked in so Rust builds need no Node toolchain.

```sh
npm ci --prefix tools/companion
npm run build --prefix tools/companion
npm test --prefix tools/companion
node --test tools/tests/companion-events.test.cjs
```

Edit `companion_ui.source.js`, `companion_ui.source.html`, and `companion_layout.css`. Shared colors are in `ui/design-tokens.json`; the build generates both native and browser tokens.
The build extracts static form descriptors and copy into inert JSON in the HTML,
then minifies executable code with the locked Terser version. There is no runtime
framework, CDN, dynamic evaluation, or untrusted HTML construction. The build
rejects combined raw CSS/JS above 64 KiB. HTML remains a separate document.
Always finish generation before starting Cargo: Rust embeds matching assets at
compile time. CI regenerates them and rejects an uncommitted difference.

For real-browser verification, build `gm-daemon`, set `CHROME_BIN` to a local
Chromium/Chrome executable, then run `npm run smoke --prefix tools/companion`.
The runner creates temporary configuration/storage, uses mock inference, verifies
bearer auth, DOM interactions, persisted messages/bots/settings, isolated Spaces
and publish confirmation, and removes its server/storage afterward. Set
`GM_SMOKE_ARTIFACT_DIR` to retain desktop/mobile screenshots.
