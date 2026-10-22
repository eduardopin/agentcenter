#!/usr/bin/env bash
# Native GUI -> extension -> native persistence; synthetic provider, private HOME.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
# --scenario (CC7): scripted synthetic sessions and pickup adapter, no Pi SDK;
# walks absent/present/dialog/six stages/failure/unknown for captures.
scenario=0
if [[ ${1:-} == --scenario ]]; then
    scenario=1
else
    : "${PI_SDK_DIR:?PI_SDK_DIR must point to the original Pi SDK}"
    : "${AGENTCENTER_GUARDED_PI_SDK:?AGENTCENTER_GUARDED_PI_SDK must point to the guarded Pi SDK}"
    export PI_SDK_DIR AGENTCENTER_GUARDED_PI_SDK
    export AGENTCENTER_PI_CONTROL_EXTENSION=${AGENTCENTER_PI_CONTROL_EXTENSION:-$repo/extensions/pi-control/index.ts}
    for file in "$PI_SDK_DIR/node_modules/@earendil-works/pi-ai/dist/index.js" "$AGENTCENTER_GUARDED_PI_SDK/dist/core/sdk.js" "$AGENTCENTER_PI_CONTROL_EXTENSION"; do
        [[ -f $file ]] || { echo "Missing required test input: $file" >&2; exit 2; }
    done
fi
runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be a valid private runtime directory}
[[ -d $runtime && -O $runtime && $(stat -c %a "$runtime") == 700 ]] || { echo 'XDG_RUNTIME_DIR must be owned by this user with mode 700' >&2; exit 2; }
backend=${CC3_BACKEND:-broadway}
case $backend in
    broadway) command -v gtk4-broadwayd >/dev/null ;;
    wayland)
        wayland=${WAYLAND_DISPLAY:?WAYLAND_DISPLAY is required for CC3_BACKEND=wayland}
        [[ $wayland == /* ]] || wayland="$runtime/$wayland"
        [[ -S $wayland ]] || { echo "Wayland socket unavailable: $wayland" >&2; exit 2; }
        ;;
    *) echo 'CC3_BACKEND must be broadway or wayland' >&2; exit 2 ;;
esac
(( scenario )) || command -v node >/dev/null
# Cargo/rustup need the original HOME; no compilation after isolation.
cargo build --offline -p agentcenter --example pi_controls_fixture -j4
export CC3_GUI="${CARGO_TARGET_DIR:-$repo/target}/debug/examples/pi_controls_fixture"
CC3_GUI=$(realpath "$CC3_GUI"); export CC3_GUI
root=$(mktemp -d "$runtime/cc3-gui-XXXXXX")
broadway_pid= driver_pid=''
cleanup() {
    trap '' INT TERM
    if [[ -n $driver_pid ]]; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    if [[ -n $broadway_pid ]]; then
        kill -TERM "$broadway_pid" 2>/dev/null || true
        wait "$broadway_pid" 2>/dev/null || true
    fi
    rm -rf -- "$root"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -m700 "$root/home" "$root/runtime" "$root/state" "$root/config"
export HOME="$root/home" XDG_RUNTIME_DIR="$root/runtime" XDG_STATE_HOME="$root/state" XDG_CONFIG_HOME="$root/config"
export GDK_BACKEND="$backend" GSETTINGS_BACKEND=memory
unset GTK_THEME
if [[ $backend == broadway ]]; then
    export BROADWAY_DISPLAY=":$$"
    # Private display socket, ephemeral TCP port, loopback only.
    gtk4-broadwayd --address=127.0.0.1 --port=0 "$BROADWAY_DISPLAY" >"$root/broadway.log" 2>&1 &
    broadway_pid=$!
    sleep .3
    kill -0 "$broadway_pid" || { read -r line <"$root/broadway.log"; echo "$line" >&2; exit 1; }
else
    # Only this new synthetic fixture uses the compositor; no existing window is controlled.
    export WAYLAND_DISPLAY="$wayland"
fi
for size in ${CC3_SIZES:-360x640}; do
    [[ $size =~ ^[0-9]+x[0-9]+$ ]] || { echo "Invalid size: $size" >&2; exit 2; }
    for theme in ${CC3_THEMES:-light dark}; do
        [[ $theme == light || $theme == dark ]] || { echo "Invalid theme: $theme" >&2; exit 2; }
        key="$size-$theme"
        mkdir -m700 "$root/home/$key" "$root/state/$key" "$root/config/$key"
        export HOME="$root/home/$key" XDG_STATE_HOME="$root/state/$key" XDG_CONFIG_HOME="$root/config/$key"
        export CC3_THEME="$theme" CC3_WIDTH=${size%x*} CC3_HEIGHT=${size#*x}
        echo "RUN $size $theme backend=$backend"
        if (( scenario )); then
            "$CC3_GUI" --scenario &
        else
            node crates/agentcenter/tests/pi-controls-gui.mjs &
        fi
        driver_pid=$!
        wait "$driver_pid"
        driver_pid=''
    done
done
