# DT v1.0.5

DT is a native desktop agent with web and mobile clients. It combines computer use, a persistent coworker workspace, and configurable model routing. Run it on your own machine or host workspaces behind HTTPS with PostgreSQL.

[Downloads](https://github.com/galaxycoils/DT/releases/tag/v1.0.5) · [Release notes](docs/releases/v1.0.5.md) · [Interactive demo](https://galaxycoils.github.io/DT/) · [Operations guide](docs/OPERATIONS.md)

## What's new

- The React client at `/app/` accepts the local Companion token on loopback. It keeps the token in memory and resumes the live stream after transient disconnects, including mid-reply.
- Page previews run on a separate loopback port in a scripts-only sandbox. The client explains when you need the daemon's canonical URL or a newer daemon.
- Hosted workspaces use enrollment, secure cookies, and CSRF protection. Caddy redirects the HTTPS root to `/app/` and forwards the app and v1 API to DT.
- Doctor and the setup guide explain how to supply the web bundle and choose between background and foreground operation.

## Downloads

| Platform | Asset |
| --- | --- |
| Ubuntu 24.04 x86-64 | [`dt_1.0.5_amd64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/dt_1.0.5_amd64.deb) |
| Ubuntu 24.04 ARM64 | [`dt_1.0.5_arm64.deb`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/dt_1.0.5_arm64.deb) |
| Linux x86-64 standalone | [`DT-1.0.5-linux-x86_64-binaries.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-linux-x86_64-binaries.tar.gz) |
| macOS Apple Silicon | [`DT-1.0.5-macos-aarch64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-macos-aarch64.dmg) |
| macOS Intel | [`DT-1.0.5-macos-x86_64.dmg`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-macos-x86_64.dmg) |
| Windows 11 x64 | [`DT-1.0.5-windows-x86_64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-windows-x86_64-setup.exe) |
| Windows 11 ARM64 | [`DT-1.0.5-windows-aarch64-setup.exe`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-windows-aarch64-setup.exe) |
| React web client | [`DT-1.0.5-web-client.tar.gz`](https://github.com/galaxycoils/DT/releases/download/v1.0.5/DT-1.0.5-web-client.tar.gz) |

Download the package and its matching `.sha256` file into the same directory. Run `sha256sum -c <asset>.sha256` on Linux or `shasum -a 256 -c <asset>.sha256` on macOS. On Windows, compare `Get-FileHash -Algorithm SHA256 <asset>` with the sidecar. The installers also have build metadata and SBOMs.

These are CI-built packages. The macOS apps use ad hoc signatures and the Windows installers ship without Authenticode. See [release readiness](docs/release-readiness.md) for native certification and signed update requirements.

## Run DT locally

1. **Ubuntu:** Install the matching `.deb`, then run `dt service install`.
2. **macOS:** Open the disk image and run `Install DT.pkg`. Add `/Applications/DT.app/Contents/MacOS` to your shell's `PATH`. Run `dt onboard` and `dt permissions --open-missing`, then run `dt service install`.
3. **Windows:** Run the matching setup executable. Run `dt service install` from the installed `bin` directory and check the background tasks with `dt service status`.

Run `dt doctor check` to find the active storage directory and inspect readiness. To run in the foreground, stop the background daemon first, then run `dt serve`. Both processes use an exclusive storage lease.

Open the React app at [http://127.0.0.1:7433/app/](http://127.0.0.1:7433/app/) and enter the token from the storage directory's `companion-token` file, or use the configured `GROK_MUSE_COMPANION_TOKEN`. The built-in Companion UI is available at [http://127.0.0.1:7433/](http://127.0.0.1:7433/).

### Install or replace the React bundle

The Ubuntu packages, standalone Linux archive, and Docker image include the React build. For macOS and Windows, verify and extract the matching web client archive. Copy the contents of `dist` into the `web` subdirectory of your DT storage directory, then restart the background daemon. For foreground use, set `DT_WEB_DIST` to the extracted `dist` directory before starting `dt serve`.

Use the v1.0.5 daemon and web bundle together. For split-view Pages, open the canonical `127.0.0.1` app URL on the machine running DT.

## Self-host with PostgreSQL

Set `DT_DOMAIN` for Caddy and `DT_DATABASE_URL` for the PostgreSQL 16 service, using the Compose hostname `postgres` and database `dt_hosted`. Provide the Compose password and hosted master-key files using `DT_POSTGRES_PASSWORD_FILE` and `DT_HOSTED_MASTER_KEY_FILE`. Set `DT_COMPANION_TOKEN` for operator access.

```sh
cd deploy/docker
docker compose up -d --build
```

Open the deployment's HTTPS hostname and connect with a workspace enrollment token. Use `/api/v1/health/ready` to check readiness and the active backend. See the [operations guide](docs/OPERATIONS.md) for backups, restore, and device revocation.

## Build from source

Use the Rust toolchain pinned in `rust-toolchain.toml` and Node.js 22. DT's Rust workspace uses the sibling Darius checkout at the pinned commit below.

```sh
git clone https://github.com/galaxycoils/darius.git Darius
git -C Darius checkout f47a4ce980abecb751c9f5c79e1c424a20100433
git clone --branch v1.0.5 https://github.com/galaxycoils/DT.git DT
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
