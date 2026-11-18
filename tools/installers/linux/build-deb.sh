#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: build-deb.sh VERSION BIN_DIR OUTPUT_DEB [ARCH]}"
BIN_DIR="${2:?usage: build-deb.sh VERSION BIN_DIR OUTPUT_DEB [ARCH]}"
OUTPUT_DEB="${3:?usage: build-deb.sh VERSION BIN_DIR OUTPUT_DEB [ARCH]}"
ARCH="${4:-}"

if [[ -z "$ARCH" ]]; then
  case "$OUTPUT_DEB" in
    *_arm64.deb|*aarch64*) ARCH="arm64" ;;
    *) ARCH="amd64" ;;
  esac
fi

case "$ARCH" in
  amd64|x86_64) DEB_ARCH="amd64"; ELF_PATTERN="x86-64" ;;
  arm64|aarch64) DEB_ARCH="arm64"; ELF_PATTERN="ARM aarch64" ;;
  *)
    echo "Unsupported Debian architecture: $ARCH" >&2
    exit 1
    ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

PKG_ROOT="$WORK_DIR/dt_${VERSION}_${DEB_ARCH}"
mkdir -p "$PKG_ROOT/DEBIAN" \
         "$PKG_ROOT/usr/bin" \
         "$PKG_ROOT/usr/share/applications" \
         "$PKG_ROOT/usr/share/doc/dt" \
         "$PKG_ROOT/usr/lib/systemd/user"

for bin in dt gm dt-doctor gm-daemon gm-cu-helper gm-ui; do
  if [[ ! -f "$BIN_DIR/$bin" ]]; then
    echo "Missing required binary: $BIN_DIR/$bin" >&2
    exit 1
  fi
  if command -v file >/dev/null 2>&1; then
    if ! file "$BIN_DIR/$bin" | grep -q "$ELF_PATTERN"; then
      echo "Binary architecture mismatch for $bin: expected $ELF_PATTERN, got $(file "$BIN_DIR/$bin")" >&2
      exit 1
    fi
  fi
  install -m 0755 "$BIN_DIR/$bin" "$PKG_ROOT/usr/bin/$bin"
done

install -m 0644 "$SCRIPT_DIR/dt.desktop" "$PKG_ROOT/usr/share/applications/dt.desktop"
install -m 0644 "$REPO_ROOT/LICENSE" "$PKG_ROOT/usr/share/doc/dt/LICENSE"
install -m 0644 "$REPO_ROOT/THIRD_PARTY_NOTICES.md" "$PKG_ROOT/usr/share/doc/dt/THIRD_PARTY_NOTICES.md"
install -m 0644 "$REPO_ROOT/README.md" "$PKG_ROOT/usr/share/doc/dt/README.md"

cat > "$PKG_ROOT/usr/lib/systemd/user/gm-cu-helper.service" <<'EOF'
[Unit]
Description=DT desktop helper
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/gm-cu-helper
Restart=on-failure
RestartSec=3
UMask=0077

[Install]
WantedBy=graphical-session.target
EOF

cat > "$PKG_ROOT/usr/lib/systemd/user/gm-daemon.service" <<'EOF'
[Unit]
Description=DT daemon
PartOf=graphical-session.target
After=gm-cu-helper.service

[Service]
Type=simple
ExecStart=/usr/bin/gm-daemon daemon
Restart=on-failure
RestartSec=3
UMask=0077

[Install]
WantedBy=graphical-session.target
EOF

cat > "$PKG_ROOT/DEBIAN/control" <<EOF
Package: dt
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${DEB_ARCH}
Maintainer: DT Contributors <https://github.com/galaxycoils/DT/issues>
Depends: libc6 (>= 2.38), libssl3, libsqlite3-0, libxkbcommon0, libxcb-render0, libxcb-shape0, libxcb-xfixes0, libpipewire-0.3-0, libatspi2.0-0, libsecret-1-0, xdg-desktop-portal
Description: DT Hybrid Agent for Ubuntu 24.04 Desktop (X11 and Wayland)
 Fast computer-use, local VLM & voice runtimes, multi-agent coworkers, and
 unified Doctor recovery for Ubuntu 24.04 LTS.
EOF

cat > "$PKG_ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
chmod 0755 "$PKG_ROOT/DEBIAN/postinst"

mkdir -p "$(dirname "$OUTPUT_DEB")"
dpkg-deb --build --root-owner-group "$PKG_ROOT" "$OUTPUT_DEB"
echo "Built Ubuntu package (${DEB_ARCH}): $OUTPUT_DEB"
