#!/usr/bin/env bash
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

find_python_311_plus() {
  for candidate in "${DT_PYTHON:-}" python3.13 python3.12 python3.11 "$HOME/.local/bin/python3.13" /opt/homebrew/bin/python3 python3; do
    [[ -n "$candidate" ]] || continue
    if command -v "$candidate" >/dev/null 2>&1 || [[ -x "$candidate" ]]; then
      if "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null; then
        echo "$candidate"
        return 0
      fi
    fi
  done
  return 1
}

if ! py_bin="$(find_python_311_plus)"; then
  echo "Python 3.11+ is required for DT release tools (set DT_PYTHON or install python3.11+)." >&2
  exit 1
fi
echo "Using Python: $py_bin ($("$py_bin" --version))"
cargo check --workspace --locked
