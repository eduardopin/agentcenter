#!/usr/bin/env bash
# Teste do install.sh num prefixo descartavel, sem rede, sem sudo e sem build:
# binarios de mentira num CARGO_TARGET_DIR proprio e --no-build.
#
#   bash scripts/test-install.sh
#
# Confere: instala tudo no prefixo (caminho com espaco, para o Exec com
# aspas), segunda rodada identica (idempotente), desktop-file-validate e
# glib-compile-schemas --strict no prefixo (pulados com aviso se a ferramenta
# faltar), --uninstall deixa o prefixo vazio e repetido nao falha; num prefixo
# com arquivos de outros programas, o --uninstall so tira os seus; manifesto
# com caminho fora do prefixo e recusado; --no-build sem binario sai 2 sem
# escrever nada. Chamado pelo scripts/ci.sh.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

APP_ID=io.github.eduardopin.AgentCenter
target="${CARGO_TARGET_DIR:-$PWD/target}"
mkdir -p "$target"
root="$(mktemp -d "$target/install-test.XXXXXX")"
trap 'rm -rf "$root"' EXIT

fail() { echo "test-install: FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

stub="$root/stub"
mkdir -p "$stub/release"
for b in agentcenter agentcenter-cli; do
    printf '#!/bin/sh\necho %s-stub\n' "$b" >"$stub/release/$b"
    chmod 755 "$stub/release/$b"
done

inst() { CARGO_TARGET_DIR="$stub" bash install.sh --no-build "$@" >"$root/last.log" 2>&1; }

# Arvore do prefixo com tipo, modo e conteudo; os caches gerados ficam fora
# do conteudo (gtk-update-icon-cache grava a hora).
tree() {
    (cd "$1" && find . -printf '%y %m %p\n' | LC_ALL=C sort &&
        find . -type f ! -name icon-theme.cache ! -name mimeinfo.cache ! -name gschemas.compiled \
            -exec md5sum {} + | LC_ALL=C sort -k2)
}

# --- 1. prefixo vazio --------------------------------------------------------------
prefix="$root/pre fix"
inst --prefix "$prefix" || { cat "$root/last.log" >&2; fail "install exited non-zero"; }

expected=(
    "bin/agentcenter"
    "bin/agentcenter-cli"
    "share/applications/$APP_ID.desktop"
    "share/icons/hicolor/scalable/apps/$APP_ID.svg"
    "share/glib-2.0/schemas/$APP_ID.gschema.xml"
    "share/agentcenter/installed.txt"
)
for f in "${expected[@]}"; do
    [[ -f $prefix/$f ]] || fail "missing $f"
done
for b in agentcenter agentcenter-cli; do
    [[ -x $prefix/bin/$b ]] || fail "bin/$b is not executable"
    cmp -s "$stub/release/$b" "$prefix/bin/$b" || fail "bin/$b differs from the build"
done
cmp -s "data/$APP_ID.gschema.xml" "$prefix/share/glib-2.0/schemas/$APP_ID.gschema.xml" || fail "schema differs"
cmp -s "data/icons/hicolor/scalable/apps/$APP_ID.svg" "$prefix/share/icons/hicolor/scalable/apps/$APP_ID.svg" ||
    fail "icon differs"
desktop="$prefix/share/applications/$APP_ID.desktop"
grep -qxF "Exec=\"$prefix/bin/agentcenter\"" "$desktop" || fail "Exec is not the quoted absolute path: $(grep '^Exec=' "$desktop")"
grep -qxF "TryExec=$prefix/bin/agentcenter" "$desktop" || fail "TryExec is not the absolute path"
[[ $(grep -c '^Exec=' "$desktop") == 1 ]] || fail "more than one Exec line"
# Fora Exec/TryExec, o .desktop e o do repo.
diff <(grep -v '^Exec=' "data/$APP_ID.desktop") <(grep -v -e '^Exec=' -e '^TryExec=' "$desktop") >/dev/null ||
    fail ".desktop changed beyond Exec/TryExec"
grep -q "installed in $prefix" "$root/last.log" || fail "no closing message"
grep -q "AgentCenter" "$root/last.log" || fail "closing message does not name the menu entry"
grep -q "task_delayacct" "$root/last.log" || fail "closing message has no task_delayacct hint"
ok "install into an empty prefix"

before="$(tree "$prefix")"
manifest_before="$(cat "$prefix/share/agentcenter/installed.txt")"
inst --prefix "$prefix" || { cat "$root/last.log" >&2; fail "second install exited non-zero"; }
[[ "$(tree "$prefix")" == "$before" ]] || fail "second install changed the prefix"
[[ "$(cat "$prefix/share/agentcenter/installed.txt")" == "$manifest_before" ]] || fail "second install changed the manifest"
ok "second install is identical"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop" || fail "desktop-file-validate"
    ok "desktop-file-validate"
else
    echo "skip: desktop-file-validate not found (apt install desktop-file-utils)"
fi
if command -v glib-compile-schemas >/dev/null 2>&1; then
    glib-compile-schemas --strict --dry-run "$prefix/share/glib-2.0/schemas" || fail "glib-compile-schemas --strict"
    [[ -f $prefix/share/glib-2.0/schemas/gschemas.compiled ]] || fail "gschemas.compiled not built"
    ok "glib-compile-schemas --strict"
else
    echo "skip: glib-compile-schemas not found (apt install libglib2.0-bin)"
fi

inst --uninstall --prefix "$prefix" || { cat "$root/last.log" >&2; fail "uninstall exited non-zero"; }
left="$(find "$prefix" -mindepth 1)"
[[ -z $left ]] || fail "uninstall left: $left"
inst --uninstall --prefix "$prefix" || fail "second uninstall exited non-zero"
ok "uninstall leaves the prefix empty, and again is a no-op"

# --- 2. prefixo compartilhado ------------------------------------------------------------
shared="$root/shared"
mkdir -p "$shared/bin" "$shared/share/applications"
printf '#!/bin/sh\n' >"$shared/bin/other"
printf '[Desktop Entry]\nType=Application\nName=Other\nExec=other\n' >"$shared/share/applications/other.desktop"
inst --prefix "$shared" || fail "install into a shared prefix"
grep -qx "dir $shared/bin" "$shared/share/agentcenter/installed.txt" && fail "manifest claims bin/, which existed before"
inst --uninstall --prefix "$shared" || fail "uninstall from a shared prefix"
[[ -f $shared/bin/other && -f $shared/share/applications/other.desktop ]] || fail "uninstall removed another program's file"
for f in "${expected[@]}"; do
    [[ ! -e $shared/$f ]] || fail "uninstall left $f"
done
[[ ! -e $shared/share/agentcenter && ! -e $shared/share/icons && ! -e $shared/share/glib-2.0 ]] ||
    fail "uninstall left a directory it created"
ok "uninstall from a shared prefix removes only its own files"

# --- 3. manifesto adulterado ------------------------------------------------------------------
evil="$root/evil"
inst --prefix "$evil" || fail "install into $evil"
outside="$root/outside.txt"
echo keep >"$outside"
echo "file $outside" >>"$evil/share/agentcenter/installed.txt"
if inst --uninstall --prefix "$evil"; then fail "uninstall accepted a manifest entry outside the prefix"; fi
[[ -f $outside ]] || fail "uninstall deleted a file outside the prefix"
ok "manifest entry outside the prefix is refused"

# --- 4. --no-build sem binario, opcao desconhecida -------------------------------------------
empty="$root/empty-target"
mkdir -p "$empty"
nobin="$root/nobin"
set +e
CARGO_TARGET_DIR="$empty" bash install.sh --no-build --prefix "$nobin" >/dev/null 2>&1
code=$?
bash install.sh --bogus >/dev/null 2>&1
bogus=$?
set -e
[[ $code == 2 ]] || fail "--no-build without binaries exited $code, want 2"
[[ ! -e $nobin ]] || fail "--no-build without binaries wrote into the prefix"
[[ $bogus == 2 ]] || fail "unknown option exited $bogus, want 2"
ok "--no-build without binaries and an unknown option exit 2"

# --- 5. regressao: manifesto nao autoriza exclusao arbitraria nem via symlink ---
# Snapshot inclui caches e destinos dos links, sem seguir links externos.
# Cada recusa deve preceder QUALQUER escrita/exclusao, inclusive arquivos geridos.
exact_tree() {
    (cd "$1" && find . -printf '%y %m %p %l\n' | LC_ALL=C sort &&
        find . -type f -exec sha256sum {} + | LC_ALL=C sort -k2)
}
security_cases=0
security_failures=0
security_bad() {
    echo "not ok: $*" >&2
    security_failures=$((security_failures + 1))
}
security_fixture() {
    security_cases=$((security_cases + 1))
    case_root="$root/security-$security_cases"
    secure="$case_root/prefix"
    external="$case_root/external"
    mkdir -p "$external"
    printf 'external sentinel\n' >"$external/keep"
    inst --prefix "$secure" || fail "security fixture install"
    printf 'internal sentinel\n' >"$secure/keep"
}
refuse_unchanged() {
    local label=$1 operation=$2 before_all after_all status=0
    before_all="$(exact_tree "$case_root")"
    if [[ $operation == uninstall ]]; then
        inst --uninstall --prefix "$secure" || status=$?
    else
        inst --prefix "$secure" || status=$?
    fi
    after_all="$(exact_tree "$case_root")"
    if [[ $status != 2 || $before_all != "$after_all" ]]; then
        security_bad "$label/$operation: exit=$status (want 2), whole fixture unchanged=$([[ $before_all == "$after_all" ]] && echo yes || echo no)"
    else
        ok "$label/$operation refuses before any partial deletion or write"
    fi
}

for operation in install uninstall; do
    for attack in traversal internal-file internal-directory; do
        security_fixture
        # Entradas validas aparecem ANTES da entrada maliciosa: nao pode apagar
        # binarios antes de descobrir que o restante do manifesto e invalido.
        case $attack in
            traversal) printf 'file %s/../external/keep\n' "$secure" ;;
            internal-file) printf 'file %s/keep\n' "$secure" ;;
            internal-directory)
                mkdir "$secure/unmanaged-empty"
                printf 'dir %s/unmanaged-empty\n' "$secure" ;;
        esac >>"$secure/share/agentcenter/installed.txt"
        refuse_unchanged "manifest-$attack" "$operation"
    done

    # O manifesto continua com os caminhos lexicais originais; o pai agora
    # aponta para fora. Copiamos os arquivos para que a exclusao externa seja real.
    for relative in bin share/applications share/icons/hicolor share/glib-2.0/schemas share/agentcenter; do
        security_fixture
        cp -a "$secure/$relative" "$external/redirected"
        rm -rf "$secure/$relative"
        ln -s "$external/redirected" "$secure/$relative"
        refuse_unchanged "directory-symlink-$relative" "$operation"
    done

    # Caches nao constam no manifesto, mas as ferramentas poderiam seguir links.
    for relative in share/applications/mimeinfo.cache share/icons/hicolor/icon-theme.cache share/glib-2.0/schemas/gschemas.compiled; do
        security_fixture
        rm -f "$secure/$relative"
        ln -s "$external/keep" "$secure/$relative"
        refuse_unchanged "cache-symlink-$relative" "$operation"
    done

    security_fixture
    mv "$secure/share/agentcenter/installed.txt" "$external/manifest"
    ln -s "$external/manifest" "$secure/share/agentcenter/installed.txt"
    refuse_unchanged manifest-symlink "$operation"
