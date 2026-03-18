#!/usr/bin/env bash
# VibeClaw one-click start script
# Usage: DEEPSEEK_API_KEY=your_key ./start.sh
#
# Options:
#   --force    Overwrite existing peripheral workspace without prompting
set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()  { echo -e "${CYAN}[vibeclaw]${RESET} $*"; }
ok()    { echo -e "${GREEN}[vibeclaw]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[vibeclaw]${RESET} $*"; }
die()   { echo -e "${RED}[vibeclaw] ERROR:${RESET} $*" >&2; exit 1; }

# ── parse flags ──────────────────────────────────────────────────────────────
FORCE_OVERWRITE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE_OVERWRITE=1 ;;
    esac
done

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
BACKUP_DIR="${LOOPY_DIR}/backups"

sync_workspace() {
    info "Syncing source to default workspace: ${DEFAULT_WORKSPACE}"
    mkdir -p "${DEFAULT_WORKSPACE}"
    rsync -a --exclude=target/ --exclude='.git/' \
        "${SCRIPT_DIR}/" "${DEFAULT_WORKSPACE}/"
    ok "Workspace ready at ${DEFAULT_WORKSPACE}"
}

backup_and_sync() {
    local timestamp
    timestamp="$(date +%Y%m%d_%H%M%S)"
    mkdir -p "${BACKUP_DIR}"

    # Back up the existing workspace.
    local ws_backup="${BACKUP_DIR}/workspace.${timestamp}"
    # Avoid collision if a backup with the same timestamp already exists.
    if [[ -e "${ws_backup}" ]]; then
        local n=1
        while [[ -e "${ws_backup}.${n}" ]]; do ((n++)); done
        ws_backup="${ws_backup}.${n}"
    fi
    info "Backing up existing workspace to ${ws_backup}"
    mv "${DEFAULT_WORKSPACE}" "${ws_backup}"
    ok "Workspace backed up to: ${ws_backup}"

    # Also back up peripheral version history if it exists.
    local peripheral_dir="${LOOPY_DIR}/peripheral"
    if [[ -d "${peripheral_dir}" ]]; then
        local p_backup="${BACKUP_DIR}/peripheral.${timestamp}"
        if [[ -e "${p_backup}" ]]; then
            local n=1
            while [[ -e "${p_backup}.${n}" ]]; do ((n++)); done
            p_backup="${p_backup}.${n}"
        fi
        info "Backing up peripheral version history to ${p_backup}"
        mv "${peripheral_dir}" "${p_backup}"
        ok "Peripheral version history backed up to: ${p_backup}"
    fi

    sync_workspace
    echo ""
    ok "Upgrade complete. Backup location: ${BACKUP_DIR}/"
}

if [[ ! -d "${DEFAULT_WORKSPACE}/crates/peripheral" ]]; then
    sync_workspace
else
    if [[ "${FORCE_OVERWRITE}" -eq 1 ]]; then
        warn "Existing peripheral detected. --force specified, overwriting."
        backup_and_sync
    else
        warn "An existing peripheral workspace was found at:"
        warn "  ${DEFAULT_WORKSPACE}"
        if [[ ! -t 0 ]]; then
            info "Non-interactive mode detected. Use --force to overwrite."
            info "Keeping existing workspace."
        else
            echo ""
            echo -ne "${YELLOW}[vibeclaw]${RESET} Do you want to overwrite it with the default peripheral? [y/N] "
            read -r answer
            if [[ "${answer}" =~ ^[Yy]$ ]]; then
                backup_and_sync
            else
                info "Keeping existing workspace. Skipping overwrite."
            fi
        fi
    fi
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
    if [[ ${#PIDS[@]} -gt 0 ]]; then
        for pid in "${PIDS[@]}"; do
            kill "$pid" 2>/dev/null || true
        done
    fi
    ok "All processes stopped."
}
trap cleanup EXIT INT TERM

# ── loopy-boot ────────────────────────────────────────────────────────────────
info "Starting loopy-boot…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/loopy-boot" \
    > "${LOG_DIR}/boot.log" 2>&1 &
PIDS+=($!)
BOOT_PID=$!
sleep 1
ok "loopy-boot running  (pid ${BOOT_PID}, log: .loopy/logs/boot.log)"

# ── loopy-compiler ────────────────────────────────────────────────────────────
info "Starting loopy-compiler…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/loopy-compiler" \
    > "${LOG_DIR}/compiler.log" 2>&1 &
PIDS+=($!)
COMPILER_PID=$!
sleep 1
ok "loopy-compiler running  (pid ${COMPILER_PID}, log: .loopy/logs/compiler.log)"

# ── loopy-peripheral ─────────────────────────────────────────────────────────
if [[ "${SKIP_PERIPHERAL}" -eq 0 ]]; then
    info "Starting loopy-peripheral…"
    RUST_LOG="${RUST_LOG:-info}" \
    DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY}" \
        "${SCRIPT_DIR}/target/release/loopy-peripheral" \
        > "${LOG_DIR}/peripheral.log" 2>&1 &
    PIDS+=($!)
    PERIPHERAL_PID=$!
    sleep 2
    ok "loopy-peripheral running  (pid ${PERIPHERAL_PID}, log: .loopy/logs/peripheral.log)"
    echo ""
    echo -e "${BOLD}  ➜  Open http://127.0.0.1:${LOOPY_HTTP_PORT:-7700}${RESET}"
fi

echo ""
info "Press Ctrl-C to stop all services."
wait
