#!/usr/bin/env bash
# Reloopy one-click start script
# Usage: DEEPSEEK_API_KEY=your_key ./start.sh [--force] [--dev]
#
# Options:
#   --force    Overwrite existing peripheral workspace without prompting
#   --dev      Print all service logs to the terminal (coloured, prefixed by service name)
set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
MAGENTA='\033[0;35m'; BLUE='\033[0;34m'

info()  { echo -e "${CYAN}[reloopy]${RESET} $*"; }
ok()    { echo -e "${GREEN}[reloopy]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[reloopy]${RESET} $*"; }
die()   { echo -e "${RED}[reloopy] ERROR:${RESET} $*" >&2; exit 1; }

# ── dev mode helpers ──────────────────────────────────────────────────────────
# prefix_log: reads stdin and prints each line with a fixed-width coloured label.
prefix_log() {
    local label="$1" color="$2"
    while IFS= read -r line; do
        printf "${color}[%-12s]${RESET} %s\n" "$label" "$line"
    done
}

# _launch_bg: start a service in the background.
#
# Sets the global LAST_PID to the PID of the actual service process.
# The pipeline must be started directly in the calling shell (NOT inside $())
# so that the shell's job table tracks it and `wait` at script end blocks
# until all services exit.
#
# Usage: _launch_bg <log-name> <label> <color> <cmd> [args…]
#        PID=$LAST_PID
_launch_bg() {
    local name="$1" label="$2" color="$3"
    shift 3
    local log="${LOG_DIR}/${name}.log"

    if [[ "$DEV_MODE" -eq 1 ]]; then
        # bash -c writes its own PID ($$) before exec-ing the service binary,
        # so the PID survives the exec and can be used for kill/check_alive.
        # > /dev/tty bypasses the parent shell's stdout so log lines appear on
        # the terminal even when the caller has redirected stdout elsewhere.
        local pid_file
        pid_file=$(mktemp)
        bash -c 'echo $$ > "$1"; exec "${@:2}"' _ "$pid_file" "$@" \
            2>&1 | prefix_log "$label" "$color" | tee -a "$log" > /dev/tty &
        local retries=0
        while [[ ! -s "$pid_file" ]] && (( retries < 20 )); do
            sleep 0.05
            (( retries++ )) || true
        done
        LAST_PID=$(cat "$pid_file")
        rm -f "$pid_file"
    else
        "$@" > "$log" 2>&1 &
        LAST_PID=$!
    fi
}

# ── parse flags ──────────────────────────────────────────────────────────────
FORCE_OVERWRITE=0
DEV_MODE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE_OVERWRITE=1 ;;
        --dev)   DEV_MODE=1 ;;
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
if [[ "$DEV_MODE" -eq 1 ]]; then
    cargo build --release
else
    cargo build --release 2>&1 | tail -5
fi
ok "Build complete."

# ── stop previous instances ───────────────────────────────────────────────────
RELOOPY_SOCK="${RELOOPY_SOCKET:-${RELOOPY_DIR}/reloopy.sock}"
RELOOPY_LOCK="${RELOOPY_DIR}/boot.lock"

stop_existing() {
    # Quick check: is there actually a Boot process alive?
    # We probe the flock — if we CAN acquire it, no Boot is running.
    if [[ -f "$RELOOPY_LOCK" ]]; then
        if (flock -n 9) 9<"$RELOOPY_LOCK" 2>/dev/null; then
            # Lock acquired → no Boot process running; just clean up stale socket.
            if [[ -S "$RELOOPY_SOCK" ]]; then
                info "Stale socket detected (no Boot process running) — removing."
                rm -f "$RELOOPY_SOCK"
            fi
            return
        fi
    elif [[ ! -S "$RELOOPY_SOCK" ]]; then
        # No lock file AND no socket → nothing to clean up.
        return
    fi

    # Boot is alive (or at least the socket exists). Try graceful shutdown.
    if [[ -S "$RELOOPY_SOCK" ]]; then
        info "Existing system detected — requesting graceful shutdown via reloopy-admin…"
        if timeout 5 "${SCRIPT_DIR}/target/release/reloopy-admin" --socket "$RELOOPY_SOCK" shutdown 2>/dev/null; then
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
            warn "reloopy-admin shutdown failed or timed out. Cleaning up…"
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
    # Use reloopy-admin for graceful shutdown (with timeout to avoid hanging)
    timeout 5 "${SCRIPT_DIR}/target/release/reloopy-admin" --socket "$RELOOPY_SOCK" shutdown 2>/dev/null || true
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
_launch_bg "boot" "boot" "$CYAN" \
    env RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-boot"
BOOT_PID=$LAST_PID
PIDS+=($BOOT_PID)
sleep 1
check_alive "reloopy-boot" "$BOOT_PID" "${LOG_DIR}/boot.log"
ok "reloopy-boot running  (pid ${BOOT_PID}, log: .reloopy/logs/boot.log)"

# ── reloopy-compiler ────────────────────────────────────────────────────────────
info "Starting reloopy-compiler…"
_launch_bg "compiler" "compiler" "$GREEN" \
    env RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-compiler"
COMPILER_PID=$LAST_PID
PIDS+=($COMPILER_PID)
sleep 1
check_alive "reloopy-compiler" "$COMPILER_PID" "${LOG_DIR}/compiler.log"
ok "reloopy-compiler running  (pid ${COMPILER_PID}, log: .reloopy/logs/compiler.log)"

# ── reloopy-admin-web ──────────────────────────────────────────────────────────
info "Starting reloopy-admin-web (dashboard)…"
_launch_bg "admin-web" "admin-web" "$MAGENTA" \
    env RUST_LOG="${RUST_LOG:-info}" \
    "${SCRIPT_DIR}/target/release/reloopy-admin-web"
ADMIN_WEB_PID=$LAST_PID
PIDS+=($ADMIN_WEB_PID)
sleep 1
check_alive "reloopy-admin-web" "$ADMIN_WEB_PID" "${LOG_DIR}/admin-web.log"
ADMIN_WEB_PORT="${RELOOPY_ADMIN_WEB_PORT:-7801}"
ok "reloopy-admin-web running  (pid ${ADMIN_WEB_PID}, log: .reloopy/logs/admin-web.log)"

# ── reloopy-peripheral ─────────────────────────────────────────────────────────
if [[ "${SKIP_PERIPHERAL}" -eq 0 ]]; then
    info "Starting reloopy-peripheral…"
    _launch_bg "peripheral" "peripheral" "$BLUE" \
        env RUST_LOG="${RUST_LOG:-info}" \
            DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY}" \
        "${SCRIPT_DIR}/target/release/reloopy-peripheral"
    PERIPHERAL_PID=$LAST_PID
    PIDS+=($PERIPHERAL_PID)
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
