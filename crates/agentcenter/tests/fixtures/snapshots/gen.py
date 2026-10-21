#!/usr/bin/env python3
"""Gera as fixtures sinteticas de Snapshot do modo fixture da GUI.

    python3 -I crates/agentcenter/tests/fixtures/snapshots/gen.py

Nada aqui vem da maquina: nomes, caminhos e numeros sao inventados, com seed
fixa, para a captura (scripts/capture-gui.sh) sair igual em toda rodada. O
formato e o do `Snapshot` do core (schema 5), com o `summary` do C13 calculado
das proprias sessoes pela mesma regra de `Summary::of` (summary.rs), para C17
conferir a faixa com `jq .summary`. Do C21 vem o `provider` de cada overlay
(regra de `cockpit::provider`) e os agregados do cockpit no resumo; nas quatro
primeiras o `overlay.cockpit` sai sempre `null` (como o `tokens_in`: o core so
publica os dois depois da varredura completa do transcript), com algumas
sessoes em `cockpit_pending`. A `cockpit.json` traz o cockpit preenchido: as
cinco recomendacoes, uma sem dado, uma pendente, providers anthropic, openai,
deepinfra e nenhum, com as contas de `Thresholds::contas` e `recommend`
(cockpit/mod.rs) portadas abaixo; `tests/snapshot_json.rs` do core as refaz
dos tokens de cada sessao e compara o objeto inteiro. O teste
`snapshot_fixtures_follow_the_core_summary_and_schema` (feed.rs) compara as
chaves e os numeros com o core: campo novo no `Summary` exige rodar isto de novo.
"""

import json
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent
GIB = 1 << 30
MIB = 1 << 20
HOME = "/home/dev"
CORES = 16

PROJECTS = [
    "atlas-api", "billing", "ledger", "orbit-web", "harbor", "quill",
    "relay", "tundra", "vesper", "kiln", "mosaic", "nimbus", "sable",
    "pylon", "garnet", "lumen",
]
TOPICS = [
    "auth-refactor", "flaky-tests", "migration-v4", "perf-pass", "docs",
    "release-notes", "bugfix-1287", "search-index", "cache-layer",
    "retry-policy", "i18n", "onboarding", "rate-limit", "export-csv",
]
CHILDREN = ["node", "bash", "cargo", "rustc", "python3", "npm", "git", "rg"]
MODELS = {
    "claude": ["claude-opus-5-5", "claude-sonnet-5", "claude-haiku-5"],
    "codex": ["gpt-6-codex", "gpt-6-codex-mini"],
    "pi": ["pi-local-32b"],
}


def process(rng, pid, ppid, comm, big):
    pss = rng.randint(120, 520) * MIB if big else rng.randint(2, 60) * MIB
    swap = int(pss * rng.choice([0.0, 0.3, 0.8, 1.2])) if big else int(pss * rng.random() * 0.5)
    spread = [0.5, 1.0, 2.0, 3.0, 6.0, 25.0, 60.0] if big else [0.0, 0.0, 0.0, 0.0, 0.2, 0.8, 3.0, 15.0]
    cpu = round(rng.choice(spread) * rng.random(), 2)
    return {
        "pid": pid, "ppid": ppid, "comm": comm,
        "state": "R" if cpu >= 10 else rng.choice("SSSSSSSSSR"),
        "uid": 1000, "threads": rng.randint(1, 24) if big else rng.randint(1, 6),
        "cpu_pct": cpu, "rss_bytes": int(pss * 1.4), "pss_bytes": pss,
        "swap_bytes": int(swap * 1.2), "swap_pss_bytes": swap,
        "read_bps": float(rng.choice([0, 0, 0, 4096, 65536, 2 * MIB])),
        "write_bps": float(rng.choice([0, 0, 8192, 131072])),
        "rchar_bps": 0.0, "wchar_bps": 0.0,
        "stall_pct": round(rng.choice([0.0] * 12 + [1.0, 4.0, 18.0]), 1),
        "threads_d": None,
        "voluntary_ctxt_switches": rng.randint(100, 90000),
        "nonvoluntary_ctxt_switches": rng.randint(10, 9000),
    }