done

# Symlink FINAL gerido e diferente de um pai/cache: substituir/remover o link
# e permitido, mas mv nao pode entrar num diretorio externo e rm nao o segue.
for operation in install uninstall; do
    security_fixture
    rm "$secure/bin/agentcenter"
    ln -s "$external" "$secure/bin/agentcenter"
    external_before="$(exact_tree "$external")"
    status=0
    if [[ $operation == install ]]; then
        inst --prefix "$secure" || status=$?
        if [[ -L $secure/bin/agentcenter ]] || ! cmp -s "$stub/release/agentcenter" "$secure/bin/agentcenter"; then
            security_bad "final-symlink/install did not replace the link with the binary"
        fi
    else
        inst --uninstall --prefix "$secure" || status=$?
        if [[ -e $secure/bin/agentcenter || -L $secure/bin/agentcenter ]]; then
            security_bad "final-symlink/uninstall left the link"
        fi
    fi
    if [[ $status != 0 || "$(exact_tree "$external")" != "$external_before" || $(<"$secure/keep") != 'internal sentinel' ]]; then
        security_bad "final-symlink/$operation changed a sentinel or exited $status"
    else
        ok "final-symlink/$operation preserves internal and external sentinels"
    fi
done
[[ $security_failures == 0 ]] || fail "$security_failures security assertions failed across $security_cases scenarios"
ok "$security_cases security regression scenarios"

