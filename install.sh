#!/usr/bin/env bash
# Instalador de um comando (Linux): compila a GUI e a CLI em release e instala
# no prefixo do usuario, com atalho no menu, icone e schema do GSettings.
#
#   bash install.sh                      # ~/.local: bin/, share/applications, ...
#   bash install.sh --prefix DIR         # outro prefixo (relativo vira absoluto)
#   bash install.sh --deps               # libs de build faltando: sudo apt-get install
#   bash install.sh --no-build           # usa o que ja esta em target/release
#   bash install.sh --uninstall          # remove exatamente o que instalou
#
# Idempotente: rodar de novo sobrescreve os mesmos arquivos e o manifesto sai
# igual. O manifesto ($PREFIX/share/agentcenter/installed.txt) lista cada
# arquivo instalado (`file <caminho>`) e cada diretorio que o instalador criou
# (`dir <caminho>`); o --uninstall remove os arquivos, refaz ou apaga os caches
# que ficaram sem fonte e remove os diretorios criados que ficaram vazios.
# Nada fora do prefixo e tocado, a nao ser o `sudo apt-get install` do --deps.
#
# Sem --no-build: cargo (rust-version do Cargo.toml), um linker C, pkg-config,
# gtk4 e libadwaita-1 nas versoes das features do crates/agentcenter. Faltando
# lib, imprime a linha apt exata e sai 2 (com --deps, roda a linha).
# CARGO_TARGET_DIR troca de onde os binarios saem (padrao: target/ do repo).
set -euo pipefail

APP_ID=io.github.eduardopin.AgentCenter
BINS=(agentcenter agentcenter-cli)
# Mesmas versoes das features do crates/agentcenter/Cargo.toml (v4_22, v1_9)
# e do passo pkg-config do scripts/ci.sh.
PKG_DEPS=(gtk4:4.22 libadwaita-1:1.9)
APT_PKGS=(build-essential pkgconf libgtk-4-dev libadwaita-1-dev)

usage() {
    cat <<'USAGE'
Usage: bash install.sh [--prefix DIR] [--deps] [--no-build] [--uninstall]

Builds AgentCenter (GUI and CLI) in release mode and installs it with a menu
entry, icon and settings schema. Run it again to update; it is idempotent.

  --prefix DIR   install under DIR instead of ~/.local
  --deps         install missing build libraries with sudo apt-get
  --no-build     install the binaries already in target/release
  --uninstall    remove exactly what was installed (listed in
                 DIR/share/agentcenter/installed.txt)
  -h, --help     show this help
USAGE
}

die() { echo "install: $*" >&2; exit 2; }
say() { printf '%s\n' "$*"; }

prefix="$HOME/.local"
deps=0
build=1
uninstall=0
while (($#)); do
    case $1 in
        --prefix) (($# >= 2)) || die "--prefix needs a directory"; prefix=$2; shift ;;
        --prefix=*) prefix=${1#--prefix=} ;;
        --deps) deps=1 ;;
        --no-build) build=0 ;;
        --uninstall) uninstall=1 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