# Provider por agente e por prefixo do modelo, como `cockpit::provider` (mod.rs).
PROVIDER_BY_AGENT = {
    "claude": "anthropic", "codex": "openai", "gemini": "google",
    "copilot": "github", "kiro": "aws", "cursor-agent": "cursor",
}
PROVIDER_BY_MODEL = [
    ("claude-", "anthropic"), ("anthropic/", "anthropic"), ("gpt-", "openai"),
    ("o1", "openai"), ("o3", "openai"), ("o4", "openai"), ("codex-", "openai"),
    ("openai/", "openai"), ("gemini-", "google"), ("google/", "google"),
]


def provider(agent, model):
    """`cockpit::provider` sem provider declarado (as fixtures nao tem transcript)."""
    if agent in PROVIDER_BY_AGENT:
        return PROVIDER_BY_AGENT[agent]
    model = (model or "").strip().lower()
    return next((pid for prefix, pid in PROVIDER_BY_MODEL if model.startswith(prefix)), None)


def session(rng, agent, pid, name, cwd, status, role="agent", children=None, herdr=True, pending=False):
    n = rng.randint(1, 10) if children is None else children
    procs = [process(rng, pid, 1, agent, True)]
    for i in range(n):
        procs.append(process(rng, pid + 1 + i, pid, rng.choice(CHILDREN), False))
    total = lambda k: sum(p[k] for p in procs)
    cpu = round(total("cpu_pct"), 2)
    stall = max(p["stall_pct"] for p in procs)
    overlay = None
    if role == "agent":
        ws = rng.randint(1, 6)
        pane = rng.randint(1, 40)
        overlay = {
            "name": name,
            "agent_session_id": f"00000000-0000-4000-8000-{pid:012d}",
            "status": status,
            "kind": "interactive",
            "herdr": {
                "workspace_id": f"w{ws}", "tab_id": f"w{ws}:t{pane}",
                "pane_id": f"w{ws}:p{pane}", "title": name,
                "status": {"busy": "working", "blocked": "blocked", "idle": "idle"}.get(status, "idle"),
            } if herdr else None,
            "model": rng.choice(MODELS.get(agent, ["unknown"])),
            "tokens_in": None, "tokens_out": None,
            "context_pct": round(rng.uniform(4, 97), 1),
            # Reservados no schema 4 (C14/C15): nenhum overlay preenche ainda.
            "tool_calls": None, "tool_calls_open": None, "tool_calls_per_min": None,
            "subagents_live": None, "subagents_total": None,
        }
        # Schema 5 (C21), fora do rng: as fixtures do C16 saem com os mesmos numeros.
        overlay["provider"] = provider(agent, overlay["model"])
        overlay["cockpit"] = None
        overlay["cockpit_pending"] = pending
    return {
        "agent": agent, "role": role, "pid": pid, "cmdline": agent, "cwd": cwd,
        "procs": len(procs), "cpu_pct": cpu, "cpu_share_pct": cpu / CORES,
        "pss_bytes": total("pss_bytes"), "swap_pss_bytes": total("swap_pss_bytes"),
        "read_bps": total("read_bps"), "write_bps": total("write_bps"),
        "stall_pct": stall, "threads_d": None,
        "io_bound": stall >= 10.0 and cpu < 5.0,
        "partial": [], "overlay": overlay, "processes": procs,
    }


def machine(sessions, stall_source="delayacct", mem_used=29.3, swap_used=19.7, psi_mem=12.5, psi_io=3.1):
    return {
        "cores": CORES, "cpu_pct": 41.8, "io_stall_source": stall_source,
        "processes": 861 if sessions else 402,
        "session_processes": sum(s["procs"] for s in sessions),
        "mem_total_bytes": 38 * GIB, "mem_available_bytes": int((38 - mem_used) * GIB),
        "swap_total_bytes": 27 * GIB, "swap_free_bytes": int((27 - swap_used) * GIB),
        "load_avg": {"one": 13.4, "five": 13.9, "fifteen": 14.7},
        "psi": {
            "cpu": {"some_avg10": 1.1, "some_avg60": 6.7, "full_avg10": 0.0, "full_avg60": 0.0},
            "memory": {"some_avg10": psi_mem, "some_avg60": psi_mem / 2, "full_avg10": psi_mem / 3, "full_avg60": 0.3},
            "io": {"some_avg10": psi_io, "some_avg60": 0.6, "full_avg10": 0.03, "full_avg60": 0.2},
        },
    }


