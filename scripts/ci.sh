#!/usr/bin/env bash
# Fonte unica do CI: o workflow do GitHub so chama este script, e localmente
# `bash scripts/ci.sh` roda exatamente o mesmo portao.
#
# Passos: GTK4 + libadwaita no pkg-config (a GUI esta no workspace), teste do
# instalador (scripts/test-install.sh, mais shellcheck se houver), fmt,
# clippy (warnings sao erro), testes do workspace, build da GUI, bench do tick
# com portao de mediana (scripts/bench-gate.sh; variaveis BENCH_GATE_* la).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# rustup instala em ~/.cargo/bin, que shell nao interativo nem sempre tem no PATH.
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || { echo "ci: cargo not found (install Rust with rustup)" >&2; exit 2; }
[ -f Cargo.toml ] || { echo "ci: no Cargo.toml at $(pwd): the workspace does not exist yet" >&2; exit 2; }

step() { printf '\n==> %s\n' "$*"; }

# One EXIT handler owns both temporary resources; never stop a caller's display.
stamp="" broadway_pid="" broadway_log=""
cleanup() {
    if [ -n "$broadway_pid" ]; then
        kill "$broadway_pid" 2>/dev/null || true
        wait "$broadway_pid" 2>/dev/null || true
    fi
    [ -z "$broadway_log" ] || rm -f "$broadway_log"
    [ -z "$stamp" ] || rm -f "$stamp"
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

prepare_test_display() {
    # Respect explicit displays, including an externally managed Broadway server.
    if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}${BROADWAY_DISPLAY:-}" ]; then
        return 0
    fi
    command -v gtk4-broadwayd >/dev/null 2>&1 || {
        echo "ci: headless GTK tests require gtk4-broadwayd (libgtk-4-bin)" >&2
        return 2
    }
    broadway_log="$(mktemp)"
    export GDK_BACKEND=broadway BROADWAY_DISPLAY=":$$"
    # PID-unique GTK socket; HTTP binds loopback on a kernel-selected port.
    gtk4-broadwayd --address=127.0.0.1 --port=0 "$BROADWAY_DISPLAY" >"$broadway_log" 2>&1 &
    broadway_pid=$!
    for ((attempt=0; attempt<100; attempt++)); do
        if ! kill -0 "$broadway_pid" 2>/dev/null; then
            echo "ci: private Broadway backend failed to start" >&2
            return 2
        fi
        if grep -q '^Listening on ' "$broadway_log"; then
            return 0
        fi
        sleep 0.05
    done
    echo "ci: private Broadway backend did not become ready within 5s" >&2
    return 2
}

# Mesmas versoes das features do crates/agentcenter/Cargo.toml (v4_22, v1_9).
# Sem elas o clippy do workspace ja falharia, mas no meio do build do gtk4-sys.
step "pkg-config: gtk4 >= 4.22, libadwaita-1 >= 1.9"
command -v pkg-config >/dev/null 2>&1 || { echo "ci: pkg-config not found (apt install pkgconf)" >&2; exit 2; }
for dep in gtk4:4.22 libadwaita-1:1.9; do
    name="${dep%%:*}" min="${dep#*:}"
    if ! pkg-config --atleast-version="$min" "$name"; then
        have="$(pkg-config --modversion "$name" 2>/dev/null || echo missing)"
        echo "ci: $name >= $min required, found $have (apt install libgtk-4-dev libadwaita-1-dev)" >&2
        exit 2
    fi
    echo "$name $(pkg-config --modversion "$name")"
done

# Instalador (C25): sem build, sem rede e sem sudo, num prefixo descartavel
# dentro de target/. shellcheck so nos scripts do instalador, e so se houver.
step "install.sh: shellcheck + scripts/test-install.sh"
if command -v shellcheck >/dev/null 2>&1; then
    shellcheck install.sh scripts/test-install.sh
    echo "shellcheck: clean"
else
    echo "shellcheck: not found, skipped (apt install shellcheck)"
fi
bash scripts/test-install.sh

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "GTK test display (existing display or private local Broadway)"
prepare_test_display

step "cargo test --workspace"
cargo test --workspace

step "cargo build -p agentcenter"
cargo build -p agentcenter

# Padrao: as duas fixtures de 500 processos, tick_fixture_500_delayacct e
# tick_fixture_500_d_state (fallback sem delay accounting). Limite de 5 ms para as duas.
bench_id="${BENCH_GATE_ID:-tick_fixture_500}"
target_dir="$(cargo metadata --format-version 1 --no-deps \
    | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
mkdir -p "$target_dir"
# Marco antes do bench: o portao so aceita resultado gravado depois dele.
stamp="$(mktemp "$target_dir/.bench-gate-stamp.XXXXXX")"
# The EXIT handler above removes this stamp as well as our Broadway process.

step "cargo bench -p agentcenter-core --bench tick -- $bench_id"
cargo bench -p agentcenter-core --bench tick -- "$bench_id"

step "bench gate"
BENCH_GATE_ID="$bench_id" BENCH_GATE_SINCE="$stamp" CRITERION_DIR="$target_dir/criterion" \
    bash scripts/bench-gate.sh

step "ci: ok"
