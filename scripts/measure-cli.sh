#!/usr/bin/env bash
# Custo de `agentcenter-cli watch --json > /dev/null` (ARCHITECTURE, orcamento:
# < 2 % de um nucleo e < 10 MB de RSS a 1 Hz). Le utime+stime de
# /proc/<pid>/stat e VmRSS/VmHWM de /proc/<pid>/status do proprio processo.
#
#   scripts/measure-cli.sh [args extras do watch]
#   DURATION=60 WARMUP=2 CPU_MAX=2 RSS_MAX_KB=10240 BIN=target/release/agentcenter-cli
#
# O aquecimento fica fora da janela: o 1o tick le smaps_rollup de todos os
# processos de sessao (custo de partida, impresso a parte). Sai 1 se estourar.
set -euo pipefail
cd "$(dirname "$0")/.."

DURATION=${DURATION:-60}
WARMUP=${WARMUP:-2}
CPU_MAX=${CPU_MAX:-2}
RSS_MAX_KB=${RSS_MAX_KB:-10240}
BIN=${BIN:-target/release/agentcenter-cli}

if [[ ! -x $BIN ]]; then
    cargo build --release -p agentcenter-cli >&2
fi
hz=$(getconf CLK_TCK)

# utime+stime em ticks; o comm pode ter espaco, entao corta depois do ultimo ')'.
cpu_ticks() {
    local rest
    rest=$(sed 's/.*) //' "/proc/$1/stat")
    awk '{ print $12 + $13 }' <<<"$rest"
}
status_kb() { awk -v k="$2:" '$1 == k { print $2 }' "/proc/$1/status"; }

"$BIN" watch --json "$@" >/dev/null &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT

sleep "$WARMUP"
warm=$(cpu_ticks "$pid")
t0=$(date +%s.%N)
sleep "$DURATION"
t1=$(date +%s.%N)
end=$(cpu_ticks "$pid")
rss=$(status_kb "$pid" VmRSS)
hwm=$(status_kb "$pid" VmHWM)
kill "$pid"
wait "$pid" 2>/dev/null || true
trap - EXIT

# Glob, nao find: com pipefail, o find sai 1 quando um pid some durante a
# listagem e derruba o script antes de imprimir a medida.
pids=(/proc/[0-9]*)
procs=${#pids[@]}
awk -v w="$warm" -v e="$end" -v hz="$hz" -v t0="$t0" -v t1="$t1" -v rss="$rss" \
    -v hwm="$hwm" -v cmax="$CPU_MAX" -v rmax="$RSS_MAX_KB" -v procs="$procs" '
BEGIN {
    secs = t1 - t0
    cpu = (e - w) / hz / secs * 100
    printf "machine: %d processes\n", procs
    printf "warmup: %.0f ms cpu (first tick reads every smaps_rollup)\n", w / hz * 1000
    printf "window: %.1f s\n", secs
    printf "cpu: %.3f %% of one core (%.2f ms per second) budget %s %%\n", cpu, cpu * 10, cmax
    printf "rss: %d kB (peak %d kB) budget %d kB\n", rss, hwm, rmax
    ok = cpu < cmax && hwm < rmax
    print (ok ? "PASS" : "FAIL")
    exit (ok ? 0 : 1)
}'