WORKING_CPU_PCT = 1.0  # summary.rs


def working(p):
    return p["state"] in ("R", "D") or (p["cpu_pct"] is not None and p["cpu_pct"] >= WORKING_CPU_PCT)


def total_or_none(sessions, key):
    """Soma das sessoes que tem o valor; None se nenhuma tem (como Summary::of)."""
    vals = [s[key] for s in sessions if s[key] is not None]
    return sum(vals) if vals else None


def state(s):
    """session_state do core: overlay.status, depois o status do pane herdr."""
    o = s["overlay"]
    if o is None:
        return "unknown"
    if o["status"] in ("busy", "blocked", "idle"):
        return o["status"]
    herdr = (o["herdr"] or {}).get("status")
    return {"working": "busy", "blocked": "blocked", "idle": "idle", "done": "idle"}.get(herdr, "unknown")


RECOMMENDATIONS = {"CONTINUAR": "continuar", "COMPACTAR": "compactar", "HANDOFF": "handoff",
                   "OCIOSA": "ociosa", "LOOP": "loop"}


def cockpit_summary(agents):
    """Os agregados do cockpit de `Summary::of`, so das sessoes de agente."""
    counts = {"continuar": 0, "compactar": 0, "handoff": 0, "ociosa": 0, "loop": 0, "sem_dado": 0}
    pending, burn, read, tokens_in = 0, None, 0, 0
    for s in agents:
        o = s["overlay"] or {}
        k = o.get("cockpit")
        if k is None:
            counts["sem_dado"] += 1
            pending += bool(o.get("cockpit_pending"))
            continue
        counts[RECOMMENDATIONS[k["recommendation"]]] += 1
        burn = (burn or 0) + k["burn_weighted"]
        if o.get("tokens_in") is not None:
            read += k["cache_read"]
            tokens_in += o["tokens_in"]
    ctx = [s["overlay"]["context_pct"] for s in agents
           if s["overlay"] and s["overlay"].get("context_pct") is not None]
    return {
        "cockpit": counts,
        "cockpit_pending": pending,
        "sessions_burn_weighted": burn,
        # Ponderado pelos tokens; o round do Python e o `round_n` do core.
        "cache_hit": round(read / tokens_in, 3) if tokens_in > 0 else None,
        "ctx_pct_max": max(ctx) if ctx else None,
    }


def summary(sessions):
    """Mesmos campos e mesma regra de `Summary::of`; nada inventado."""
    agents = [s for s in sessions if s["role"] == "agent"]
    by_agent = {}
    status = {"busy": 0, "blocked": 0, "idle": 0, "unknown": 0}
    for s in agents:
        by_agent[s["agent"]] = by_agent.get(s["agent"], 0) + 1
        status[state(s)] += 1
    children = lambda s: [p for p in s["processes"] if p["pid"] != s["pid"]]
    child = [p for s in sessions for p in children(s)]
    return {
        "sessions": len(agents),
        "multiplexers": sum(1 for s in sessions if s["role"] == "multiplexer"),
        "by_agent": dict(sorted(by_agent.items())),
        "status": status,
        "sessions_with_working_children": sum(1 for s in agents if any(working(p) for p in children(s))),
        "child_processes": len(child),
        "working_child_processes": sum(1 for p in child if working(p)),
        "io_bound_sessions": sum(1 for s in sessions if s["io_bound"]),
        "sessions_cpu_pct": total_or_none(sessions, "cpu_pct"),
        "sessions_pss_bytes": total_or_none(sessions, "pss_bytes"),
        "sessions_swap_pss_bytes": total_or_none(sessions, "swap_pss_bytes"),
        "tool_calls_open": None,
        "tool_calls_per_min": None,
        "subagents_live": None,
        **cockpit_summary(agents),
    }