# --- 6. regressao: Exec reservados sem whitespace tambem precisam de aspas ---
# O quoting antigo so tratava whitespace: apostrofo/;/parenteses produziam
# desktops invalidos. Validador real + GIO real executam apenas este stub,
# sem display, sessao D-Bus ou diretorios de usuario reais.
for tool in desktop-file-validate gio timeout; do
    command -v "$tool" >/dev/null 2>&1 ||
        fail "Exec regression requires $tool (desktop-file-utils, libglib2.0-bin, coreutils)"
done
cat >"$stub/release/agentcenter" <<'STUB'
#!/bin/sh
printf '%s\n%s\n' "$0" "$#" >"$C25_LAUNCH_RESULT"
STUB
chmod 755 "$stub/release/agentcenter"
launch_home="$root/launch-home"
mkdir -p "$launch_home"
exec_cases=0
# Todos os reservados de Exec aceitos pelo instalador, exceto whitespace.
for reserved in "'" ';' '(' ')' '&' '#' '<' '>' '~' '|' '*' '?' "a';()&#<>~|*?z"; do
    exec_cases=$((exec_cases + 1))
    exec_prefix="$root/exec-$reserved"
    [[ $exec_prefix != *[[:space:]]* ]] || fail "Exec regression fixture contains whitespace"
    inst --prefix "$exec_prefix" || fail "reserved Exec install: $reserved"
    exec_desktop="$exec_prefix/share/applications/$APP_ID.desktop"
    desktop-file-validate "$exec_desktop" || fail "reserved Exec desktop validation: $reserved"
    grep -qxF "Exec=\"$exec_prefix/bin/agentcenter\"" "$exec_desktop" ||
        fail "reserved Exec is not a quoted absolute path: $reserved"
    launch_result="$root/launched-$exec_cases"
    env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        HOME="$launch_home" XDG_CONFIG_HOME="$launch_home/config" \
        XDG_DATA_HOME="$launch_home/data" XDG_CACHE_HOME="$launch_home/cache" \
        C25_LAUNCH_RESULT="$launch_result" \
        timeout 5 gio launch "$exec_desktop" >"$root/launch.log" 2>&1 || {
            cat "$root/launch.log" >&2
            fail "reserved Exec GIO launch: $reserved"
        }
    # GIO pode retornar antes de o processo filho gravar a evidencia.
    for ((waited=0; waited<100; waited++)); do
        [[ -s $launch_result ]] && break
        sleep 0.01
    done
    [[ -f $launch_result ]] || fail "reserved Exec did not execute stub: $reserved"
    printf '%s\n0\n' "$exec_prefix/bin/agentcenter" >"$root/expected-launch"
    cmp -s "$root/expected-launch" "$launch_result" ||
        fail "reserved Exec launched wrong executable or extra arguments: $reserved"
    inst --uninstall --prefix "$exec_prefix" || fail "reserved Exec uninstall: $reserved"
    [[ -z $(find "$exec_prefix" -mindepth 1 -print -quit) ]] ||
        fail "reserved Exec uninstall left files: $reserved"
    ok "reserved Exec $reserved validates and launches the exact stub without arguments"
done
ok "$exec_cases reserved Exec regression scenarios"

echo "test-install: ok"
