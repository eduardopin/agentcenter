#!/usr/bin/env bash
# Custo da GUI (ARCHITECTURE, orcamento: aberta a 1 Hz < 2 % de um nucleo e
# < 80 MB de PSS; cold start ate o primeiro frame < 300 ms).
#
#   scripts/measure-gui.sh
#   RUNS=5 WARMUP=10 DURATION=60 CPU_MAX=2 PSS_MAX_KB=81920 START_MAX_MS=300
#   BIN=target/release/agentcenter
#
# 1. Cold start: RUNS execucoes com AGENTCENTER_EXIT_AFTER_FIRST_FRAME=1; a app
#    imprime o tempo do exec (AGENTCENTER_T0_NS, gravado aqui logo antes) ate o
#    primeiro frame pintado e ate o primeiro frame com dados. Portao: mediana
#    do primeiro frame.
# 2. Janela aberta: WARMUP s de aquecimento fora da janela, depois DURATION s.
#    CPU por utime+stime de /proc/<pid>/stat (todas as threads, sampler
#    incluso); PSS e SwapPss de /proc/<pid>/smaps_rollup no fim.
# A carga da maquina (loadavg) sai antes e depois: sob carga alta o numero de
# CPU e de partida e ruido de escalonamento. Sai 1 se estourar um orcamento.
# GSK_RENDERER=vulkan|gl|cairo mede outro renderer (a app respeita o do usuario).
# Precisa de sessao grafica (WAYLAND_DISPLAY ou DISPLAY).
#
# --threads: na janela aberta, CPU tambem por thread (/proc/<pid>/task/<tid>/stat),
# com a thread GTK (a principal, tid == pid) numa linha propria. Com
# AGENTCENTER_SNAPSHOT_JSON=<fixture> a app nao sobe o sampler e o custo e o da
# UI aplicando o mesmo frame a cada tick, por exemplo:
#   AGENTCENTER_SNAPSHOT_JSON=crates/agentcenter/tests/fixtures/snapshots/tipico-40.json \
#       scripts/measure-gui.sh --threads
#
# --detail: as execucoes abrem o detalhe da sessao com mais processos
# (AGENTCENTER_DETAIL=busiest, C11). Com o sampler, a selecao vai a ele:
# smaps_rollup e task/ por tick so nessa sessao, o custo real de ter o
# detalhe aberto. A linha "detail:" diz o pid e quantos processos ele tem;
# se a app nao abrir o detalhe, a medida nao vale e o script sai 1.
set -euo pipefail
cd "$(dirname "$0")/.."

THREADS=0
DETAIL=
for arg in "$@"; do
    case $arg in
        --threads) THREADS=1 ;;
        --detail) DETAIL=busiest ;;
        *) echo "measure-gui: unknown option $arg (--threads, --detail)" >&2; exit 2 ;;
    esac
done

RUNS=${RUNS:-5}
WARMUP=${WARMUP:-10}
DURATION=${DURATION:-60}
CPU_MAX=${CPU_MAX:-2}
PSS_MAX_KB=${PSS_MAX_KB:-81920}
START_MAX_MS=${START_MAX_MS:-300}
BIN=${BIN:-target/release/agentcenter}

if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
    echo "measure-gui: no WAYLAND_DISPLAY or DISPLAY: needs a graphical session" >&2
    exit 2
fi
if [[ ! -x $BIN ]]; then
    cargo build --release -p agentcenter >&2
fi
hz=$(getconf CLK_TCK)
load() { cut -d' ' -f1-3 /proc/loadavg; }

cpu_ticks() {
    local rest
    rest=$(sed 's/.*) //' "/proc/$1/stat")
    awk '{ print $12 + $13 }' <<<"$rest"
}
rollup_kb() { awk -v k="$2:" '$1 == k { print $2 }' "/proc/$1/smaps_rollup"; }

# "tid<TAB>comm<TAB>utime+stime" por thread; thread que sumiu no meio e pulada.
thread_ticks() {
    local t rest comm
    for t in "/proc/$1/task/"*; do
        comm=$(cat "$t/comm" 2>/dev/null) || continue
        rest=$(sed 's/.*) //' "$t/stat" 2>/dev/null) || continue
        printf '%s\t%s\t%s\n' "${t##*/}" "$comm" "$(awk '{ print $12 + $13 }' <<<"$rest")"
    done
}

median() { sort -n | awk '{ v[NR] = $1 } END { print (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2 }'; }

if [[ -n $DETAIL ]]; then
    export AGENTCENTER_DETAIL=$DETAIL
fi

echo "load before: $(load)"
echo "renderer: ${GSK_RENDERER:-app default (cairo, crates/agentcenter/src/renderer.rs)}"