def snapshot(sessions, **mk):
    sessions.sort(key=lambda s: -(s["pss_bytes"] + s["swap_pss_bytes"]))
    return {
        "schema_version": 5, "taken_at_ms": 1_792_200_000_000, "interval_ms": 1000,
        "machine": machine(sessions, **mk), "sessions": sessions, "summary": summary(sessions),
    }


# Limiares padrao de `Thresholds::default()` (cockpit/mod.rs), sem ambiente.
W_WRITE, W_READ, W_OUT = 1.25, 0.1, 5.0
HANDOFF_SHOW, HANDOFF_OUT, IDLE_MIN, HANDOFF_OLD_MIN = 50, 2_000, 90.0, 240.0
HANDOFF_NOW, REREAD_SHARE, REREAD_FLOOR_PCT = 90, 0.6, 70
BUDGET, BURN_FULL_PCT, CTX_BASE = 20_000_000, 50, 40_000


def contas(inp, wr, rd, out, ctx, window):
    """`Thresholds::contas`, mesma ordem de operacoes (que e a do contexto.py)."""
    novo = inp + wr * W_WRITE + out * W_OUT
    relei = rd * W_READ
    saving = (ctx - CTX_BASE) * W_READ
    restart = CTX_BASE * W_WRITE + HANDOFF_OUT * W_OUT
    share = relei / (novo + relei) if novo + relei != 0.0 else 0.0
    burn_pct = 100.0 * (novo + relei) / BUDGET
    now = float(HANDOFF_NOW)
    low = min(REREAD_FLOOR_PCT, HANDOFF_NOW)
    floor_pct = now - (now - low) * min(burn_pct / BURN_FULL_PCT, 1.0)
    piso = max(2 * CTX_BASE, int(window * floor_pct / 100.0))
    return {
        "burn": int(novo + relei), "burn_pct": round(burn_pct, 1),
        "razao": round(relei / novo, 3) if novo != 0.0 else 0.0,
        "share": round(share, 3), "piso": piso, "turno": int(ctx * W_READ),
        "payback": round(restart / saving, 1) if saving > 0.0 else None,
        "trocar": share >= REREAD_SHARE and ctx >= piso,
    }


def recommend(ctx_pct, c, idle_min, handoff_age_min, loop_repeats):
    """`Thresholds::recommend`: veredito e motivos, na precedencia do script."""
    verdict, why = "CONTINUAR", []
    if idle_min is not None and idle_min > IDLE_MIN:
        verdict = "OCIOSA"
        why.append({"kind": "ociosa", "idle_min": idle_min})
    if loop_repeats is not None:
        verdict = "LOOP"
        why.append({"kind": "loop", "repeats": loop_repeats})
    if ctx_pct >= HANDOFF_NOW:
        verdict = "HANDOFF"
        why.append({"kind": "contexto_urgente", "ctx_pct": ctx_pct, "limite_pct": HANDOFF_NOW})
    elif ctx_pct >= HANDOFF_SHOW and verdict == "CONTINUAR":
        verdict = "COMPACTAR"
        why.append({"kind": "contexto_alto", "ctx_pct": ctx_pct, "limite_pct": HANDOFF_SHOW})
    if c["trocar"]:
        if verdict in ("CONTINUAR", "COMPACTAR"):
            verdict = "HANDOFF"
        why.append({"kind": "releitura", "share": c["share"], "limite": REREAD_SHARE,
                    "piso": c["piso"], "turno": c["turno"], "payback": c["payback"]})
    if handoff_age_min is not None and ctx_pct >= HANDOFF_SHOW and handoff_age_min > HANDOFF_OLD_MIN:
        why.append({"kind": "handoff_velho", "age_min": handoff_age_min})
    return verdict, why


LOOP_REPEATS = 3


