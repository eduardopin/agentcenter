#!/usr/bin/env python3
"""Regenerates the parity fixtures from the Python reference.

    generate.py SNAP_REAL_JSON INSIGHTS_PY_DIR

Writes NN-name.snapshot.json and NN-name.expected.json beside this script. The base snapshot is
sanitized first (synthetic names, paths and ids; processes dropped) and perturbed with a fixed
seed, in the manner of scenarios() in bakeoff.py.
"""
import copy
import json
import os
import random
import sys

sys.path.insert(0, sys.argv[2])
import insights  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))


def sanitize(snap):
    for i, s in enumerate(snap["sessions"]):
        s["cwd"] = f"/home/dev/proj-{i:02d}"
        s["cmdline"] = s["agent"]
        s["processes"] = []
        ov = s.get("overlay")
        if ov:
            ov["name"] = f"session-{i:02d}"
            ov["agent_session_id"] = f"sid-{i:02d}"
            if ov.get("herdr"):
                ov["herdr"]["title"] = f"session-{i:02d}"
            if ov.get("cockpit") and ov["cockpit"].get("handoff_name"):
                ov["cockpit"]["handoff_name"] = f"handoff-{i:02d}.md"
    return snap


def perturb(base, seed, kinds, calm=False):
    rng = random.Random(seed)
    s = copy.deepcopy(base)
    agents = [x for x in s["sessions"] if x.get("role") == "agent" and (x.get("overlay") or {}).get("cockpit")]
    keep = rng.sample(agents, rng.randint(3, len(agents)))
    s["sessions"] = keep
    for x in keep:
        x["pss_bytes"] = int(x["pss_bytes"] * rng.uniform(0.5, 6))
        x["overlay"]["cockpit"]["idle_min"] = rng.choice([0.2, 5, 45, 180, 1500])
        x["overlay"]["status"] = "idle" if x["overlay"]["cockpit"]["idle_min"] >= 5 else "busy"
    m = s["machine"]
    for kind in kinds:
        if kind == "loop":
            for x in rng.sample(keep, min(len(keep), rng.randint(1, 3))):
                x["overlay"]["cockpit"]["max_streak"] = rng.randint(5, 40)
        if kind == "memory":
            m["mem_available_bytes"] = int(m["mem_total_bytes"] * rng.uniform(0.02, 0.14))
            m["swap_free_bytes"] = int(m["swap_total_bytes"] * rng.uniform(0.1, 0.9))
        if kind == "context":
            for x in rng.sample(keep, min(len(keep), rng.randint(1, 4))):
                x["overlay"]["context_pct"] = rng.uniform(75, 99)
        if kind == "cpu":
            m["load_avg"]["one"] = rng.uniform(13, 40)
        if kind == "burn":
            w = rng.choice(keep)
            w["overlay"]["cockpit"]["burn_weighted"] = sum(x["overlay"]["cockpit"].get("burn_weighted") or 0 for x in keep) * 3
            w["overlay"]["cockpit"]["cache_hit"] = rng.uniform(0.3, 0.79)
        if kind == "cache":
            w = rng.choice(keep)
            w["overlay"]["tokens_in"] = 5_000_000
            w["overlay"]["cockpit"]["cache_hit"] = rng.uniform(0.3, 0.79)
        if kind == "calm":
            for x in keep:
                x["pss_bytes"] = int(rng.uniform(5e7, 9e7))
                x["overlay"]["cockpit"]["idle_min"] = 1
                x["overlay"]["status"] = "busy"
                x["overlay"]["cockpit"]["burn_weighted"] = 1000
                x["overlay"]["cockpit"]["max_streak"] = 1
                x["overlay"]["context_pct"] = 20.0
                x["overlay"]["tokens_in"] = 100
                x["overlay"]["cockpit"]["cache_hit"] = 0.97
            m["mem_available_bytes"] = int(m["mem_total_bytes"] * 0.5)
            m["load_avg"]["one"] = 1.0
    return s


SCENARIOS = [
    ("loop", ["loop"]),
    ("memory-pressure", ["memory"]),
    ("context-full", ["context"]),
    ("cpu-saturated", ["cpu"]),
    ("burn-leader", ["burn"]),
    ("cache-cold", ["cache"]),
    ("all-clear", ["calm"]),
    ("loop-memory-context", ["loop", "memory", "context"]),
    ("everything-capped", ["loop", "memory", "context", "cpu", "burn", "cache"]),
    ("cpu-burn", ["cpu", "burn"]),
    ("idle-memory", []),
    ("memory-cpu-cache", ["memory", "cpu", "cache"]),
]


def main():
    base = sanitize(json.load(open(sys.argv[1])))
    seen = set()
    for i, (name, kinds) in enumerate(SCENARIOS):
        snap = perturb(base, 20261110 + i, kinds)
        fs = insights.findings(snap)
        seen |= {f["id"] for f in fs}
        stem = os.path.join(HERE, f"{i + 1:02d}-{name}")
        json.dump(snap, open(stem + ".snapshot.json", "w"), indent=1)
        json.dump({"findings": fs, "facts": insights.facts(fs), "fallback": insights.fallback(fs)},
                  open(stem + ".expected.json", "w"), indent=1)
        print(f"{i + 1:02d}-{name}: {[f['id'] for f in fs]}")
    print("rule ids covered:", sorted(seen))


main()
