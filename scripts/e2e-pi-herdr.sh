#!/usr/bin/env bash
# E2E of the Pi handoff-pickup (CC5, protocol 3) with a REAL private Pi inside
# a REAL, isolated Herdr, driven by the Rust client (pi::handoff_pickup +
# pi::pickup_status) to `pickup_loaded`. Nothing of the owner's environment is
# used: a fresh HOME under ~/.cache/ac-e2e-pi.* (0700; /tmp is refused by the
# control socket checks), its own XDG_RUNTIME_DIR (where the extension
# publishes its endpoint), XDG_STATE_HOME (handoffs), HERDR_SOCKET_PATH/
# HERDR_CONFIG_PATH/HERDR_HOME, and a stub of an OpenAI-compatible API on
# 127.0.0.1 (scripts/e2e-pi-stub.py) as the only model provider, with a fake key.
#
# Arrangement:
#   - HOME/.pi/agent/models.json with the `stub` provider only;
#   - `herdr integration install pi` into that HOME (Herdr then reports the
#     Pi pane as agent_session {kind:"path", value:<session .jsonl>});
#   - a `pi` wrapper on PATH that runs the private guarded runtime
#     (AGENTCENTER_GUARDED_PI_SDK, default target/pi-guarded-runtime) offline,
#     on the stub model, with extensions/pi-control/index.ts of this checkout;
#   - a git repo as cwd; `herdr server` headless; one workspace whose root
#     pane runs `herdr agent start ... --kind pi`; one warm-up turn so the
#     session has a conversation to hand off.
# Then the #[ignore] Rust driver control_pi_pickup_e2e runs against it with the
# AC_E2E_* variables below; E2E_FILTER narrows it to one test name.
#
# Usage: bash scripts/e2e-pi-herdr.sh            # run the driver
#        E2E_KEEP=1 bash scripts/e2e-pi-herdr.sh # keep the root to inspect
#        E2E_SETUP_ONLY=1 ...                    # print env and wait
# Needs: herdr, node and jq on PATH, python3, cargo, the prepared private Pi
# runtime. No real API key, no network, never the owner's ~/.pi or Herdr.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
orig_home=$HOME orig_path=$PATH
runtime_src=${AGENTCENTER_GUARDED_PI_SDK:-$repo_root/target/pi-guarded-runtime}
herdr_bin=$(command -v herdr) || { echo "e2e: herdr not on PATH" >&2; exit 2; }
node_bin=$(command -v node) || { echo "e2e: node not on PATH" >&2; exit 2; }
jq_bin=$(command -v jq) || { echo "e2e: jq not on PATH" >&2; exit 2; }
[[ -f $runtime_src/run.mjs ]] || {
  echo "e2e: no private Pi runtime at $runtime_src (set AGENTCENTER_GUARDED_PI_SDK)" >&2; exit 2; }
runtime_src=$(cd "$runtime_src" && pwd)
# Nothing of the caller's environment leaks in, only an allowlist: a parent's
# HERDR_SOCKET_PATH would point at the owner's server, a parent's PI_* or
# XDG_* at the owner's Pi profile, and tokens, keys or agent sockets must not
# reach herdr, pi or the stub.
for v in $(compgen -e); do
  case $v in
    HOME|PATH|USER|LOGNAME|SHELL|TERM|LANG|LANGUAGE|LC_*|TMPDIR) ;;
    CARGO_TARGET_DIR|CARGO_HOME|RUSTUP_HOME|E2E_*) ;;
    *) unset "$v" ;;
  esac
done

umask 077
mkdir -p "$HOME/.cache"
root=$(mktemp -d "$HOME/.cache/ac-e2e-pi.XXXXXX")
chmod 700 "$root"
home=$root/home
repo=$root/repo
hdir=$root/herdr
run=$root/run
state=$root/state
bin=$root/bin
mkdir -p "$home/.pi/agent" "$repo" "$hdir" "$run" "$state" "$bin"
chmod 700 "$home" "$hdir" "$run" "$state"
stub_pid= server_pid=