def loop_repeats(max_streak, repeated_max):
    """`Loops::repeats` (C23): `rep or streak >= 3`, com a maior das duas."""
    if repeated_max >= LOOP_REPEATS or max_streak >= LOOP_REPEATS:
        return max(repeated_max, max_streak)
    return None


def with_cockpit(s, model, tokens, window, inferred, idle_min, handoff=None, loops=(1, 1),
                 provider_declared=None):
    """Preenche o overlay de `s` como o core depois da varredura completa.

    `tokens` = (entrada sem cache, cache write, cache read, saida, contexto do
    ultimo pedido); `handoff` = (nome do arquivo, idade em minutos); `loops` =
    (max_streak, repeated_max) do sinal de loop do C23."""
    inp, wr, rd, out, ctx = tokens
    o = s["overlay"]
    c = contas(inp, wr, rd, out, ctx, window)
    pct = 100.0 * ctx / window if window else 0.0  # `cockpit::ctx_pct`, sem arredondar
    age = handoff[1] if handoff else None
    verdict, why = recommend(pct, c, idle_min, age, loop_repeats(*loops))
    total = inp + wr + rd
    o["model"] = model
    o["tokens_in"], o["tokens_out"] = total, out
    o["context_pct"] = round(pct, 1)
    o["provider"] = provider_declared or provider(s["agent"], model)
    o["cockpit"] = {
        "burn_weighted": c["burn"], "burn_pct": c["burn_pct"],
        "cache_read": rd, "cache_write": wr,
        "cache_hit": round(rd / total, 3) if total > 0 else None,
        "reread_share": c["share"], "reread_ratio": c["razao"],
        "payback_turns": c["payback"], "floor_tokens": c["piso"],
        "ctx_tokens": ctx, "window": window, "window_inferred": inferred,
        "idle_min": idle_min, "handoff_age_min": age,
        "handoff_name": handoff[0] if handoff else None,
        "max_streak": loops[0], "repeated_max": loops[1],
        "recommendation": verdict, "reasons": why,
    }
    o["cockpit_pending"] = False
    return s


def cockpit():
    """Uma sessao por recomendacao, uma sem dado e uma pendente (numeros inventados)."""
    rng = random.Random(21)
    out, pid = [], 50000

    def add(agent, name, status, **kw):
        nonlocal pid
        s = session(rng, agent, pid, name, f"{HOME}/src/{name.split('-')[0]}", status, **kw)
        pid += 40
        out.append(s)
        return s

    # Contexto em 18 %, releitura em 0,36: segue.
    with_cockpit(add("claude", "atlas-api-auth-refactor", "busy"), "claude-opus-5-5",
                 (40_000, 300_000, 4_000_000, 60_000, 180_000), 1_000_000, False, 3.2,
                 handoff=("20261015-101500-atlas-api-auth-refactor.md", 45.5))
    # 62 % da janela, releitura baixa e handoff velho: compactar.
    with_cockpit(add("claude", "ledger-migration-v4", "idle"), "claude-sonnet-5",
                 (80_000, 900_000, 9_000_000, 120_000, 620_000), 1_000_000, False, 12.0,
                 handoff=("20261014-180000-ledger-migration-v4.md", 300.0))
    # Releitura em 0,93 com o contexto acima do piso pesado pelo burn: handoff.
    with_cockpit(add("codex", "orbit-web-perf-pass", "blocked"), "gpt-6-codex",
                 (300_000, 0, 80_000_000, 60_000, 200_000), 258_400, False, 0.5)
    # Parada ha mais de 90 min; provider declarado no transcript do pi.
    with_cockpit(add("pi", "harbor-docs", "idle"), "deepseek-ai/DeepSeek-V4.1-Flash",
                 (25_000, 0, 400_000, 9_000, 30_000), 128_000, True, 132.5,
                 provider_declared="deepinfra")
    # A mesma chamada 4 vezes nas ultimas 30, no maximo 2 seguidas (C23).
    with_cockpit(add("claude", "quill-flaky-tests", "busy"), "claude-haiku-5",
                 (20_000, 90_000, 1_500_000, 30_000, 70_000), 200_000, False, 1.0, loops=(2, 4))
    # Transcript casado ainda em varredura: sem tokens, sem cockpit, sem provider.
    s = add("pi", "relay-i18n", "idle", pending=True)
    s["overlay"]["model"] = "pi-local-32b"
    s["overlay"]["provider"] = provider("pi", "pi-local-32b")
    s["overlay"]["context_pct"] = None
    # Agente sem transcript: sem dado (o provider sai do agente).
    s = add("gemini", "tundra-onboarding", "idle")
    s["overlay"]["model"] = None
    s["overlay"]["provider"] = provider("gemini", None)
    s["overlay"]["context_pct"] = None
    out.append(session(rng, "herdr", pid, None, HOME, None, role="multiplexer", children=6))
    return snapshot(out, mem_used=21.0, swap_used=8.0, psi_mem=3.2, psi_io=1.1)


