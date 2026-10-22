#!/usr/bin/env bash
# E2E of the Claude Code controls with a REAL Claude Code inside a REAL, isolated
# Herdr (CC6 handoff-pickup; reusable by the CC4 E2E). Nothing of the owner's
# environment is used: a fresh HOME under ~/.cache (0700; /tmp is refused by
# the control socket checks), its own HERDR_SOCKET_PATH/HERDR_CONFIG_PATH/
# HERDR_HOME, and a stub of the Anthropic API on 127.0.0.1
# (scripts/e2e-claude-stub.py) behind ANTHROPIC_BASE_URL with a fake key.
#
# Arrangement:
#   - HOME/.claude.json with onboarding, the fake key and the repo trust
#     accepted; HOME/.claude/settings.json allowing Read/Write/Skill;
#   - `herdr integration install claude` into that HOME (SessionStart hook,
#     so Herdr knows the agent_session_id, also after /clear);
#   - the clear-pickup skill copied from CLEAR_PICKUP_SKILL (default
#     ~/.claude/skills/clear-pickup, read only) into HOME/.claude/skills;
#   - a git repo as cwd; `herdr server` headless; one workspace whose root
#     pane runs `herdr agent start ... --kind claude`.
# Then the #[ignore] Rust drivers of the test target E2E_TEST (default
# control_claude_pickup_e2e: CC4 Handoff and Compact, CC6 pickup), one at a
# time on the same pane, run against it with the AC_E2E_* variables below;
# E2E_FILTER narrows them to one test name.
#
# Usage: bash scripts/e2e-claude-herdr.sh            # run the driver
#        E2E_KEEP=1 bash scripts/e2e-claude-herdr.sh # keep the root to inspect
#        E2E_SETUP_ONLY=1 ...                        # print env and wait
# Needs: claude and herdr on PATH, python3, cargo. No real API key, no network.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
orig_home=$HOME orig_path=$PATH
skill_src=${CLEAR_PICKUP_SKILL:-$HOME/.claude/skills/clear-pickup}
test_name=${E2E_TEST:-control_claude_pickup_e2e}
claude_bin=$(command -v claude) || { echo "e2e: claude not on PATH" >&2; exit 2; }
herdr_bin=$(command -v herdr) || { echo "e2e: herdr not on PATH" >&2; exit 2; }
[[ -f $skill_src/SKILL.md ]] || { echo "e2e: no skill at $skill_src" >&2; exit 2; }
# Nothing of the caller's environment leaks in, only an allowlist: a parent's
# CLAUDE_CODE_CHILD_SESSION turns transcripts off, a parent's HERDR_SOCKET_PATH
# would point at the owner's server, and tokens, keys or agent sockets must not
# reach herdr, claude or the stub.
for v in $(compgen -e); do
  case $v in
    HOME|PATH|USER|LOGNAME|SHELL|TERM|LANG|LANGUAGE|LC_*|TMPDIR|XDG_RUNTIME_DIR) ;;
    CARGO_TARGET_DIR|CARGO_HOME|RUSTUP_HOME|E2E_*|CLEAR_PICKUP_SKILL) ;;
    *) unset "$v" ;;
  esac
done

mkdir -p "$HOME/.cache"
root=$(mktemp -d "$HOME/.cache/ac-e2e.XXXXXX")
chmod 700 "$root"
home=$root/home
repo=$root/repo
hdir=$root/herdr
mkdir -p "$home/.claude/skills" "$repo/.claude/handoffs" "$hdir"
chmod 700 "$hdir"
stub_pid= server_pid=

