#!/usr/bin/env bash
# Regenerate docs/CLI.md and man/agentcenter-cli.1 from the CLI's own command
# table (the hidden `gen-docs` subcommand). scripts/ci.sh runs this and then
# fails on a git diff, so commit the result. Offline: only a local cargo build.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || { echo "gen-docs: cargo not found" >&2; exit 2; }

mkdir -p docs man
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo build --quiet --locked -p agentcenter-cli
bin="$(cargo metadata --format-version 1 --no-deps \
    | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/debug/agentcenter-cli"
"$bin" gen-docs markdown >"$tmp/CLI.md"
"$bin" gen-docs man >"$tmp/agentcenter-cli.1"
mv "$tmp/CLI.md" docs/CLI.md
mv "$tmp/agentcenter-cli.1" man/agentcenter-cli.1
echo "gen-docs: wrote docs/CLI.md and man/agentcenter-cli.1"
