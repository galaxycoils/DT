#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

# Ensure non-interactive bash sessions source ~/.bashrc exports.
touch "$HOME/.bashrc"
sed -i '/case \$- in/,/esac/d' "$HOME/.bashrc" 2>/dev/null || true

append_bashrc() {
  local line="$1"
  grep -qxF "$line" "$HOME/.bashrc" || printf '%s\n' "$line" >> "$HOME/.bashrc"
}

# 1. Configure user-space Rust toolchain (avoids read-only system RUSTUP_HOME).
append_bashrc 'export RUSTUP_HOME="$HOME/.rustup"'
append_bashrc 'export CARGO_HOME="$HOME/.cargo"'
append_bashrc 'export PATH="$HOME/.cargo/bin:$PATH"'
export RUSTUP_HOME="$HOME/.rustup"
export CARGO_HOME="$HOME/.cargo"
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p "$RUSTUP_HOME" "$CARGO_HOME/bin"

rust_channel="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml | head -n 1)"
rust_channel="${rust_channel:-1.99.1}"

if command -v rustup >/dev/null 2>&1; then
  rustup toolchain install "$rust_channel" --profile minimal --component clippy --component rustfmt --no-self-update
  rustup default "$rust_channel"
fi

# 2. Install cargo-nextest in user space if missing.
if ! command -v cargo-nextest >/dev/null 2>&1; then
  if [[ "$(uname -s)" == "Linux" ]]; then
    curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C "$CARGO_HOME/bin"
  fi
fi

# 3. Install missing Linux native dev libraries (PipeWire >= 1.0, SPA, XCB) into user sysroot.
if [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]]; then
  sysroot="$HOME/.local/sysroot"
  append_bashrc 'export PKG_CONFIG_PATH="$HOME/.local/sysroot/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"'
  append_bashrc 'export LIBRARY_PATH="$HOME/.local/sysroot/usr/lib/x86_64-linux-gnu${LIBRARY_PATH:+:$LIBRARY_PATH}"'
  append_bashrc 'export LD_LIBRARY_PATH="$HOME/.local/sysroot/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"'
  export PKG_CONFIG_PATH="$sysroot/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  export LIBRARY_PATH="$sysroot/usr/lib/x86_64-linux-gnu${LIBRARY_PATH:+:$LIBRARY_PATH}"
  export LD_LIBRARY_PATH="$sysroot/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

  if ! (pkg-config --atleast-version=1.0 libpipewire-0.3 && pkg-config --exists libspa-0.2 xcb-shape xcb-xfixes); then
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT
    mkdir -p "$sysroot"

    extract_debs_from_suite() {
      local suite="$1"
      local pkg_regex="$2"
      curl -fsSL "https://deb.debian.org/debian/dists/${suite}/main/binary-amd64/Packages.xz" \
        | xz -dc \
        | awk -v re="^Package: (${pkg_regex})\$" '$0 ~ re {p=1} p && /^Filename: / {print $2} p && /^$/ {p=0}' \
        | while read -r rel; do
            local fname="$tmp_dir/$(basename "$rel")"
            curl -fsSL "https://deb.debian.org/debian/$rel" -o "$fname"
            dpkg-deb -x "$fname" "$sysroot"
          done
    }

    extract_debs_from_suite "bookworm-backports" "libpipewire-0\.3-dev|libpipewire-0\.3-0|libspa-0\.2-dev"
    extract_debs_from_suite "bookworm" "libxcb-shape0-dev|libxcb-xfixes0-dev"

    for pc in "$sysroot"/usr/lib/x86_64-linux-gnu/pkgconfig/*.pc; do
      [[ -f "$pc" ]] && sed -i "s|^prefix=/usr|prefix=$sysroot/usr|" "$pc"
    done

    rm -rf "$tmp_dir"
    trap - EXIT
  fi
fi

# 4. Clone/verify pinned sibling Darius checkout (matching tools/bootstrap.sh).
darius_path="$(dirname "$project_root")/Darius"
darius_revision="$(sed -n 's/^darius_revision=//p' tools/bootstrap.sh | head -n 1)"
if [[ -n "$darius_revision" ]]; then
  if [[ ! -d "$darius_path/.git" ]]; then
    git clone --no-checkout https://github.com/galaxycoils/darius.git "$darius_path"
    git -C "$darius_path" checkout --detach "$darius_revision"
  else
    actual_revision="$(git -C "$darius_path" rev-parse HEAD)"
    if [[ "$actual_revision" != "$darius_revision" ]]; then
      git -C "$darius_path" fetch origin "$darius_revision"
      git -C "$darius_path" checkout --detach "$darius_revision"
    fi
  fi
fi

# 5. Install Node dependencies for companion, web, and mobile clients.
if command -v npm >/dev/null 2>&1; then
  npm ci --prefer-offline --no-audit --no-fund --prefix tools/companion
  npm ci --prefer-offline --no-audit --no-fund --prefix clients/web
  npm ci --prefer-offline --no-audit --no-fund --prefix clients/mobile
fi
