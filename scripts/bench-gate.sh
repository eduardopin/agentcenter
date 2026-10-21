#!/usr/bin/env bash
# Portao de desempenho do tick: le as medianas que o criterion gravou e falha se
# alguma fixture de 500 processos conferida passar do orcamento (ARCHITECTURE: < 5 ms).
#
# Uso: scripts/bench-gate.sh            (depois de `cargo bench -p agentcenter-core --bench tick`)
#
# Variaveis:
#   BENCH_GATE_MS      limite em ms; falha se mediana >= limite (padrao 5).
#   BENCH_GATE_ID      prefixo dos ids do criterion conferidos (padrao tick_fixture_500:
#                      tick_fixture_500_delayacct e tick_fixture_500_d_state).
#   BENCH_GATE_SINCE   arquivo-marco: so conta estimates.json mais novo que ele, para
#                      que resultado velho de bench renomeado nao passe no lugar do novo.
#   BENCH_GATE_HEADROOM folga desejada (padrao 2): abaixo dela o portao passa, mas avisa.
#   CRITERION_DIR      onde o criterion grava (padrao ${CARGO_TARGET_DIR:-target}/criterion).
#
# Margem para runner compartilhado: o limite e o proprio orcamento, sem folga
# embutida. A folga fica do lado da medicao: localmente a mediana deve ficar a
# BENCH_GATE_HEADROOM vezes abaixo do limite, porque o ubuntu-latest (vCPU
# compartilhada, vizinhos barulhentos) roda o mesmo tick mais devagar. Usa a
# mediana, nao a media, porque ela resiste as amostras atrasadas por vizinho.
# Se o CI ficar vermelho so no runner, a resposta e deixar o tick mais rapido,
# nao subir BENCH_GATE_MS.
#
# Saida: 0 dentro do limite; 1 alguma mediana >= limite; 2 uso errado ou nenhum
# resultado encontrado (bench que nao rodou nao pode passar).
set -euo pipefail

limit_ms="${BENCH_GATE_MS:-5}"
prefix="${BENCH_GATE_ID:-tick_fixture_500}"
since="${BENCH_GATE_SINCE:-}"
headroom="${BENCH_GATE_HEADROOM:-2}"
criterion_dir="${CRITERION_DIR:-${CARGO_TARGET_DIR:-target}/criterion}"

exec python3 -I -u - "$limit_ms" "$prefix" "$since" "$headroom" "$criterion_dir" <<'PY'
import json
import math
import os
import sys

limit_raw, prefix, since, headroom_raw, criterion_dir = sys.argv[1:6]


def die(msg):
    print(f"bench-gate: {msg}", file=sys.stderr)
    sys.exit(2)


def positive(name, raw):
    try:
        v = float(raw)
    except ValueError:
        die(f"{name}=`{raw}` is not a number")
    if not math.isfinite(v) or v <= 0:
        die(f"{name}=`{raw}` must be a finite number > 0")
    return v


limit_ms = positive("BENCH_GATE_MS", limit_raw)
headroom = positive("BENCH_GATE_HEADROOM", headroom_raw)
if not prefix:
    die("BENCH_GATE_ID is empty")
if not os.path.isdir(criterion_dir):
    die(f"{criterion_dir} not found: run `cargo bench -p agentcenter-core --bench tick` first")

since_ns = None
if since:
    try:
        since_ns = os.stat(since).st_mtime_ns
    except OSError as e:
        die(f"BENCH_GATE_SINCE: {e}")

results = []
for name in sorted(os.listdir(criterion_dir)):
    if not name.startswith(prefix):
        continue
    path = os.path.join(criterion_dir, name, "new", "estimates.json")
    if not os.path.isfile(path):
        continue
    if since_ns is not None and os.stat(path).st_mtime_ns <= since_ns:
        continue
    try:
        with open(path) as f:
            median_ns = float(json.load(f)["median"]["point_estimate"])
    except (OSError, ValueError, KeyError, TypeError) as e:
        die(f"{path}: cannot read median ({e})")
    if not math.isfinite(median_ns) or median_ns <= 0:
        die(f"{path}: median `{median_ns}` is not a positive number")
    results.append((name, median_ns / 1e6))

if not results:
    scope = " newer than BENCH_GATE_SINCE" if since_ns is not None else ""
    die(f"no criterion result for `{prefix}*` in {criterion_dir}{scope}")

failed = False
print(f"bench-gate: median < {limit_ms:g} ms (want {headroom:g}x headroom on this machine)")
for name, ms in results:
    over = ms >= limit_ms
    failed |= over
    margin = limit_ms / ms
    if over:
        verdict = "FAIL"
    elif margin < headroom:
        verdict = "ok, low headroom"
    else:
        verdict = "ok"
    print(f"  {name:<32} {ms:9.3f} ms  {margin:6.2f}x  {verdict}")

if failed:
    print(f"bench-gate: FAIL: median >= {limit_ms:g} ms", file=sys.stderr)
    sys.exit(1)
low = [n for n, ms in results if limit_ms / ms < headroom]
if low:
    print(
        f"bench-gate: warning: headroom under {headroom:g}x for {', '.join(low)}; "
        "a shared CI runner may cross the limit",
        file=sys.stderr,
    )
print("bench-gate: ok")
PY
