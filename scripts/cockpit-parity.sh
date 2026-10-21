#!/usr/bin/env bash
# Paridade do `agentcenter-cli cockpit` com o contexto.py do squad (C22).
#
#   scripts/cockpit-parity.sh CONTEXTO_PY [BIN]
#   AGENTCENTER_CONTEXT_SCRIPT=/path/to/contexto.py scripts/cockpit-parity.sh
#
# Roda os dois sobre as sessoes vivas desta maquina e casa por id de sessao
# (contexto.py `session` == overlay.agent_session_id). Compara janela,
# contexto %, burn ponderado, releitura, payback e veredito so nas sessoes
# cujo transcript ficou parado ha 2 min ou mais (as vivas andam entre as duas
# leituras). Diferencas conhecidas vao para listas proprias, nao contam como
# divergencia:
#   - LOOP: o contexto.py le os argumentos das ferramentas; o core nao (ADR #5,
#     sinal de loop e o C23).
#   - cwd: o core sobe do cwd do processo, o script do cwd do transcript;
#     numa worktree o handoff achado pode ser outro (docs/ARCHITECTURE.md).
# Saida: casadas / iguais / divergentes, e exit 1 se houver divergente.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
ctx_py=${1:-${AGENTCENTER_CONTEXT_SCRIPT:-}}
[[ -n $ctx_py && -f $ctx_py && -r $ctx_py ]] || {
    echo 'cockpit-parity: provide a readable CONTEXTO_PY argument or AGENTCENTER_CONTEXT_SCRIPT' >&2
    exit 2
}
bin=${2:-$root/target/release/agentcenter-cli}
tmp=${TMPDIR:-$root/target/tmp}
mkdir -p "$tmp"

[[ -x $bin ]] || (cd "$root" && cargo build -q --release -p agentcenter-cli)
python3 -I "$ctx_py" --json --all --hours 72 >"$tmp/parity-ctx.json"
"$bin" cockpit --json --all >"$tmp/parity-core.json"

python3 -I - "$tmp/parity-ctx.json" "$tmp/parity-core.json" <<'EOF'
import json, sys

ctx = json.load(open(sys.argv[1]))
ctx = ctx if isinstance(ctx, list) else ctx.get("sessions", [])
core = json.load(open(sys.argv[2]))["sessions"]

by_id = {}
for s in core:
    o = s.get("overlay") or {}
    sid = o.get("agent_session_id")
    if sid and o.get("cockpit"):
        by_id[sid] = s

def pct(c):
    return round(100.0 * c["ctx_tokens"] / c["window"], 1) if c.get("window") else None

fields = [
    ("window", lambda c, s: c["window"]),
    ("ctx_pct", lambda c, s: pct(c)),
    ("burn_weighted", lambda c, s: c["burn_weighted"]),
    ("reread_share", lambda c, s: c["reread_share"]),
    ("payback_turns", lambda c, s: c["payback_turns"]),
]

matched = equal = 0
divergent, loops, cwds, live = [], [], [], 0
for x in ctx:
    s = by_id.get(x.get("session"))
    if s is None:
        continue
    if (x.get("idle_min") or 0) < 2:
        live += 1
        continue
    matched += 1
    c = s["overlay"]["cockpit"]
    diffs = [f"{name} {x.get(name)!r} != {get(c, s)!r}"
             for name, get in fields if x.get(name) != get(c, s)]
    verdict = x.get("verdict")
    rec = c.get("recommendation")
    if verdict == "LOOP" and rec != "LOOP":
        loops.append(f"{x['session'][:8]} pid {s['pid']}: script LOOP, core {rec}")
    elif verdict != rec:
        if (x.get("cwd") or "") != (s.get("cwd") or ""):
            cwds.append(f"{x['session'][:8]} pid {s['pid']}: script {verdict}, core {rec} "
                        f"(cwd do transcript {x.get('cwd')} != cwd do processo {s.get('cwd')})")
        else:
            diffs.append(f"veredito {verdict} != {rec}")
    if diffs:
        divergent.append(f"{x['session'][:8]} pid {s['pid']} {x.get('client')}: " + "; ".join(diffs))
    else:
        equal += 1

seen = {x.get("session") for x in ctx}
unpaired = [s for s in core
            if s.get("role") == "agent" and (s.get("overlay") or {}).get("agent_session_id") not in seen]

print(f"casadas {matched} | iguais {equal} | divergentes {len(divergent)} "
      f"| vivas fora da conta {live} | agentes do core sem par no script {len(unpaired)}")
for title, rows in (("divergentes", divergent),
                    ("LOOP (C23, conhecido)", loops),
                    ("cwd do processo != cwd do transcript (conhecido)", cwds)):
    if rows:
        print(f"\n{title}:")
        for r in rows:
            print(f"  {r}")
sys.exit(1 if divergent else 0)
EOF
