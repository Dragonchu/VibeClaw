#!/usr/bin/env bash
# Reloopy one-click start script
# Usage: DEEPSEEK_API_KEY=your_key ./start.sh
#
# Options:
#   --force    Overwrite existing peripheral workspace without prompting
set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()  { echo -e "${CYAN}[reloopy]${RESET} $*"; }
ok()    { echo -e "${GREEN}[reloopy]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[reloopy]${RESET} $*"; }
die()   { echo -e "${RED}[reloopy] ERROR:${RESET} $*" >&2; exit 1; }

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
RELOOPY_DIR="${HOME}/.reloopy"
DEFAULT_WORKSPACE="${RELOOPY_DIR}/workspace"

# ── sync source to default workspace ─────────────────────────────────────────
BACKUP_DIR="${RELOOPY_DIR}/backups"

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
    local peripheral_dir="${RELOOPY_DIR}/peripheral"
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
            echo -ne "${YELLOW}[reloopy]${RESET} Do you want to overwrite it with the default peripheral? [y/N] "
            read -r answer
            if [[ "${answer}" =~ ^[Yy]$ ]]; then
                backup_and_sync
            else
                info "Keeping existing workspace. Skipping overwrite."
            fi
        fi
    fi
fi

LOG_DIR="${RELOOPY_DIR}/logs"
mkdir -p "${LOG_DIR}"

# ── build ─────────────────────────────────────────────────────────────────────
info "Building workspace (release)…"
cargo build --release 2>&1 | tail -5
ok "Build complete."

# ── stop previous instances ───────────────────────────────────────────────────
RELOOPY_SOCK="${RELOOPY_SOCKET:-${RELOOPY_DIR}/reloopy.sock}"

stop_existing() {
    if [[ -S "$RELOOPY_SOCK" ]]; then
        info "Existing system detected — requesting graceful shutdown via reloopy-admin…"
        if "${SCRIPT_DIR}/target/release/reloopy-admin" --socket "$RELOOPY_SOCK" shutdown 2>/dev/null; then
            ok "Shutdown command accepted. Waiting for socket to disappear…"
            local waited=0
            while [[ -S "$RELOOPY_SOCK" ]] && [[ "$waited" -lt 10 ]]; do
                sleep 1
                waited=$((waited + 1))
            done
            if [[ -S "$RELOOPY_SOCK" ]]; then
                warn "Socket still present after ${waited}s — removing stale socket"
                rm -f "$RELOOPY_SOCK"
            else
                ok "Old system exited cleanly."
            fi
        else
            warn "reloopy-admin shutdown failed (boot may have already exited). Continuing…"
            rm -f "$RELOOPY_SOCK"
        fi
    fi
}
stop_existing

# ── cleanup on exit ───────────────────────────────────────────────────────────
PIDS=()
cleanup() {
    echo ""
    info "Shutting down…"
    # Use reloopy-admin for graceful shutdown (boot broadcasts to all peers)
    "${SCRIPT_DIR}/target/release/reloopy-admin" --socket "$RELOOPY_SOCK" shutdown 2>/dev/null || true
    # Fallback: kill any remaining child processes
    if [[ ${#PIDS[@]} -gt 0 ]]; then
        sleep 2
        for pid in "${PIDS[@]}"; do
            kill "$pid" 2>/dev/null || true
        done
    fi
    ok "All processes stopped."
}
trap cleanup EXIT INT TERM

# ── liveness check helper ─────────────────────────────────────────────────────
check_alive() {
    local name="$1" pid="$2" log="$3"
    if ! kill -0 "$pid" 2>/dev/null; then
        echo ""
        die "${name} exited unexpectedly. Check ${log} for details:\n$(tail -20 "${log}")"
    fi
}

# ── reloopy-boot ────────────────────────────────────────────────────────────────
info "Starting reloopy-boot…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-boot" \
    > "${LOG_DIR}/boot.log" 2>&1 &
PIDS+=($!)
BOOT_PID=$!
sleep 1
check_alive "reloopy-boot" "$BOOT_PID" "${LOG_DIR}/boot.log"
ok "reloopy-boot running  (pid ${BOOT_PID}, log: .reloopy/logs/boot.log)"

# ── reloopy-compiler ────────────────────────────────────────────────────────────
info "Starting reloopy-compiler…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-compiler" \
    > "${LOG_DIR}/compiler.log" 2>&1 &
PIDS+=($!)
COMPILER_PID=$!
sleep 1
check_alive "reloopy-compiler" "$COMPILER_PID" "${LOG_DIR}/compiler.log"
ok "reloopy-compiler running  (pid ${COMPILER_PID}, log: .reloopy/logs/compiler.log)"

# ── reloopy-admin-web ──────────────────────────────────────────────────────────
info "Starting reloopy-admin-web (dashboard)…"
RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-admin-web" \
    > "${LOG_DIR}/admin-web.log" 2>&1 &
PIDS+=($!)
ADMIN_WEB_PID=$!
sleep 1
check_alive "reloopy-admin-web" "$ADMIN_WEB_PID" "${LOG_DIR}/admin-web.log"
ADMIN_WEB_PORT="${RELOOPY_ADMIN_WEB_PORT:-7801}"
ok "reloopy-admin-web running  (pid ${ADMIN_WEB_PID}, log: .reloopy/logs/admin-web.log)"

# ── reloopy-peripheral ─────────────────────────────────────────────────────────
if [[ "${SKIP_PERIPHERAL}" -eq 0 ]]; then
    info "Starting reloopy-peripheral…"
    RUST_LOG="${RUST_LOG:-info}" \
    DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY}" \
        "${SCRIPT_DIR}/target/release/reloopy-peripheral" \
        > "${LOG_DIR}/peripheral.log" 2>&1 &
    PIDS+=($!)
    PERIPHERAL_PID=$!
    sleep 2
    check_alive "reloopy-peripheral" "$PERIPHERAL_PID" "${LOG_DIR}/peripheral.log"
    ok "reloopy-peripheral running  (pid ${PERIPHERAL_PID}, log: .reloopy/logs/peripheral.log)"
    # Extract the actual bound port from the peripheral log (may differ from
    # the configured port when the default was already in use).
    ACTUAL_PORT=$(sed -n 's/.*HTTP server listening on http:\/\/[^:]*:\([0-9]*\).*/\1/p' "${LOG_DIR}/peripheral.log" | tail -1)
    ACTUAL_PORT="${ACTUAL_PORT:-${RELOOPY_HTTP_PORT:-7700}}"
    echo ""
    echo -e "${BOLD}  ➜  Agent UI  http://127.0.0.1:${ACTUAL_PORT}${RESET}"
fi

echo ""
echo -e "${BOLD}  ➜  Dashboard  http://127.0.0.1:${ADMIN_WEB_PORT}${RESET}"
echo ""
info "Press Ctrl-C to stop all services."
wait
