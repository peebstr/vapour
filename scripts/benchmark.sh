#!/usr/bin/env bash
# ==============================================================================
# Vapour Empirical Benchmarking Suite
# Measures cold startup latency, resident set size (RSS), page faults,
# and zRAM memory eviction efficiency comparing Vapour vs Native Steam GUI.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCHMARK_DIR="${REPO_ROOT}/benchmarks"
OUT_MD="${BENCHMARK_DIR}/BENCHMARK_RESULTS.md"
OUT_JSON="${BENCHMARK_DIR}/benchmark_data.json"
VAPOUR_BIN="${REPO_ROOT}/target/release/vapour"

MODE="mock"
APPID="730"
RUNS=5
AUTO_STARTED_STEAM=false

# Colors
C_RESET="\033[0m"
C_BOLD="\033[1m"
C_GREEN="\033[32m"
C_CYAN="\033[36m"
C_YELLOW="\033[33m"
C_MAGENTA="\033[35m"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --mock             Run synthetic / simulation benchmark without requiring active GUI
  --live [APPID]     Run live empirical benchmark against real Steam daemon & kernel (default)
  --runs <N>         Number of benchmark iterations for latency averaging (default: 5)
  --output <DIR>     Output directory for benchmark reports (default: ./benchmarks)
  -h, --help         Display this help message
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mock)
            MODE="mock"
            shift
            ;;
        --live)
            MODE="live"
            if [[ $# -gt 1 && ! "$2" =~ ^-- ]]; then
                APPID="$2"
                shift 2
            else
                shift
            fi
            ;;
        --runs)
            RUNS="$2"
            shift 2
            ;;
        --output)
            BENCHMARK_DIR="$2"
            OUT_MD="${BENCHMARK_DIR}/BENCHMARK_RESULTS.md"
            OUT_JSON="${BENCHMARK_DIR}/benchmark_data.json"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            ;;
    esac
done

mkdir -p "${BENCHMARK_DIR}"

# Build release binary if not present or stale
echo -e "${C_CYAN}[BUILD]${C_RESET} Ensuring release binary is compiled..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --release --quiet

echo -e "${C_MAGENTA}╔════════════════════════════════════════════════════════════════════╗${C_RESET}"
echo -e "${C_MAGENTA}║           VAPOUR EMPIRICAL PERFORMANCE BENCHMARK HARNESS           ║${C_RESET}"
echo -e "${C_MAGENTA}╚════════════════════════════════════════════════════════════════════╝${C_RESET}"
echo -e "Mode:       ${C_BOLD}${MODE}${C_RESET}"
echo -e "Iterations: ${C_BOLD}${RUNS}${C_RESET}"
echo -e "Binary:     ${VAPOUR_BIN}"
echo ""

MODE_LABEL="live measurement"
[[ "${MODE}" == "mock" ]] && MODE_LABEL="mock: placeholder values, not a measurement"

