#!/usr/bin/env bash
# bench/run.sh — WASI-USB thesis benchmark harness
#
# Runs all combinations of (workload × condition) and writes per-run CSV files
# into results/<timestamp>/.
#
# Usage:
#   sudo bench/run.sh [OPTIONS]
#
# Options:
#   --smoke           1 iteration per cell (quick sanity check)
#   --iter N          iterations per cell (overrides per-workload defaults)
#   --warmup N        warm-up iterations before timed run (default: 0)
#   --workloads LIST  comma-separated subset, e.g. ctrl,int  (default: all)
#   --conditions LIST comma-separated subset, e.g. C1,C3     (default: all)
#   --out-dir DIR     override results directory
#   --no-sudo         do not prepend sudo
#   --dry-run         print commands without executing
#
# Conditions:
#   C1  native-libusb  (C, native ELF)
#   C2  wasi-libusb    (C, WASM component via usb-wasi-host)
#   C3  native-rusb    (Rust, native ELF)
#   C4  wasi-rusb      (Rust + rusb → libusb-wasi.a → WIT, WASM component)
#   C5  wasi-raw-wit   (Rust, raw WIT, WASM component via usb-wasi-host)
#
# Workloads:
#   bulk  SCSI READ(10) on USB mass-storage  (SanDisk 3.2Gen1     0781:5581)
#   ctrl  Control transfers on loopback dev  (WASI-USB Loopback   cafe:4002)
#   int   Interrupt poll on PS5 DualSense    (PS5 DualSense       054c:0ce6)
#   iso   Isochronous UVC on Logitech Brio   (Logitech Brio 100   046d:094c)
#
# Compatible with bash 3.2+ (macOS default shell).

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

C_NATIVE_BIN="${REPO_ROOT}/usb-bench-c/build-native"
C_WASI_BIN="${REPO_ROOT}/usb-bench-c/build-wasi"
RS_NATIVE_BIN="${REPO_ROOT}/usb-bench-rs/target/release"
RS_WASI_BIN="${REPO_ROOT}/usb-bench-rs/target/wasm32-wasip2/release"
RS_WASI_RUSB_BIN="${REPO_ROOT}/usb-bench-rs/target-wasi-rusb/wasm32-wasip2/release"
HOST="${REPO_ROOT}/usb-wasi-host/target/release/usb-wasi-host"

# ── Defaults ──────────────────────────────────────────────────────────────────
SMOKE=0
WARMUP=0
WORKLOADS="bulk,ctrl,int,iso"
CONDITIONS="C1,C2,C3,C4,C5"
OUT_DIR=""
USE_SUDO=1
DRY_RUN=0
GLOBAL_ITER=""

# ── Argument parsing ──────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --smoke)      SMOKE=1;              shift ;;
        --iter)       GLOBAL_ITER="$2";     shift 2 ;;
        --warmup)     WARMUP="$2";          shift 2 ;;
        --workloads)  WORKLOADS="$2";       shift 2 ;;
        --conditions) CONDITIONS="$2";      shift 2 ;;
        --out-dir)    OUT_DIR="$2";         shift 2 ;;
        --no-sudo)    USE_SUDO=0;           shift ;;
        --dry-run)    DRY_RUN=1;            shift ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ── Per-workload lookup helpers (bash 3.2 compatible) ─────────────────────────

# Default iterations
default_iter() {  # $1 = workload
    case "$1" in
        bulk) echo 100  ;;
        ctrl) echo 1000 ;;
        int)  echo 1000 ;;
        iso)  echo 200  ;;
        *)    echo 100  ;;
    esac
}

# VID:PID
# bulk: SanDisk 3.2Gen1  (was Kingston DataTraveler 0951:1666)
# ctrl: WASI-USB Loopback (was Arduino Nano 2341:8057)
# int:  PS5 DualSense     (may not be connected — cell will warn+skip)
# iso:  Logitech Brio 100 (was Brio 4K 046d:086c)
vidpid_for() {  # $1 = workload
    case "$1" in
        bulk) echo "0781:5581" ;;
        ctrl) echo "cafe:4002" ;;
        int)  echo "054c:0ce6" ;;
        iso)  echo "046d:094c" ;;
        *)    echo "0000:0000" ;;
    esac
}

# Resolved iteration count for a workload
iter_for() {  # $1 = workload
    if [[ $SMOKE -eq 1 ]]; then
        echo 1
    elif [[ -n "$GLOBAL_ITER" ]]; then
        echo "$GLOBAL_ITER"
    else
        default_iter "$1"
    fi
}

# ── Output directory ──────────────────────────────────────────────────────────
TS="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR="${REPO_ROOT}/results/${TS}"
fi
mkdir -p "${OUT_DIR}"

# ── sudo prefix ───────────────────────────────────────────────────────────────
SUDO=""
if [[ $USE_SUDO -eq 1 ]]; then
    SUDO="sudo"
fi

