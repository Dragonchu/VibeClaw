#!/usr/bin/env bash
# VibeClaw one-click start script
# Usage: DEEPSEEK_API_KEY=your_key ./start.sh
set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()  { echo -e "${CYAN}[vibeclaw]${RESET} $*"; }
ok()    { echo -e "${GREEN}[vibeclaw]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[vibeclaw]${RESET} $*"; }
die()   { echo -e "${RED}[vibeclaw] ERROR:${RESET} $*" >&2; exit 1; }

# ── pre-flight ───────────────────────────────────────────────────────────────
command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs"

if ! command -v git >/dev/null 2>&1; then
    warn "git not found. Attempting to install…"
    if command -v apt-get >/dev/null 2>&1; then
        sudo apt-get install -y git || die "Failed to install git. Please install it manually: https://git-scm.com/downloads"
    elif command -v brew >/dev/null 2>&1; then
        brew install git || die "Failed to install git. Please install it manually: https://git-scm.com/downloads"
    else
        die "git is required but not installed. Please install it manually: https://git-scm.com/downloads"
    fi
    ok "git installed: $(git --version)"
fi

if [[ -z "${DEEPSEEK_API_KEY:-}" ]]; then
    warn "DEEPSEEK_API_KEY is not set."
    warn "Boot, compiler, and admin will start, but the peripheral agent will be skipped."
    warn "Re-run with:  DEEPSEEK_API_KEY=your_key ./start.sh"
    SKIP_PERIPHERAL=1
else
    SKIP_PERIPHERAL=0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOOPY_DIR="${HOME}/.loopy"
DEFAULT_WORKSPACE="${LOOPY_DIR}/workspace"

# ── sync source to default workspace ─────────────────────────────────────────
if [[ ! -d "${DEFAULT_WORKSPACE}/crates/peripheral" ]]; then
    info "Syncing source to default workspace: ${DEFAULT_WORKSPACE}"
    mkdir -p "${DEFAULT_WORKSPACE}"
    rsync -a --exclude=target/ --exclude='.git/' \
        "${SCRIPT_DIR}/" "${DEFAULT_WORKSPACE}/"
    ok "Workspace ready at ${DEFAULT_WORKSPACE}"
else
    info "Workspace already exists at ${DEFAULT_WORKSPACE}"
fi

LOG_DIR="${LOOPY_DIR}/logs"
mkdir -p "${LOG_DIR}"

# ── build ─────────────────────────────────────────────────────────────────────
info "Building workspace (release)…"
cargo build --release 2>&1 | tail -5
ok "Build complete."

# ── cleanup on exit ───────────────────────────────────────────────────────────
PIDS=()
cleanup() {
    echo ""
    info "Shutting down…"
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    ok "All processes stopped."
}
trap cleanup EXIT INT TERM

# ── loopy-boot ────────────────────────────────────────────────────────────────
info "Starting loopy-boot…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/loopy-boot" \
    > "${LOG_DIR}/boot.log" 2>&1 &
PIDS+=($!)
sleep 1
ok "loopy-boot running  (pid ${PIDS[-1]}, log: .loopy/logs/boot.log)"

# ── loopy-compiler ────────────────────────────────────────────────────────────
info "Starting loopy-compiler…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/loopy-compiler" \
    > "${LOG_DIR}/compiler.log" 2>&1 &
PIDS+=($!)
sleep 1
ok "loopy-compiler running  (pid ${PIDS[-1]}, log: .loopy/logs/compiler.log)"

# ── loopy-peripheral ─────────────────────────────────────────────────────────
if [[ "${SKIP_PERIPHERAL}" -eq 0 ]]; then
    info "Starting loopy-peripheral…"
    RUST_LOG="${RUST_LOG:-info}" \
    DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY}" \
        "${SCRIPT_DIR}/target/release/loopy-peripheral" \
        > "${LOG_DIR}/peripheral.log" 2>&1 &
    PIDS+=($!)
    sleep 2
    ok "loopy-peripheral running  (pid ${PIDS[-1]}, log: .loopy/logs/peripheral.log)"
    echo ""
    echo -e "${BOLD}  ➜  Open http://127.0.0.1:${LOOPY_HTTP_PORT:-7700}${RESET}"
fi

echo ""
info "Press Ctrl-C to stop all services."
wait
