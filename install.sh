#!/bin/sh
# Lyra one-line installer
#
#   curl -fsSL https://raw.githubusercontent.com/iamhsouna/lyra/main/install.sh | sh
#
# Environment:
#   LYRA_REF   git branch/tag to build (default: main)
#   CARGO_HOME cargo home; binaries land in "$CARGO_HOME/bin" (default: ~/.cargo)
set -eu

REPO="https://github.com/iamhsouna/lyra"
REF="${LYRA_REF:-main}"
CRATE="lyra"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- 1. make sure a Rust toolchain is available ----------------------------
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi

if ! command -v cargo >/dev/null 2>&1; then
    command -v curl >/dev/null 2>&1 || err "curl is required to bootstrap Rust"
    say "No Rust toolchain found — installing rustup (minimal profile)…"
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi

command -v cargo >/dev/null 2>&1 || err "cargo is not on PATH; add ~/.cargo/bin and retry"

# --- 2. build and install lyra ---------------------------------------------
say "Installing ${CRATE} from ${REPO} (${REF})…"
cargo install --git "$REPO" --branch "$REF" --locked --force "$CRATE" \
    || err "cargo install failed"

BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
say ""
say "✓ Lyra installed → ${BIN_DIR}/${CRATE}"
case ":$PATH:" in
    *":${BIN_DIR}:"*) ;;
    *) say "  note: ${BIN_DIR} is not on your PATH; add it with:"
       say "        export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac
say ""
say "Next steps:"
say "  1. build the audio.cpp runtime (audiocpp_cli) — see ${REPO}#requirements"
say "  2. download the MiniMax-Music3-GGUF weights"
say "  3. run:  ${CRATE}        # terminal UI"
say "           ${CRATE} web    # browser UI at http://127.0.0.1:8282"