cleanup() {
  set +e
  if [[ -n $server_pid ]]; then
    HOME=$home HERDR_SOCKET_PATH=$hdir/herdr.sock HERDR_CONFIG_PATH=$hdir/config.toml \
      HERDR_HOME=$hdir "$herdr_bin" server stop >/dev/null 2>&1
    for _ in $(seq 50); do kill -0 "$server_pid" 2>/dev/null || break; sleep 0.1; done
    kill "$server_pid" 2>/dev/null
  fi
  # Whatever the server left behind (pane shells, pi): every process of this
  # user whose environment names this run's Herdr socket.
  for e in /proc/[0-9]*/environ; do
    [[ -O $e ]] || continue
    if { tr '\0' '\n' <"$e"; } 2>/dev/null | grep -qxF "HERDR_SOCKET_PATH=$hdir/herdr.sock"; then
      p=${e#/proc/}; kill "${p%/environ}" 2>/dev/null
    fi
  done
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

python3 "$repo_root/scripts/e2e-pi-stub.py" "$root/stub.port" "$root/stub.log" &
stub_pid=$!
for _ in $(seq 50); do [[ -s $root/stub.port ]] && break; sleep 0.1; done
[[ -s $root/stub.port ]] || { echo "e2e: stub did not start" >&2; exit 1; }
port=$(cat "$root/stub.port")
cat >"$home/.pi/agent/models.json" <<JSON
{"providers":{"stub":{"baseUrl":"http://127.0.0.1:$port/v1","api":"openai-completions","apiKey":"stub-not-a-key",
 "compat":{"supportsDeveloperRole":false,"supportsReasoningEffort":false,"supportsUsageInStreaming":true},
 "models":[{"id":"fake","name":"fake","reasoning":false,"input":["text"],"contextWindow":65536,"maxTokens":2048,
  "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]}}}
JSON
cat >"$bin/pi" <<SH
#!/bin/sh
exec "$node_bin" "$runtime_src/run.mjs" --offline --provider stub --model fake --no-skills \
  --no-prompt-templates --no-themes --no-context-files --no-approve \
  -e "$repo_root/extensions/pi-control/index.ts" "\$@"
SH
chmod 700 "$bin/pi"

# Isolated environment for everything started from here (herdr, its shells,
# pi, the herdr integration). PATH keeps only system dirs and the tools' dirs.
export HOME=$home
export PATH="$bin:$(dirname "$node_bin"):$(dirname "$herdr_bin"):$(dirname "$jq_bin"):/usr/local/bin:/usr/bin:/bin"
export HERDR_SOCKET_PATH=$hdir/herdr.sock HERDR_CONFIG_PATH=$hdir/config.toml HERDR_HOME=$hdir
export XDG_RUNTIME_DIR=$run XDG_STATE_HOME=$state PI_OFFLINE=1
unset XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME

"$herdr_bin" integration install pi >/dev/null
git -C "$repo" init -q

setsid "$herdr_bin" server >"$root/herdr.log" 2>&1 </dev/null &
server_pid=$!
for _ in $(seq 100); do [[ -S $HERDR_SOCKET_PATH ]] && break; sleep 0.1; done
[[ -S $HERDR_SOCKET_PATH ]] || { echo "e2e: herdr socket missing" >&2; cat "$root/herdr.log" >&2; exit 1; }

ws=$("$herdr_bin" workspace create --cwd "$repo" --label e2e)
pane=$(jq -r '.result.root_pane.pane_id' <<<"$ws")
"$herdr_bin" agent start e2e-pi --kind pi --pane "$pane" >"$root/agent-start.json" 2>&1 \
  || { echo "e2e: agent start failed" >&2; cat "$root/agent-start.json" >&2; \
       "$herdr_bin" pane read "$pane" >&2 || true; exit 1; }

# Warm-up: Pi writes the session file from the first turn, and a handoff needs
# a conversation. Herdr may report the pane before the TUI takes input, so a
# prompt typed during start-up can be dropped: resend until the stub saw a
# turn (at most 5 tries).
turned() { [[ -s $root/stub.log ]]; }
for _ in $(seq 5); do
  "$herdr_bin" agent prompt "$pane" "e2e warm-up" >"$root/warmup.json" 2>&1 || true
  for _ in $(seq 25); do turned && break; sleep 0.2; done
  turned && break
done
turned || { echo "e2e: warm-up never reached the stub" >&2; "$herdr_bin" pane read "$pane" >&2 || true
            [[ ${E2E_SETUP_ONLY:-0} == 1 ]] || exit 1; }
"$herdr_bin" agent wait "$pane" --until idle --timeout 30000 >/dev/null 2>&1 || true
agent=$("$herdr_bin" agent get "$pane")

export AC_E2E_ROOT=$root AC_E2E_HOME=$home AC_E2E_REPO=$repo
export AC_E2E_RUNTIME=$run AC_E2E_STUB_LOG=$root/stub.log
export AC_E2E_SOCKET=$HERDR_SOCKET_PATH AC_E2E_SERVER_PID=$server_pid
export AC_E2E_PANE=$pane
export AC_E2E_WORKSPACE=$(jq -r '.result.agent.workspace_id' <<<"$agent")
export AC_E2E_TAB=$(jq -r '.result.agent.tab_id' <<<"$agent")
env | grep '^AC_E2E_' | sort >&2

if [[ ${E2E_SETUP_ONLY:-0} == 1 ]]; then
  echo "e2e: setup only; Ctrl-C to tear down" >&2
  sleep infinity & wait $!
fi
cd "$repo_root"
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$orig_home/.cache/agentcenter-target/e2e} HOME=$orig_home \
  PATH=$orig_path cargo test -j4 -p agentcenter-core --test control_pi_pickup_e2e -- --ignored \
  --nocapture --test-threads=1 ${E2E_FILTER:-}