cleanup() {
    if [[ "${AUTO_STARTED_STEAM}" == "true" ]]; then
        echo -e "${C_CYAN}[CLEANUP]${C_RESET} Stopping auto-started Steam daemon..."
        "${VAPOUR_BIN}" shutdown --quiet 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ------------------------------------------------------------------------------
# Live Daemon Lifecycle Execution (if MODE == live)
# ------------------------------------------------------------------------------
PRE_RECLAIM_RSS_MB=0
POST_RECLAIM_RSS_MB=0
POST_RECLAIM_SWAP_MB=0
MEASURED_CORE_RSS_MB=0
MEASURED_CEF_RSS_MB=0
STEAM_PID=0

if [[ "${MODE}" == "live" ]]; then
    echo -e "${C_CYAN}[LIVE]${C_RESET} Checking current Steam daemon status..."
    initial_status=$("${VAPOUR_BIN}" status --json)
    is_running=$(echo "${initial_status}" | grep -o '"steam_running": *[^,]*' | awk '{print $2}')

    if [[ "${is_running}" != "true" ]]; then
        echo -e "${C_YELLOW}[LIVE]${C_RESET} Steam is not currently running. Launching headless Steam daemon via Vapour..."
        "${VAPOUR_BIN}" daemon
        AUTO_STARTED_STEAM=true

        echo -e "${C_YELLOW}[LIVE]${C_RESET} Waiting for Steam daemon and CEF webhelpers to initialize and reach steady-state..."
        for ((sec=1; sec<=20; sec++)); do
            sleep 1
            cur_status=$("${VAPOUR_BIN}" status --json 2>/dev/null || true)
            cur_running=$(echo "${cur_status}" | grep -o '"steam_running": *[^,]*' | awk '{print $2}' || echo "false")
            if [[ "${cur_running}" == "true" ]]; then
                # Allow CEF webhelpers to finish launching and settle to idle (10s)
                sleep 10
                break
            fi
        done
    else
        echo -e "${C_GREEN}[LIVE]${C_RESET} Found active Steam daemon running."
    fi

    # Read pre-reclamation state
    pre_status=$("${VAPOUR_BIN}" status --json)
    pre_rss_bytes=$(echo "${pre_status}" | grep -o '"total_rss_bytes": *[0-9]*' | awk '{print $2}')
    core_rss_bytes=$(echo "${pre_status}" | grep -o '"steam_rss_bytes": *[0-9]*' | awk '{print $2}')
    cef_rss_bytes=$(echo "${pre_status}" | grep -o '"webhelper_rss_bytes": *[0-9]*' | awk '{print $2}')
    STEAM_PID=$(echo "${pre_status}" | grep -o '"steam_pid": *[0-9]*' | awk '{print $2}' || echo "0")

    PRE_RECLAIM_RSS_MB=$((pre_rss_bytes / 1024 / 1024))
    MEASURED_CORE_RSS_MB=$((core_rss_bytes / 1024 / 1024))
    MEASURED_CEF_RSS_MB=$((cef_rss_bytes / 1024 / 1024))

    echo -e "  • Pre-Reclaim Memory:      ${C_YELLOW}${PRE_RECLAIM_RSS_MB} MB RSS${C_RESET} (Core: ${MEASURED_CORE_RSS_MB} MB, CEF: ${MEASURED_CEF_RSS_MB} MB)"

    # Trigger real kernel cgroup v2 reclamation
    echo -e "${C_CYAN}[LIVE]${C_RESET} Executing kernel cgroup v2 memory reclamation pass..."
    "${VAPOUR_BIN}" reclaim || true
    sleep 2

    # Read post-reclamation state
    post_status=$("${VAPOUR_BIN}" status --json)
    post_rss_bytes=$(echo "${post_status}" | grep -o '"total_rss_bytes": *[0-9]*' | awk '{print $2}')
    post_swap_bytes=$(echo "${post_status}" | grep -o '"total_swap_bytes": *[0-9]*' | awk '{print $2}')

    POST_RECLAIM_RSS_MB=$((post_rss_bytes / 1024 / 1024))
    POST_RECLAIM_SWAP_MB=$((post_swap_bytes / 1024 / 1024))

    echo -e "  • Post-Reclaim Memory:     ${C_GREEN}${POST_RECLAIM_RSS_MB} MB RSS${C_RESET} (${POST_RECLAIM_SWAP_MB} MB offloaded to zRAM)"
fi

# ------------------------------------------------------------------------------
# 1. Startup Latency Benchmark
# ------------------------------------------------------------------------------
echo -e "${C_CYAN}[1/4] Measuring CLI Dispatch & Startup Latency...${C_RESET}"

measure_command_latency_ms() {
    local cmd=("$@")
    local start_ns
    local end_ns
    start_ns=$(date +%s%N)
    "${cmd[@]}" > /dev/null 2>&1 || true
    end_ns=$(date +%s%N)
    local delta_ns=$((end_ns - start_ns))
    echo $((delta_ns / 1000000))
}

vapour_latencies=()
for ((i=1; i<=RUNS; i++)); do
    ms=$(measure_command_latency_ms "${VAPOUR_BIN}" status --json)
    vapour_latencies+=("$ms")
done

sum=0
for val in "${vapour_latencies[@]}"; do
    sum=$((sum + val))
done
avg_vapour_latency=$((sum / RUNS))

echo -e "  • Vapour Status Dispatch Latency: ${C_GREEN}${avg_vapour_latency} ms${C_RESET} (avg across ${RUNS} runs)"

# ------------------------------------------------------------------------------
# 2. Memory Footprint Comparison (RSS & Swap)
# ------------------------------------------------------------------------------
echo -e "${C_CYAN}[2/4] Computing Real Memory Reclaimed...${C_RESET}"

if [[ "${MODE}" == "live" ]]; then
    VAPOUR_CORE_RSS_MB=${POST_RECLAIM_RSS_MB}
    VAPOUR_SWAP_MB=${POST_RECLAIM_SWAP_MB}
else
    # Placeholders so the report pipeline can be exercised without Steam.
    PRE_RECLAIM_RSS_MB=1200
    VAPOUR_CORE_RSS_MB=40
    VAPOUR_SWAP_MB=900
fi

# Baseline is the measured Steam RSS right before reclaim, never a hardcoded figure.
RAM_SAVED_MB=$((PRE_RECLAIM_RSS_MB - VAPOUR_CORE_RSS_MB))
SAVINGS_PERCENT=$(awk "BEGIN {printf \"%.1f\", (${RAM_SAVED_MB} / ${PRE_RECLAIM_RSS_MB}) * 100}")

echo -e "  • Steam before reclaim:    ${C_YELLOW}${PRE_RECLAIM_RSS_MB} MB RSS${C_RESET} (Core: ${MEASURED_CORE_RSS_MB} MB, CEF: ${MEASURED_CEF_RSS_MB} MB)"
echo -e "  • Steam after reclaim:     ${C_GREEN}${VAPOUR_CORE_RSS_MB} MB RSS${C_RESET} (${VAPOUR_SWAP_MB} MB moved to swap/zRAM)"
echo -e "  • Net RAM Reclaimed:       ${C_BOLD}${C_GREEN}${RAM_SAVED_MB} MB (${SAVINGS_PERCENT}% reduction)${C_RESET}"

# ------------------------------------------------------------------------------
# 3. Kernel Page Faults & PSI Dynamics
# ------------------------------------------------------------------------------
echo -e "${C_CYAN}[3/4] Analyzing Kernel Page Faults & Pressure Stall Information...${C_RESET}"

# Read page faults of Steam process if running, else current shell
target_pid=${STEAM_PID:-$$}
if [[ ${target_pid} -eq 0 || ! -f "/proc/${target_pid}/stat" ]]; then
    target_pid=$$
fi

stat_fields=($(cat "/proc/${target_pid}/stat" 2>/dev/null || echo "0 0 0 0 0 0 0 0 0 0 0 0"))
min_flt=${stat_fields[9]:-0}
maj_flt=${stat_fields[11]:-0}

# Read host zRAM compression ratio
zram_ratio="1.0x"
if [[ -f /sys/block/zram0/mm_stat ]]; then
    zram_stats=($(cat /sys/block/zram0/mm_stat))
    orig_sz=${zram_stats[0]:-0}
    compr_sz=${zram_stats[1]:-0}
    if [[ ${compr_sz} -gt 0 && ${orig_sz} -gt 0 ]]; then
        zram_ratio=$(awk "BEGIN {printf \"%.2fx\", ${orig_sz} / ${compr_sz}}")
    fi
fi

# Read PSI
psi_avg10="0.00%"
if [[ -f /proc/pressure/memory ]]; then
    psi_line=$(grep "some" /proc/pressure/memory | head -n 1 || true)
    if [[ -n "${psi_line}" ]]; then
        psi_avg10=$(echo "${psi_line}" | grep -o 'avg10=[0-9.]*' | cut -d'=' -f2 || echo "0.00")"%"
    fi
fi

echo -e "  • Target PID Profiled:      ${target_pid}"
echo -e "  • Kernel Minor Page Faults: ${min_flt}"
echo -e "  • Kernel Major Page Faults: ${maj_flt}"
echo -e "  • zRAM Compression Ratio:   ${C_BOLD}${zram_ratio}${C_RESET}"
echo -e "  • PSI Memory Stall avg10:   ${psi_avg10}"

# ------------------------------------------------------------------------------
# 4. Report Generation (Markdown & JSON)
# ------------------------------------------------------------------------------
echo -e "${C_CYAN}[4/4] Generating Benchmark Artifacts...${C_RESET}"

timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat <<EOF > "${OUT_MD}"
# Vapour Benchmark Report

**Generated:** ${timestamp}  
**Architecture:** $(uname -m)  
**Kernel:** $(uname -r)  
**Mode:** ${MODE} (${MODE_LABEL})

---

## 1. Summary

| Metric | Before reclaim | After reclaim | Delta |
| :--- | :---: | :---: | :---: |
| **Steam + CEF RSS** | ${PRE_RECLAIM_RSS_MB} MB | ${VAPOUR_CORE_RSS_MB} MB | -${RAM_SAVED_MB} MB (${SAVINGS_PERCENT}%) |
| **Steam swap / zRAM** | — | ${VAPOUR_SWAP_MB} MB | +${VAPOUR_SWAP_MB} MB |
| **Host zRAM compression ratio** | — | ${zram_ratio} | — |

RSS moved to swap still costs its compressed size in zRAM; the net saving is lower than the RSS delta.

---

## 2. Details

- **Target process profiled:** PID \`${target_pid}\`
- **RSS before reclaim:** \`${PRE_RECLAIM_RSS_MB} MB\` (Core: \`${MEASURED_CORE_RSS_MB} MB\`, CEF webhelpers: \`${MEASURED_CEF_RSS_MB} MB\`)
- **RSS after reclaim:** \`${VAPOUR_CORE_RSS_MB} MB\`
- **Moved to swap/zRAM:** \`${VAPOUR_SWAP_MB} MB\`
- **\`vapour status --json\` runtime:** \`${avg_vapour_latency} ms\` (average of ${RUNS} runs; this is CLI overhead, not game launch time)
- **PSI memory some avg10:** \`${psi_avg10}\`
- **Page faults of the profiled process:** \`${min_flt}\` minor, \`${maj_flt}\` major
EOF

cat <<EOF > "${OUT_JSON}"
{
  "timestamp": "${timestamp}",
  "mode": "${MODE}",
  "iterations": ${RUNS},
  "metrics": {
    "target_pid": ${target_pid},
    "pre_reclaim_rss_mb": ${PRE_RECLAIM_RSS_MB},
    "pre_reclaim_core_rss_mb": ${MEASURED_CORE_RSS_MB},
    "pre_reclaim_cef_rss_mb": ${MEASURED_CEF_RSS_MB},
    "vapour_core_rss_mb": ${VAPOUR_CORE_RSS_MB},
    "vapour_swap_mb": ${VAPOUR_SWAP_MB},
    "ram_saved_mb": ${RAM_SAVED_MB},
    "savings_percentage": ${SAVINGS_PERCENT},
    "status_cmd_ms": ${avg_vapour_latency},
    "zram_compression_ratio": "${zram_ratio}",
    "psi_avg10": "${psi_avg10}",
    "minor_faults": ${min_flt},
    "major_faults": ${maj_flt}
  }
}
EOF

echo -e "  • Markdown report: ${C_GREEN}${OUT_MD}${C_RESET}"
echo -e "  • JSON dataset:   ${C_GREEN}${OUT_JSON}${C_RESET}"
echo ""
echo -e "${C_GREEN}✔ Benchmark completed (${MODE_LABEL}).${C_RESET}"
