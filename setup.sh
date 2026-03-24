#!/usr/bin/env bash
# Reloopy setup script — build and install all binaries.
#
# Usage: ./setup.sh [--prefix DIR]
#
# After installation, start the system with:
#   reloopy start
#
# And stop it with:
#   reloopy stop
set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()  { echo -e "${CYAN}[reloopy]${RESET} $*"; }
ok()    { echo -e "${GREEN}[reloopy]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[reloopy]${RESET} $*"; }
die()   { echo -e "${RED}[reloopy] ERROR:${RESET} $*" >&2; exit 1; }

# ── parse flags ──────────────────────────────────────────────────────────────
INSTALL_DIR=""
for arg in "$@"; do
    case "$arg" in
        --prefix=*) INSTALL_DIR="${arg#--prefix=}" ;;
        --prefix)   shift; INSTALL_DIR="${1:-}" ;;
        --help|-h)
            echo "Usage: ./setup.sh [--prefix DIR]"
            echo ""
            echo "Build and install Reloopy binaries."
            echo ""
            echo "Options:"
            echo "  --prefix DIR   Install binaries to DIR (default: \$CARGO_HOME/bin or ~/.cargo/bin)"
            echo ""
            echo "After installation:"
            echo "  reloopy start   Start the system"
            echo "  reloopy stop    Stop the system"
            exit 0
            ;;
    esac
done

# Determine install directory
if [[ -z "$INSTALL_DIR" ]]; then
    INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
fi

# ── pre-flight ───────────────────────────────────────────────────────────────
command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"
command -v git   >/dev/null 2>&1 || die "git not found. Install it: https://git-scm.com/downloads"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── build ─────────────────────────────────────────────────────────────────────
info "Building all crates (release)…"
cd "$SCRIPT_DIR"
cargo build --release 2>&1 | tail -5
ok "Build complete."

# ── install binaries ──────────────────────────────────────────────────────────
info "Installing binaries to ${INSTALL_DIR}/"
mkdir -p "$INSTALL_DIR"

INSTALLED=0
for bin in "$SCRIPT_DIR/target/release/reloopy"*; do
    # Skip directories, .d files, and non-executables
    [[ -d "$bin" ]] && continue
    [[ "$bin" == *.d ]] && continue
    [[ ! -x "$bin" ]] && continue
    # Skip integration test binary
    [[ "$(basename "$bin")" == "reloopy-integration-tests" ]] && continue

    install -m 755 "$bin" "$INSTALL_DIR/"
    ok "  $(basename "$bin")"
    INSTALLED=$((INSTALLED + 1))
done

if [[ "$INSTALLED" -eq 0 ]]; then
    die "No binaries found to install. Build may have failed."
fi

# ── verify ────────────────────────────────────────────────────────────────────
echo ""
if command -v reloopy >/dev/null 2>&1; then
    ok "Installation complete! ${INSTALLED} binaries installed."
    echo ""
    echo -e "${BOLD}  Usage:${RESET}"
    echo -e "    ${CYAN}reloopy start${RESET}   Start the system (foreground, Ctrl-C to stop)"
    echo -e "    ${CYAN}reloopy stop${RESET}    Gracefully shut down"
    echo ""
else
    warn "Installation complete, but 'reloopy' is not on your PATH."
    warn "Add this to your shell profile:"
    echo ""
    echo -e "    ${BOLD}export PATH=\"${INSTALL_DIR}:\$PATH\"${RESET}"
    echo ""
fi