def tipico_40():
    rng = random.Random(40)
    mix = ["claude"] * 31 + ["codex"] * 5 + ["pi"] * 2
    states = ["busy"] * 9 + ["blocked"] * 3 + ["idle"] * 26
    rng.shuffle(states)
    out, pid = [], 20000
    for i, agent in enumerate(mix):
        name = f"{rng.choice(PROJECTS)}-{rng.choice(TOPICS)}"
        cwd = f"{HOME}/src/{name.split('-')[0]}"
        # Uma em cada sete com o transcript ainda em varredura (fora do rng).
        out.append(session(rng, agent, pid, name, cwd, states[i], pending=i % 7 == 3))
        pid += 40
    for w in range(2):
        out.append(session(rng, "herdr", pid, None, HOME, None, role="multiplexer", children=rng.randint(8, 20)))
        pid += 80
    return snapshot(out)


def nomes_longos():
    rng = random.Random(7)
    cases = [
        ("claude", "migracao-do-servico-de-faturamento-para-o-novo-esquema-de-eventos-com-reprocessamento",
         f"{HOME}/src/plataforma/servicos/faturamento/eventos/reprocessamento/ferramentas/scripts", "busy"),
        ("codex", "研究-ベンチマーク-再現-長い名前のセッション-テスト",
         f"{HOME}/src/ベンチマーク/再現", "blocked"),
        ("claude", "ação-revisão-ortografia-açúcar-pão-coração-ç-ã-õ-é-ü",
         f"{HOME}/projetos/documentação/revisão", "idle"),
        ("pi", "x" * 120, f"{HOME}/" + "/".join(["muito-profundo"] * 12), "idle"),
        ("claude", "nome com espaços   e   tabulação", f"{HOME}/src/a b c/d e f", "busy"),
        ("codex", "bidi-‮txet-desrever-check", f"{HOME}/src/bidi‮/x", "idle"),
    ]
    out = []
    for i, (agent, name, cwd, st) in enumerate(cases):
        out.append(session(rng, agent, 30000 + i * 40, name, cwd, st))
    return snapshot(out, mem_used=12.0, swap_used=2.0, psi_mem=0.4, psi_io=0.2)


def vazio():
    return snapshot([], mem_used=6.1, swap_used=0.0, psi_mem=0.0, psi_io=0.0)


def uma():
    rng = random.Random(1)
    s = session(rng, "claude", 41000, "atlas-api-docs", f"{HOME}/src/atlas", "busy", children=3)
    # Sem delay accounting: o stall sai em threads D, e a janela mostra o banner.
    for p in s["processes"]:
        p["stall_pct"] = None
        p["threads_d"] = 0
    s["processes"][1]["threads_d"] = 2
    s["stall_pct"] = None
    s["threads_d"] = 2
    return snapshot([s], stall_source="d_state", mem_used=9.4, swap_used=0.5, psi_mem=0.0, psi_io=0.8)


def main():
    builds = [("tipico-40", tipico_40), ("nomes-longos", nomes_longos), ("vazio", vazio), ("uma", uma),
              ("cockpit", cockpit)]
    for name, build in builds:
        path = HERE / f"{name}.json"
        path.write_text(json.dumps(build(), indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
        print(path)


if __name__ == "__main__":
    main()
