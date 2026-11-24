# DT v1.0.10

DT is a native desktop agent with web and mobile clients. It combines computer use, a persistent coworker workspace, and configurable model routing. Run it on your own machine or host workspaces behind HTTPS with PostgreSQL.

[Downloads](https://github.com/galaxycoils/DT/releases/tag/v1.0.10) · [Release notes](docs/releases/v1.0.10.md) · [Interactive demo](https://galaxycoils.github.io/DT/) · [Operations guide](docs/OPERATIONS.md)

## What's new

- All six desktop installers (macOS, Windows, and Ubuntu) now include the React web app out of the box alongside Docker and the standalone Linux archive.
- DT Doctor checks desktop session readiness accurately on Linux and Windows, validates snapshot and rollback IDs, and keeps host CLI probes disabled.
- On Apple Silicon, DT uses 13.6 MiB idle and 90.6 MiB with the native desktop UI open. For the fallback-font mapping and single-checkout Darius build updates, see the [v1.0.9 notes](docs/releases/v1.0.9.md).

## Downloads

| Platform | Asset |
| --- | --- |
| Ubuntu 24.04 x86-64 | [`dt_1.0.10_amd64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/dt_1.0.10_amd64.deb) |
| Ubuntu 24.04 ARM64 | [`dt_1.0.10_arm64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/dt_1.0.10_arm64.deb) |
| Linux x86-64 standalone | [`DT-1.0.10-linux-x86_64-binaries.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-linux-x86_64-binaries.tar.gz) |
| macOS Apple Silicon | [`DT-1.0.10-macos-aarch64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-macos-aarch64.dmg) |
| macOS Intel | [`DT-1.0.10-macos-x86_64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-macos-x86_64.dmg) |
| Windows 11 x64 | [`DT-1.0.10-windows-x86_64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-windows-x86_64-setup.exe) |
| Windows 11 ARM64 | [`DT-1.0.10-windows-aarch64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-windows-aarch64-setup.exe) |
| React web client | [`DT-1.0.10-web-client.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.10/DT-1.0.10-web-client.tar.gz) |

Download the package and its matching `.sha256` file into the same directory. Run `sha256sum -c <asset>.sha256` on Linux or `shasum -a 256 -c <asset>.sha256` on macOS. On Windows, compare `Get-FileHash -Algorithm SHA256 <asset>` with the sidecar. The installers also have build metadata and SBOMs.

v1.0.10 is a CI-built prerelease. The macOS apps use ad hoc signatures, Windows installers ship without Authenticode, and updates use a manual download. Current certification requirements and verification coverage are in the [release readiness guide](docs/release-readiness.md).

## Run DT locally

1. **Ubuntu:** Install the matching `.deb`, then run `dt service install`.
2. **macOS:** Open the disk image and run `Install DT.pkg`. Add `/Applications/DT.app/Contents/MacOS` to your shell's `PATH`. Run `dt onboard` and `dt permissions --open-missing`, then run `dt service install`.
3. **Windows:** Run the matching setup executable. Run `dt service install` from the installed `bin` directory and check the background tasks with `dt service status`.

Run `dt doctor check` to find the active storage directory and inspect readiness. To run in the foreground, stop the background daemon first, then run `dt serve`. Both processes use an exclusive storage lease.

Open the React app at [http://127.0.0.1:7433/app/](http://127.0.0.1:7433/app/) and enter the token from the storage directory's `companion-token` file, or use the configured `GROK_MUSE_COMPANION_TOKEN`. The built-in Companion UI is available at [http://127.0.0.1:7433/](http://127.0.0.1:7433/).

### React web app

Every desktop installer, standalone Linux archive, and Docker image includes the matching React build. To host the web client separately or override the bundled copy, verify and extract `DT-1.0.10-web-client.tar.gz`, then place the contents of `dist` in the `web` subdirectory of your DT storage directory (and restart the background daemon) or set `DT_WEB_DIST` before running `dt serve`.

The React client resumes its live stream automatically after transient disconnects. For split-view Pages, open the canonical `127.0.0.1` app URL on the machine running DT.

### Verify a local model

Run `dt models install smolvlm` to download the pinned model, projector, and worker, then run `dt models probe smolvlm image.jpg` to verify inference. Restart the daemon after installation so it can pick up the model in your profile. Voice requires microphone permission and a working local speech runtime. [Local components and speech](docs/OPERATIONS.md#4-component-downloads--spending-limits)

## Self-host with PostgreSQL

Set `DT_DOMAIN` for Caddy and `DT_DATABASE_URL` for the PostgreSQL 16 service, using the Compose hostname `postgres` and database `dt_hosted`. Store the Compose password and hosted master-key files outside the source checkout and pass their absolute paths using `DT_POSTGRES_PASSWORD_FILE` and `DT_HOSTED_MASTER_KEY_FILE`. Set `DT_COMPANION_TOKEN` for operator access.

```sh
cd deploy/docker
docker compose up -d --build
```

Create a workspace with your operator token, then issue a one-time enrollment token for each user or device. Open the HTTPS hostname in React or enter the origin and enrollment token in the mobile app. Use `/api/v1/health/ready` to check readiness and the active backend. See the [operations guide](docs/OPERATIONS.md#5-hosted-server-backup-restore--device-revocation) for first-time enrollment, backups, restore, and device revocation.

## Build from source

Use the Rust toolchain pinned in `rust-toolchain.toml`, Node.js 22, and Python 3.11 or newer. Ubuntu builds of the desktop client require `libasound2-dev`. Cargo fetches the pinned Darius revision from GitHub on the first build.

```sh
git clone --branch v1.0.10 https://github.com/galaxycoils/DT.git DT
cd DT
./tools/bootstrap.sh
cargo build --release --locked --workspace --bins
npm ci --prefix clients/web
npm run build --prefix clients/web
DT_WEB_DIST="$PWD/clients/web/dist" target/release/dt serve
```

For development, `cargo run --locked -p xtask -- dev` starts DT and Vite. The React source is in [`clients/web`](clients/web), the Expo source is in [`clients/mobile`](clients/mobile), and the native desktop and backend are in [`crates`](crates).

## Verify and troubleshoot

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
npm run typecheck --prefix clients/web
npm test --prefix clients/web
python3 tools/test_release_tools.py
```

The [browser smoke runner](tools/companion/README.md) uses isolated storage and mock inference. The [support matrix](docs/SUPPORT_MATRIX.md) and [coverage inventory](docs/release/coverage-inventory.md) track platform and feature verification. For installation and recovery issues, start with [troubleshooting](docs/TROUBLESHOOTING.md).

[Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md) · [Privacy](docs/PRIVACY.md) · [License](LICENSE)
