#!/usr/bin/env bash
# Paridade da pagina Reclaim com o `agentcenter-cli reclaim --json` (C12).
#
#   scripts/reclaim-parity.sh [GUI_BIN] [CLI_BIN]
#
# Abre a GUI na pagina Reclaim (AGENTCENTER_PAGE=reclaim, sampler ao vivo) e,
# no mesmo instante, o CLI com a janela padrao de 10 s. A GUI imprime
# `reclaim_pids=<a,b,...> reclaim_total=<bytes>` uma vez, na primeira janela
# cheia (reclaim_page.rs), e o script a fecha. As duas leituras cobrem os
# mesmos ~10 s da maquina; compara os conjuntos de pids e o total.
#
# Uma sessao no limiar (CPU perto de 1 % de um nucleo, ou que mudou de status
# entre os ticks) pode cair de um lado so: o script mostra o status e o pid de
# cada diferenca para o leitor decidir, e sai 1. Rode de novo com a maquina
# quieta antes de chamar de divergencia.
# Saida: "gui N | cli M | iguais K | so na GUI ... | so no CLI ..." e exit
# 0 (mesmos pids), 1 (diferem) ou 2 (a GUI nao chegou a janela cheia).
# Precisa de sessao grafica (WAYLAND_DISPLAY ou DISPLAY).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
gui=${1:-$root/target/release/agentcenter}
cli=${2:-$root/target/release/agentcenter-cli}
tmp=${TMPDIR:-$root/target/tmp}
mkdir -p "$tmp"

if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
    echo "reclaim-parity: no WAYLAND_DISPLAY or DISPLAY: needs a graphical session" >&2
    exit 2
fi
[[ -x $gui && -x $cli ]] || (cd "$root" && cargo build -q --release -p agentcenter -p agentcenter-cli)

out=$tmp/reclaim-parity-gui.out
json=$tmp/reclaim-parity-cli.json
rm -f "$out" "$json"

AGENTCENTER_PAGE=reclaim AGENTCENTER_NON_UNIQUE=1 timeout 40 "$gui" >"$out" 2>"$out.err" &
gpid=$!
t0=$(date +%s.%N)
"$cli" reclaim --json >"$json"
t1=$(date +%s.%N)
# A GUI fecha a janela um tick depois do CLI (o primeiro tick dela nao tem
# taxas); espera a linha ate 15 s.
for _ in $(seq 150); do
    grep -q '^reclaim_pids=' "$out" && break
    kill -0 "$gpid" 2>/dev/null || break
    sleep 0.1
done
t2=$(date +%s.%N)
kill "$gpid" 2>/dev/null || true
wait "$gpid" 2>/dev/null || true

line=$(grep -m1 '^reclaim_pids=' "$out" || true)
if [[ -z $line ]]; then
    echo "reclaim-parity: the GUI never printed reclaim_pids= (stderr: $(head -c 400 "$out.err"))" >&2
    exit 2
fi
python3 -I - "$line" "$json" "$t0" "$t1" "$t2" <<'PY'
import json, sys

line, path, t0, t1, t2 = sys.argv[1], sys.argv[2], *map(float, sys.argv[3:6])
fields = dict(kv.split("=", 1) for kv in line.split())
gui = [int(p) for p in fields["reclaim_pids"].split(",") if p]
gui_total = int(fields.get("reclaim_total", "0"))
data = json.load(open(path))
cli_sessions = data["sessions"]
cli = [s["pid"] for s in cli_sessions]
by_pid = {s["pid"]: s for s in cli_sessions}

g, c = set(gui), set(cli)
only_gui, only_cli = sorted(g - c), sorted(c - g)
print(f"window cli {data.get('window_s')} s, cli {t1 - t0:.1f} s, gui line at {t2 - t0:.1f} s")
print(f"gui {len(g)} | cli {len(c)} | iguais {len(g & c)} | so na GUI {only_gui} | so no CLI {only_cli}")
print(f"total gui {gui_total} B | cli {data.get('total_bytes')} B")
print(f"pids {','.join(map(str, cli))}")
for p in only_cli:
    s = by_pid[p]
    print(f"  so no CLI pid {p}: {s.get('agent')} idle_by={s.get('idle_by')} max_cpu={s.get('max_cpu_pct')}")
sys.exit(0 if g == c else 1)
PY