frames=() datas=()
for ((i = 1; i <= RUNS; i++)); do
    out=$(AGENTCENTER_T0_NS=$(date +%s%N) AGENTCENTER_EXIT_AFTER_FIRST_FRAME=1 \
        timeout 30 "$BIN" 2>/dev/null)
    f=$(sed -n 's/.*first_frame_ms=\([0-9.]*\).*/\1/p' <<<"$out")
    d=$(sed -n 's/.*first_data_ms=\([0-9.]*\).*/\1/p' <<<"$out")
    [[ -n $f && -n $d ]] || { echo "measure-gui: run $i printed no timing: $out" >&2; exit 1; }
    echo "cold start $i: first frame $f ms, first data frame $d ms"
    frames+=("$f") datas+=("$d")
done
start_med=$(printf '%s\n' "${frames[@]}" | median)
data_med=$(printf '%s\n' "${datas[@]}" | median)

# A saida da janela aberta vai para um arquivo em target/ (nunca /tmp): o
# --detail le dela qual sessao foi aberta.
mkdir -p target
outlog=$(mktemp -p target measure-gui.XXXXXX)
AGENTCENTER_NON_UNIQUE=1 "$BIN" >"$outlog" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; rm -f "$outlog"' EXIT
sleep "$WARMUP"
kill -0 "$pid" 2>/dev/null || { echo "measure-gui: the app exited during warmup" >&2; exit 1; }
warm=$(cpu_ticks "$pid")
((THREADS)) && warm_threads=$(thread_ticks "$pid")
t0=$(date +%s.%N)
sleep "$DURATION"
t1=$(date +%s.%N)
end=$(cpu_ticks "$pid")
((THREADS)) && end_threads=$(thread_ticks "$pid")
pss=$(rollup_kb "$pid" Pss)
swap=$(rollup_kb "$pid" SwapPss)
threads=$(awk '$1 == "Threads:" { print $2 }' "/proc/$pid/status")
kill "$pid"
wait "$pid" 2>/dev/null || true
trap - EXIT
detail_line=$(sed -n 's/^detail_pid=\([0-9]*\) detail_procs=\([0-9]*\).*/pid \1, \2 processes/p' "$outlog" | head -n1)
detail_err=$(grep -F AGENTCENTER_DETAIL "$outlog" || true)
rm -f "$outlog"

echo "load after: $(load)"
if [[ -n ${AGENTCENTER_SNAPSHOT_JSON:-} ]]; then
    echo "source: fixture $AGENTCENTER_SNAPSHOT_JSON"
else
    echo "source: sampler (this machine)"
fi
if [[ -n $DETAIL ]]; then
    if [[ -z $detail_line ]]; then
        echo "measure-gui: --detail: the app opened no detail${detail_err:+ ($detail_err)}" >&2
        exit 1
    fi
    echo "detail: $detail_line (AGENTCENTER_DETAIL=$DETAIL)"
fi
if ((THREADS)); then
    # Thread presente nas duas leituras; nova no meio conta do zero.
    awk -F'\t' -v pid="$pid" -v hz="$hz" -v secs="$(awk -v a="$t0" -v b="$t1" 'BEGIN { print b - a }')" '
    FNR == NR { w[$1] = $3; next }
    {
        cpu = ($3 - ($1 in w ? w[$1] : 0)) / hz / secs * 100
        role = ($1 == pid) ? "gtk main loop" : $2
        printf "thread %s %-16s %-14s %.3f %% of one core\n", $1, $2, "(" role ")", cpu
        if ($1 == pid) gtk = cpu
    }
    END { printf "gtk thread: %.3f %% of one core\n", gtk }
    ' <(printf '%s\n' "$warm_threads") <(printf '%s\n' "$end_threads")
fi
awk -v w="$warm" -v e="$end" -v hz="$hz" -v t0="$t0" -v t1="$t1" -v pss="$pss" \
    -v swap="$swap" -v th="$threads" -v cmax="$CPU_MAX" -v pmax="$PSS_MAX_KB" \
    -v sm="$start_med" -v dm="$data_med" -v smax="$START_MAX_MS" -v runs="$RUNS" -v warmup="$WARMUP" '
BEGIN {
    secs = t1 - t0
    cpu = (e - w) / hz / secs * 100
    printf "cold start: median first frame %.1f ms, first data frame %.1f ms (%d runs) budget %s ms\n", sm, dm, runs, smax
    printf "window: %.1f s after %s s warmup, %d threads\n", secs, warmup, th
    printf "cpu: %.3f %% of one core (%.2f ms per second) budget %s %%\n", cpu, cpu * 10, cmax
    printf "pss: %d kB (%.1f MB), swap pss %d kB, budget %d kB\n", pss, pss / 1024, swap, pmax
    ok = cpu < cmax && pss < pmax && sm < smax
    print (ok ? "PASS" : "FAIL")
    exit (ok ? 0 : 1)
}'