cleanup() {
  set +e
  if [[ -n $server_pid ]]; then
    HOME=$home HERDR_SOCKET_PATH=$hdir/herdr.sock HERDR_CONFIG_PATH=$hdir/config.toml \
      HERDR_HOME=$hdir "$herdr_bin" server stop >/dev/null 2>&1
    for _ in $(seq 50); do kill -0 "$server_pid" 2>/dev/null || break; sleep 0.1; done
    kill "$server_pid" 2>/dev/null
  fi
  [[ -n $stub_pid ]] && kill "$stub_pid" 2>/dev/null
  if [[ ${E2E_KEEP:-0} == 1 ]]; then
    echo "e2e: kept $root" >&2
  else
    chmod -R u+rwX "$root" 2>/dev/null
    rm -rf "$root"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Isolated environment for everything started from here (herdr, its shells,
# claude, hooks). PATH keeps only system dirs and the two binaries' dirs.
key=sk-ant-e2e-stub-not-a-real-key-000000000000
export HOME=$home
export PATH="$(dirname "$claude_bin"):$(dirname "$herdr_bin"):/usr/local/bin:/usr/bin:/bin"
export HERDR_SOCKET_PATH=$hdir/herdr.sock HERDR_CONFIG_PATH=$hdir/config.toml HERDR_HOME=$hdir
export ANTHROPIC_API_KEY=$key
export DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
# Claude Code walks up from the cwd for CLAUDE.md, and the root lives under the
# real HOME: without this the owner's ~/.claude/CLAUDE.md would be read.
export CLAUDE_CODE_DISABLE_CLAUDE_MDS=1
export STUB_HANDOFF_DIR=$repo/.claude/handoffs
export STUB_GRANT_PATH=$repo/.claude/e2e-grant.md
unset XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME XDG_CACHE_HOME

git -C "$repo" init -q
cp -r "$skill_src" "$home/.claude/skills/clear-pickup"
version=$("$claude_bin" --version | awk '{print $1}')
python3 - "$home" "$repo" "$key" "$version" <<'PY'
import json, sys
home, repo, key, version = sys.argv[1:]
json.dump({
    "hasCompletedOnboarding": True,
    "lastOnboardingVersion": version,
    "theme": "dark",
    "autoUpdates": False,
    "customApiKeyResponses": {"approved": [key[-20:]], "rejected": []},
    "projects": {repo: {"hasTrustDialogAccepted": True,
                        "hasCompletedProjectOnboarding": True, "allowedTools": []}},
}, open(f"{home}/.claude.json", "w"))
# Auto mode off: with a custom ANTHROPIC_BASE_URL it opens a billing notice
# over the first tool call, and Herdr still reports the pane idle.
json.dump({"permissions": {"allow": ["Read", "Write", "Skill"],
                          "defaultMode": "acceptEdits", "disableAutoMode": "disable"}},
          open(f"{home}/.claude/settings.json", "w"))
PY
"$herdr_bin" integration install claude >/dev/null

python3 "$repo_root/scripts/e2e-claude-stub.py" "$root/stub.port" "$root/stub.log" &
stub_pid=$!
for _ in $(seq 50); do [[ -s $root/stub.port ]] && break; sleep 0.1; done
export ANTHROPIC_BASE_URL=http://127.0.0.1:$(cat "$root/stub.port")

setsid "$herdr_bin" server >"$root/herdr.log" 2>&1 </dev/null &
server_pid=$!
for _ in $(seq 100); do [[ -S $HERDR_SOCKET_PATH ]] && break; sleep 0.1; done
[[ -S $HERDR_SOCKET_PATH ]] || { echo "e2e: herdr socket missing" >&2; cat "$root/herdr.log" >&2; exit 1; }

ws=$("$herdr_bin" workspace create --cwd "$repo" --label e2e)
pane=$(jq -r '.result.root_pane.pane_id' <<<"$ws")
"$herdr_bin" agent start e2e-claude --kind claude --pane "$pane" -- --permission-mode acceptEdits \
  >"$root/agent-start.json" 2>&1 \
  || { echo "e2e: agent start failed" >&2; cat "$root/agent-start.json" >&2; \
       "$herdr_bin" pane read "$pane" >&2 || true; exit 1; }
# Warm-up, which also grants the session the project's .claude/ folder.
# Claude Code writes the transcript only from the first turn. Herdr reports
# interactive_ready before the TUI takes input, so a prompt typed during the
# start-up is dropped: resend until a user entry shows up (at most 5 tries).
# The stub answers "e2e warm-up" with a Write of $STUB_GRANT_PATH under the
# project's .claude/, a protected path that asks even under acceptEdits and
# that no allow rule (settings or --allowedTools) unlocks in 2.1.x. Like an
# owner whose session writes handoffs, option 2 ("allow Claude to edit files
# in this project's .claude folder for this session") is chosen once, here,
# before any operation; the controller then runs without any help.
asked() { grep -qs '"type":"user"' "$home"/.claude/projects/*/*.jsonl; }
for _ in $(seq 5); do
  "$herdr_bin" agent prompt "$pane" "e2e warm-up" >"$root/warmup.json" 2>&1 \
    || { echo "e2e: warm-up prompt failed" >&2; cat "$root/warmup.json" >&2; exit 1; }
  for _ in $(seq 20); do asked && break; sleep 0.2; done
  asked && break
done
dialog="2. Yes, and allow Claude to edit files in this project's .claude folder"
for _ in $(seq 100); do
  "$herdr_bin" pane read "$pane" 2>/dev/null | grep -qF "$dialog" && break
  sleep 0.2
done
if "$herdr_bin" pane read "$pane" 2>/dev/null | grep -qF "$dialog"; then
  "$herdr_bin" agent send-keys "$pane" 2 >/dev/null
fi
for _ in $(seq 100); do [[ -f $STUB_GRANT_PATH ]] && break; sleep 0.2; done
if [[ ! -f $STUB_GRANT_PATH ]]; then
  echo "e2e: warm-up did not write $STUB_GRANT_PATH" >&2
  "$herdr_bin" pane read "$pane" >&2 || true
  [[ ${E2E_SETUP_ONLY:-0} == 1 ]] || exit 1
fi
"$herdr_bin" agent wait "$pane" --until idle --timeout 30000 >/dev/null 2>&1 || true
agent=$("$herdr_bin" agent get "$pane")
proc=$("$herdr_bin" pane process-info --pane "$pane")

export AC_E2E_ROOT=$root AC_E2E_HOME=$home AC_E2E_REPO=$repo
export AC_E2E_SOCKET=$HERDR_SOCKET_PATH AC_E2E_SERVER_PID=$server_pid
export AC_E2E_PANE=$pane
export AC_E2E_WORKSPACE=$(jq -r '.result.agent.workspace_id' <<<"$agent")
export AC_E2E_TAB=$(jq -r '.result.agent.tab_id' <<<"$agent")
export AC_E2E_SESSION=$(jq -r '.result.agent.agent_session.value // empty' <<<"$agent")
export AC_E2E_PID=$(jq -r '[.result.process_info.foreground_processes[] | select(.name=="claude") | .pid][0] // empty' <<<"$proc")
env | grep '^AC_E2E_' | sort >&2

if [[ ${E2E_SETUP_ONLY:-0} == 1 ]]; then
  echo "e2e: setup only; Ctrl-C to tear down" >&2
  sleep infinity & wait $!
fi
cd "$repo_root"
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$orig_home/.cache/agentcenter-target/e2e} HOME=$orig_home \
  PATH=$orig_path cargo test -j4 -p agentcenter-core --test "$test_name" -- --ignored --nocapture \
  --test-threads=1 ${E2E_FILTER:-}
