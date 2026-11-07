#!/usr/bin/env bash
# Checks the repository hygiene files exist and parse: CHANGELOG, SECURITY,
# issue forms and dependabot. Needs python3 with PyYAML; no network.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

python3 -I - <<'PY'
import re, sys, pathlib
import yaml

fails = []
def fail(msg): fails.append(msg)
def load(path):
    p = pathlib.Path(path)
    if not p.is_file():
        fail(f"missing: {path}")
        return None
    try:
        return yaml.safe_load(p.read_text())
    except yaml.YAMLError as e:
        fail(f"{path}: invalid YAML: {e}")

def text(path):
    p = pathlib.Path(path)
    if not p.is_file():
        fail(f"missing: {path}")
        return ""
    return p.read_text()

cl = text("CHANGELOG.md")
if cl:
    if "keepachangelog.com" not in cl: fail("CHANGELOG.md: no Keep a Changelog reference")
    if not re.search(r"^## \[Unreleased\]\s*$", cl, re.M): fail("CHANGELOG.md: no '## [Unreleased]' heading")
    # Keep a Changelog allows any subset of its six change types per release.
    kinds = {"Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"}
    unrel = re.search(r"^## \[Unreleased\]\s*$(.*?)(?=^## |\Z)", cl, re.M | re.S)
    heads = re.findall(r"^### (.+?)\s*$", unrel.group(1), re.M) if unrel else []
    if unrel and not heads: fail("CHANGELOG.md: [Unreleased] has no '### <type>' section")
    for h in heads:
        if h not in kinds: fail(f"CHANGELOG.md: '### {h}' is not a Keep a Changelog type")

sec = text("SECURITY.md")
if sec:
    if not re.search(r"private vulnerability reporting|Report a vulnerability", sec, re.I):
        fail("SECURITY.md: no private reporting route")
    if re.search(r"[\w.+-]+@[\w-]+\.[\w.-]+", sec): fail("SECURITY.md: contains an email address")

for name in ("bug", "feature"):
    f = load(f".github/ISSUE_TEMPLATE/{name}.yml")
    if f is not None:
        if not isinstance(f, dict) or not f.get("name") or not f.get("description"):
            fail(f"{name}.yml: needs name and description")
        body = f.get("body") if isinstance(f, dict) else None
        if not body or not any(b.get("type") == "textarea" for b in body):
            fail(f"{name}.yml: needs a body with a textarea")
        for b in body or []:
            if b.get("type") != "markdown" and not b.get("attributes", {}).get("label"):
                fail(f"{name}.yml: every input needs a label")

cfg = load(".github/ISSUE_TEMPLATE/config.yml")
if cfg is not None and not isinstance(cfg.get("blank_issues_enabled"), bool):
    fail("config.yml: blank_issues_enabled must be set")

dep = load(".github/dependabot.yml")
if dep is not None:
    ups = {u.get("package-ecosystem"): u for u in dep.get("updates", [])}
    if dep.get("version") != 2: fail("dependabot.yml: version must be 2")
    for eco in ("cargo", "github-actions"):
        u = ups.get(eco)
        if not u: fail(f"dependabot.yml: no {eco} ecosystem")
        elif u.get("schedule", {}).get("interval") != "weekly": fail(f"dependabot.yml: {eco} is not weekly")

if fails:
    print("FAIL")
    for m in fails: print(" -", m)
    sys.exit(1)
print("ok: hygiene files present and valid")
PY