# ── Platform-specific performance tuning ─────────────────────────────────────
platform_tune() {
    case "$(uname -s)" in
        Linux)
            if command -v cpupower &>/dev/null; then
                ${SUDO} cpupower frequency-set -g performance 2>/dev/null || true
                echo "  [tune] CPU governor → performance"
            fi
            ;;
        Darwin)
            echo "  [tune] macOS: no CPU governor; ensure no background load"
            ;;
    esac
}

# ── RT scheduling wrapper (Linux only) ───────────────────────────────────────
# Echoes a prefix command or nothing
rt_prefix() {
    case "$(uname -s)" in
        Linux)
            if command -v chrt &>/dev/null; then
                echo "${SUDO} chrt -f 50"
            fi
            ;;
        *) echo "" ;;
    esac
}

# ── run_cmd: print + optionally execute ───────────────────────────────────────
# Returns the exit code of the command; never aborts the harness.
run_cmd() {
    echo "    \$ $*"
    if [[ $DRY_RUN -eq 0 ]]; then
        eval "$@" || {
            local rc=$?
            echo "  [WARN] command exited with status ${rc} — device may not be connected"
            return ${rc}
        }
    fi
}

# ── warn_skip ─────────────────────────────────────────────────────────────────
warn_skip() {
    echo "  [SKIP] $1 — not found: $2"
}

# ── collect system metadata ───────────────────────────────────────────────────
collect_meta() {
    local meta_file="${OUT_DIR}/meta.txt"
    {
        echo "run_timestamp=${TS}"
        echo "host_os=$(uname -sr)"
        echo "host_arch=$(uname -m)"
        echo "smoke=${SMOKE}"
        echo "warmup=${WARMUP}"
        echo "workloads=${WORKLOADS}"
        echo "conditions=${CONDITIONS}"
        if [[ -x "${HOST}" ]]; then
            echo "usb_wasi_host_path=${HOST}"
        fi
        echo "rust=$(rustc --version 2>/dev/null || echo unknown)"
        echo "cargo=$(cargo --version 2>/dev/null || echo unknown)"
    } > "${meta_file}"
    echo "  [meta] → ${meta_file}"
}

# ── run_cell <cond> <workload> <output_csv> <iters> ──────────────────────────
run_cell() {
    local cond="$1"
    local wl="$2"
    local out_csv="$3"
    local iters="$4"
    local vidpid
    vidpid="$(vidpid_for "${wl}")"
    local rt
    rt="$(rt_prefix)"

    # WASI components run with cwd=REPO_ROOT preopened as ".".
    # Absolute paths are not accessible inside the sandbox, so pass a
    # path relative to REPO_ROOT for all WASI conditions (C2/C4/C5).
    local rel_csv="${out_csv#${REPO_ROOT}/}"

    case "${cond}" in
        # ── C1: native libusb (C ELF) ─────────────────────────────────────────
        C1)
            local bin="${C_NATIVE_BIN}/w_${wl}"
            if [[ ! -f "${bin}" ]]; then warn_skip "${cond}:${wl}" "${bin}"; return; fi
            run_cmd ${SUDO} ${rt} "\"${bin}\"" \
                "\"${out_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
                --condition native-libusb
            ;;

        # ── C2: wasi-libusb (C WASM) ──────────────────────────────────────────
        C2)
            local wasm="${C_WASI_BIN}/w_${wl}.wasm"
            if [[ ! -f "${wasm}" ]];  then warn_skip "${cond}:${wl}" "${wasm}"; return; fi
            if [[ ! -x "${HOST}" ]];  then warn_skip "${cond}:${wl}" "${HOST}"; return; fi
            run_cmd cd "\"${REPO_ROOT}\"" "&&" \
                ${SUDO} ${rt} "\"${HOST}\"" -c "\"${wasm}\"" -- \
                "\"${rel_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
                --condition wasi-libusb
            ;;

        # ── C3: native rusb (Rust ELF) ────────────────────────────────────────
        C3)
            local bin="${RS_NATIVE_BIN}/w_${wl}"
            if [[ ! -f "${bin}" ]]; then warn_skip "${cond}:${wl}" "${bin}"; return; fi
            run_cmd ${SUDO} ${rt} "\"${bin}\"" \
                "\"${out_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
                --condition native-rusb
            ;;

        # ── C4: wasi-rusb (Rust + rusb → libusb-wasi.a → WIT) ───────────────────
        C4)
            local wasm="${RS_WASI_RUSB_BIN}/w_${wl}.wasm"
            if [[ ! -f "${wasm}" ]];  then warn_skip "${cond}:${wl}" "${wasm}"; return; fi
            if [[ ! -x "${HOST}" ]];  then warn_skip "${cond}:${wl}" "${HOST}"; return; fi
            run_cmd cd "\"${REPO_ROOT}\"" "&&" \
                ${SUDO} ${rt} "\"${HOST}\"" -c "\"${wasm}\"" -- \
                "\"${rel_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
                --condition wasi-rusb
            ;;

        # ── C5: wasi-raw-wit (Rust raw WIT WASM) ─────────────────────────────
        C5)
            local wasm="${RS_WASI_BIN}/w_${wl}.wasm"
            if [[ ! -f "${wasm}" ]];  then warn_skip "${cond}:${wl}" "${wasm}"; return; fi
            if [[ ! -x "${HOST}" ]];  then warn_skip "${cond}:${wl}" "${HOST}"; return; fi
            run_cmd cd "\"${REPO_ROOT}\"" "&&" \
                ${SUDO} ${rt} "\"${HOST}\"" -c "\"${wasm}\"" -- \
                "\"${rel_csv}\"" "\"${vidpid}\"" "\"${iters}\"" \
                --condition wasi-raw-wit
            ;;

        *)
            echo "  [SKIP] Unknown condition: ${cond}"
            ;;
    esac
}