[[ -n $prefix ]] || die "--prefix is empty"
# Relativo ao diretorio de onde o comando foi chamado, antes do cd para o repo.
prefix="$(realpath -m -- "$prefix")"
[[ $prefix != / ]] || die "refusing to install into /"
# O Exec do .desktop leva o caminho absoluto; estes caracteres pedem escape
# no formato do .desktop e nao valem o risco num caminho de instalacao.
case $prefix in
    *[\"\`\$\\%]* | *$'\n'*) die "prefix has a character the .desktop Exec cannot hold: $prefix" ;;
esac

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo"

bindir="$prefix/bin"
appsdir="$prefix/share/applications"
icondir="$prefix/share/icons/hicolor"
schemadir="$prefix/share/glib-2.0/schemas"
statedir="$prefix/share/agentcenter"
manifest="$statedir/installed.txt"

# Lista fechada: o manifesto nao concede autoridade para apagar outros arquivos.
managed_files=("$bindir/agentcenter" "$bindir/agentcenter-cli"
    "$appsdir/$APP_ID.desktop" "$icondir/scalable/apps/$APP_ID.svg"
    "$schemadir/$APP_ID.gschema.xml")
managed_dirs=("$bindir" "$prefix/share" "$appsdir" "$prefix/share/icons"
    "$icondir" "$icondir/scalable" "$icondir/scalable/apps"
    "$prefix/share/glib-2.0" "$schemadir" "$statedir")
cache_files=("$appsdir/mimeinfo.cache" "$icondir/icon-theme.cache"
    "$schemadir/gschemas.compiled")

contains() {
    local wanted=$1 entry
    shift
    for entry in "$@"; do
        [[ $entry != "$wanted" ]] || return 0
    done
    return 1
}

# Recusa diretorios symlink (mesmo internos) para nao confundir propriedade
# por aliases. O prefixo ja foi canonicalizado. Isto nao protege de uma troca
# concorrente maliciosa entre a checagem e a operacao de filesystem.
check_dir() {
    local resolved
    resolved=$(realpath -m -- "$1") || die "cannot resolve directory: $1"
    [[ $resolved == "$1" ]] || die "redirected directory: $1"
    [[ ! -e $1 || -d $1 ]] || die "not a directory: $1"
}

check_file() {
    check_dir "$(dirname "$1")"
    # Um symlink final pode ser removido/substituido sem seguir o destino.
    [[ -L $1 || ! -e $1 || -f $1 ]] || die "not a regular file: $1"
}

# Preflight integral antes de gravar/apagar, inclusive os outputs dos caches
# compartilhados, que nao aparecem no manifesto. Ferramentas de cache nao
# recebem um output symlink (poderiam seguir o destino).
for d in "${managed_dirs[@]}"; do check_dir "$d"; done
for f in "${managed_files[@]}"; do check_file "$f"; done
for f in "$manifest" "${cache_files[@]}"; do
    check_file "$f"
    [[ ! -L $f ]] || die "refusing symlink for state/cache: $f"
done

files=() dirs=()
if [[ -f $manifest ]]; then
    while IFS= read -r line || [[ -n $line ]]; do
        [[ -n $line ]] || continue
        kind=${line%% *} path=${line#* }
        case $kind in
            file)
                contains "$path" "${managed_files[@]}" || die "unmanaged manifest file: $line"
                check_file "$path"
                files+=("$path")
                ;;
            dir)
                contains "$path" "${managed_dirs[@]}" || die "unmanaged manifest directory: $line"
                check_dir "$path"
                dirs+=("$path")
                ;;
            *) die "bad manifest line: $line" ;;
        esac
    done <"$manifest"
fi

# --- caches -----------------------------------------------------------------

# Cada cache e refeito quando a ferramenta existe. Sem fonte nenhuma no
# diretorio (o --uninstall tirou a ultima), o cache e apagado em vez de refeito.
refresh_desktop_db() {
    [[ -d $appsdir ]] || return 0
    if ! find "$appsdir" -name '*.desktop' -print -quit | grep -q .; then
        rm -f "$appsdir/mimeinfo.cache"
    elif command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$appsdir" || say "install: warning: update-desktop-database failed" >&2
    fi
}

refresh_icon_cache() {
    [[ -d $icondir ]] || return 0
    if ! find "$icondir" -type f ! -name icon-theme.cache -print -quit | grep -q .; then
        rm -f "$icondir/icon-theme.cache"
        return 0
    fi
    local tool
    for tool in gtk-update-icon-cache gtk4-update-icon-cache; do
        if command -v "$tool" >/dev/null 2>&1; then
            # -t: o hicolor do usuario costuma nao ter index.theme.
            "$tool" -q -t -f "$icondir" || say "install: warning: $tool failed" >&2
            return 0
        fi
    done
}

refresh_schemas() {
    [[ -d $schemadir ]] || return 0
    if ! find "$schemadir" -maxdepth 1 \( -name '*.gschema.xml' -o -name '*.gschema.override' \) \
        -print -quit | grep -q .; then
        rm -f "$schemadir/gschemas.compiled"
    elif command -v glib-compile-schemas >/dev/null 2>&1; then
        glib-compile-schemas "$schemadir" || say "install: warning: glib-compile-schemas failed in $schemadir" >&2
    else
        say "install: warning: glib-compile-schemas not found; preferences will not be saved" >&2
    fi
}

# --- uninstall ----------------------------------------------------------------

if ((uninstall)); then
    if [[ ! -f $manifest ]]; then
        say "AgentCenter is not installed in $prefix (no $manifest)."
        exit 0
    fi
    for f in "${files[@]}"; do
        rm -f -- "$f"
    done
    rm -f -- "$manifest"
    refresh_desktop_db
    refresh_icon_cache
    refresh_schemas
    # Mais fundo primeiro; rmdir so apaga vazio, entao o que outro programa
    # pos num diretorio criado aqui fica.
    if ((${#dirs[@]})); then
        while IFS= read -r d; do
            rmdir -- "$d" 2>/dev/null || true
        done < <(printf '%s\n' "${dirs[@]}" | awk '{ print gsub("/", "/"), $0 }' | sort -rn | cut -d' ' -f2-)
    fi
    say "AgentCenter removed from $prefix."
    exit 0
fi

# --- dependencias -------------------------------------------------------------

version_ge() { [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]; }

check_build_deps() {
    if ! command -v cargo >/dev/null 2>&1 && [[ -x $HOME/.cargo/bin/cargo ]]; then
        PATH="$HOME/.cargo/bin:$PATH"
    fi
    local need_rust
    need_rust="$(sed -n 's/^rust-version *= *"\([0-9.]*\)".*/\1/p' Cargo.toml | head -n1)"
    if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
        die "cargo not found. Install Rust >= ${need_rust:-stable} with rustup (https://rustup.rs), then run this again."
    fi
    local have_rust
    have_rust="$(rustc --version | awk '{ print $2 }')"
    if [[ -n $need_rust ]] && ! version_ge "${have_rust%%-*}" "$need_rust"; then
        die "Rust $need_rust or newer required, found $have_rust. Update with: rustup update stable"
    fi

    local missing=() dep name min have
    command -v cc >/dev/null 2>&1 || missing+=("C linker (cc)")
    if ! command -v pkg-config >/dev/null 2>&1; then
        missing+=("pkg-config")
    else
        for dep in "${PKG_DEPS[@]}"; do
            name=${dep%%:*} min=${dep#*:}
            if ! pkg-config --atleast-version="$min" "$name"; then
                have="$(pkg-config --modversion "$name" 2>/dev/null)" || have=missing
                missing+=("$name >= $min (found $have)")
            fi
        done
    fi
    ((${#missing[@]})) || return 0

    local apt_line="sudo apt-get install -y ${APT_PKGS[*]}"
    say "install: missing build dependencies:" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    if ((deps)) && command -v apt-get >/dev/null 2>&1; then
        say "install: running: $apt_line"
        sudo apt-get install -y "${APT_PKGS[@]}"
        # Confere de novo, agora sem --deps: o que o apt nao resolveu sai 2.
        deps=0
        check_build_deps
        return 0
    fi
    say "On Debian/Ubuntu install them with:" >&2
    say "  $apt_line" >&2
    say "or run: bash install.sh --deps" >&2
    say "GTK >= 4.22 and libadwaita >= 1.9 ship with Ubuntu 26.04; older releases cannot build the GUI." >&2
    exit 2
}

target_dir=${CARGO_TARGET_DIR:-$repo/target}
[[ $target_dir == /* ]] || target_dir="$repo/$target_dir"

if ((build)); then
    check_build_deps
    say "==> cargo build --release --locked -p agentcenter -p agentcenter-cli"
    cargo build --release --locked -p agentcenter -p agentcenter-cli
fi

for b in "${BINS[@]}"; do
    [[ -x $target_dir/release/$b ]] || die "$target_dir/release/$b not found (run without --no-build)"
done

# --- instalacao -----------------------------------------------------------------

declare -A seen_dir=()
created_dirs=()
installed_files=()

# Diretorios que o instalador criou, desta vez ou numa anterior (manifesto
# antigo): sao os que o --uninstall pode remover.
for d in "${dirs[@]}"; do
    if [[ -z ${seen_dir[$d]:-} ]]; then
        seen_dir[$d]=1
        created_dirs+=("$d")
    fi
done

ensure_dir() {
    local d=$1 new=()
    while [[ $d == "$prefix"/* && ! -e $d ]]; do
        new+=("$d")
        d=$(dirname "$d")
    done
    mkdir -p -- "$1"
    for d in "${new[@]}"; do
        if [[ -z ${seen_dir[$d]:-} ]]; then
            seen_dir[$d]=1
            created_dirs+=("$d")
        fi
    done
}

# Grava a entrada padrao num temporario no mesmo diretorio + mv: troca o
# binario mesmo com a app aberta e nunca deixa um arquivo pela metade.
put() {
    local dst=$1 mode=$2 tmp
    ensure_dir "$(dirname "$dst")"
    tmp="$(mktemp "$dst.XXXXXX")"
    if ! cat >"$tmp" || ! chmod "$mode" "$tmp" || ! mv -fT -- "$tmp" "$dst"; then
        rm -f -- "$tmp"
        die "could not write $dst"
    fi
    installed_files+=("$dst")
}

# O .desktop do repo com Exec e TryExec trocados (sem sed: o caminho iria
# para dentro de uma expressao de substituicao).
desktop_entry() {
    local line
    while IFS= read -r line || [[ -n $line ]]; do
        if [[ $line == Exec=* ]]; then
            printf 'Exec=%s\nTryExec=%s\n' "$1" "$2"
        elif [[ $line != TryExec=* ]]; then
            printf '%s\n' "$line"
        fi
    done <"data/$APP_ID.desktop"
}

mkdir -p -- "$prefix"

for b in "${BINS[@]}"; do
    put "$bindir/$b" 755 <"$target_dir/release/$b"
done

# Exec e TryExec em caminho absoluto: o menu acha a app mesmo com o bin/ do
# prefixo fora do PATH da sessao grafica. Aspas duplas protegem tambem os
# caracteres reservados do Exec, como apostrofo, ponto e virgula e parenteses.
# Aspas duplas, barras invertidas, $ e ` no prefixo ja foram recusados acima.
gui="$bindir/agentcenter"
exec_value="\"$gui\""
# Substituicao de processo, nao pipe: put precisa rodar neste shell para
# registrar o arquivo no manifesto.
put "$appsdir/$APP_ID.desktop" 644 < <(desktop_entry "$exec_value" "$gui")
put "$icondir/scalable/apps/$APP_ID.svg" 644 <"data/icons/hicolor/scalable/apps/$APP_ID.svg"
put "$schemadir/$APP_ID.gschema.xml" 644 <"data/$APP_ID.gschema.xml"

refresh_desktop_db
refresh_icon_cache
refresh_schemas

ensure_dir "$statedir"
manifest_tmp="$(mktemp "$manifest.XXXXXX")"
{
    for f in "${installed_files[@]}"; do printf 'file %s\n' "$f"; done
    for d in "${created_dirs[@]}"; do printf 'dir %s\n' "$d"; done
} >"$manifest_tmp"
mv -fT -- "$manifest_tmp" "$manifest"

# --- como abrir -------------------------------------------------------------------

say ""
say "AgentCenter installed in $prefix."
say "  Open it from the app menu: AgentCenter"
case ":$PATH:" in
    *":$bindir:"*) say "  or from a terminal: agentcenter   (headless: agentcenter-cli snapshot)" ;;
    *)
        say "  or from a terminal: $gui"
        say "  ($bindir is not on your PATH; add it to run agentcenter and agentcenter-cli by name)"
        ;;
esac
if [[ $prefix != "$HOME/.local" ]]; then
    say "  The menu and the saved preferences only see this prefix when"
    say "  $prefix/share is in XDG_DATA_DIRS (or is XDG_DATA_HOME)."
fi
# Mesmos comandos que a janela mostra (ENABLE_NOW e ENABLE_BOOT em window.rs).
say ""
if [[ "$(cat /proc/sys/kernel/task_delayacct 2>/dev/null || echo 0)" == 1 ]]; then
    say "I/O stall: kernel.task_delayacct is on. To keep it on after a reboot:"
else
    say "I/O stall per session needs the kernel's task delay accounting, which is off."
    say "Without it AgentCenter counts threads in D state instead. Enable it until the next boot:"
    say "  sudo sysctl kernel.task_delayacct=1"
    say "and to keep it on after a reboot:"
fi
say "  echo 'kernel.task_delayacct = 1' | sudo tee /etc/sysctl.d/90-task-delayacct.conf"
say ""
if [[ $prefix == "$HOME/.local" ]]; then
    say "Uninstall: bash install.sh --uninstall"
else
    say "Uninstall: bash install.sh --uninstall --prefix $(printf '%q' "$prefix")"
fi