# ── warm_cell: run WARMUP iterations, output to /dev/null ────────────────────
warm_cell() {
    local cond="$1"
    local wl="$2"
    [[ $WARMUP -eq 0 ]] && return
    echo "    (warm-up: ${WARMUP} iterations)"
    run_cell "${cond}" "${wl}" /dev/null "${WARMUP}"
}

# ── Main ──────────────────────────────────────────────────────────────────────
main() {
    echo "════════════════════════════════════════════════════════════"
    echo "  WASI-USB benchmark harness"
    echo "  Results → ${OUT_DIR}"
    echo "  Smoke=${SMOKE}  Warmup=${WARMUP}"
    echo "  Workloads  : ${WORKLOADS}"
    echo "  Conditions : ${CONDITIONS}"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  *** DRY RUN — no commands executed ***"
    fi
    echo "════════════════════════════════════════════════════════════"
    echo ""

    collect_meta
    platform_tune
    echo ""

    # Split comma-separated lists into arrays
    IFS=',' read -ra WL_LIST   <<< "${WORKLOADS}"
    IFS=',' read -ra COND_LIST <<< "${CONDITIONS}"

    local total=$(( ${#WL_LIST[@]} * ${#COND_LIST[@]} ))
    local cell=0

    for wl in "${WL_LIST[@]}"; do
        wl="${wl// /}"   # strip accidental whitespace
        local iters
        iters="$(iter_for "${wl}")"
        local vidpid
        vidpid="$(vidpid_for "${wl}")"

        echo "────────────────────────────────────────────────────────────"
        echo "  Workload: ${wl}  VID:PID=${vidpid}  iter=${iters}"
        echo "────────────────────────────────────────────────────────────"

        # On macOS, the OS mounts USB mass-storage automatically and the
        # kernel driver blocks libusb from claiming the interface.
        # Unmount (not eject) the disk so libusb can take over.
        if [[ "${wl}" == "bulk" && "$(uname -s)" == "Darwin" && $DRY_RUN -eq 0 ]]; then
            local vid="${vidpid%%:*}"
            local pid="${vidpid##*:}"
            local disk
            # Find the disk node for the USB device by matching VID/PID in system_profiler
            disk=$(system_profiler SPUSBDataType 2>/dev/null \
                | awk -v v="${vid}" -v p="${pid}" '
                    tolower($0) ~ "vendor id: 0x"v { found=1 }
                    found && tolower($0) ~ "product id: 0x"p { found=2 }
                    found==2 && /BSD Name:/ { gsub(/.*BSD Name: */, ""); gsub(/[[:space:]].*/, ""); print; exit }
                ' | head -1)
            if [[ -n "${disk}" ]]; then
                echo "  [prep] Unmounting /dev/${disk} to allow libusb access..."
                ${SUDO} diskutil unmount "/dev/${disk}" 2>/dev/null \
                    && echo "  [prep] Unmounted /dev/${disk}" \
                    || echo "  [prep] unmount failed (already unmounted?)"
            else
                echo "  [prep] Could not find disk node for ${vidpid} — bulk may fail if mounted"
            fi
        fi

        for cond in "${COND_LIST[@]}"; do
            cond="${cond// /}"
            cell=$(( cell + 1 ))
            local out_csv="${OUT_DIR}/${wl}_${cond}.csv"

            echo ""
            echo "  [${cell}/${total}]  ${cond} × ${wl}  → $(basename "${out_csv}")"

            warm_cell "${cond}" "${wl}" || true
            run_cell  "${cond}" "${wl}" "${out_csv}" "${iters}" || true

            # Cooldown: let USB stack settle between cells
            if [[ $DRY_RUN -eq 0 && $SMOKE -eq 0 ]]; then
                sleep 2
            fi
        done

        echo ""
    done

    echo "════════════════════════════════════════════════════════════"
    echo "  ✓ Done — ${cell} cells — results in ${OUT_DIR}"
    if [[ $DRY_RUN -eq 0 ]]; then
        echo ""
        echo "  CSV files:"
        ls -lh "${OUT_DIR}"/*.csv 2>/dev/null \
            | awk '{printf "    %-8s  %s\n", $5, $9}' || true
    fi
    echo "════════════════════════════════════════════════════════════"
}

main "$@"
