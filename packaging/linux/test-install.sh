#!/usr/bin/env bash
# Runs install.sh against a throwaway home with stand-ins for sudo, the package manager,
# systemctl, df and mywhoosh.sh, so nothing is installed and no password is needed.
# Checks a fresh install, rerunning it to update (the game is kept) and bad input.
#
#   packaging/linux/test-install.sh        (also run by CI)
set -euo pipefail

HERE="$(dirname "$(realpath "$0")")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
failures=0

# A package in the layout install.sh expects: the script, a ready-made binary, the launcher.
PKG="$WORK/pkg"
mkdir -p "$PKG/mywhoosh" "$WORK/bin"
cp "$HERE/install.sh" "$PKG/"
printf '#!/bin/sh\necho osmolcontrol\n' >"$PKG/osmolcontrol"
chmod +x "$PKG/osmolcontrol"
echo "# bridge" >"$PKG/mywhoosh/ble-bridge.py"
cat >"$PKG/mywhoosh/mywhoosh.sh" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$HOME/mywhoosh-calls"
EOF
chmod +x "$PKG/mywhoosh/mywhoosh.sh"

# Stand-ins: sudo runs the command, package managers and systemctl succeed, df reports 100 GB
# free, pgrep finds nothing running.
stub() { printf '#!/bin/sh\n%s\n' "$2" >"$WORK/bin/$1"; chmod +x "$WORK/bin/$1"; }
stub sudo 'exec "$@"'
stub apt-get 'exit 0'
stub systemctl 'exit 0'
stub pgrep 'exit 1'
stub df 'echo "Filesystem 1K-blocks Used Available Use% Mounted"; echo "fake 1 1 104857600 1% /"'

# run <name> <expected exit status> <install.sh arguments...>; output in $WORK/<name>.log
run() {
    local name="$1" want="$2" status=0
    shift 2
    HOME="$WORK/home-$name" PATH="$WORK/bin:$PATH" bash "$PKG/install.sh" "$@" \
        >"$WORK/$name.log" 2>&1 </dev/null || status=$?
    if [[ "$status" != "$want" ]]; then
        fail "$name: exit $status, expected $want"
        sed 's/^/    /' "$WORK/$name.log" >&2
    fi
}
fail() { printf 'FAIL %s\n' "$*" >&2; failures=$((failures + 1)); }
expect() { [[ -e "$2" ]] || fail "$1: missing ${2#"$WORK"/}"; }
absent() { [[ ! -e "$2" ]] || fail "$1: unexpected ${2#"$WORK"/}"; }
says() { grep -q -- "$2" "$WORK/$1.log" || fail "$1: output lacks \"$2\""; }
calls() { local got; got="$(cat "$WORK/home-$1/mywhoosh-calls" 2>/dev/null || true)"
    [[ "$got" == "$2" ]] || fail "$1: mywhoosh.sh called with \"$got\", expected \"$2\""; }
installed_hd() {
    mkdir -p "$WORK/home-$1/Games/mywhoosh-hd-linux/game" "$WORK/home-$1/Games/mywhoosh-hd-linux/bin"
    echo hd >"$WORK/home-$1/Games/mywhoosh-hd-linux/edition"
}

# Fresh install with the edition given: OsmolControl, icon, then the game.
run fresh 0 --standard
expect fresh "$WORK/home-fresh/.local/bin/osmolcontrol"
calls fresh "install --dircon"
says fresh "Done!"

run fresh-hd 0 --hd
calls fresh-hd "install --dircon --hd"

# Rerun with MyWhoosh HD installed: no question, no second game, launcher refreshed.
installed_hd update
run update 0
says update "Found an installed MyWhoosh"
says update "already installed"
calls update ""
expect update "$WORK/home-update/Games/mywhoosh-hd-linux/bin/mywhoosh.sh"
expect update "$WORK/home-update/Games/mywhoosh-hd-linux/bin/ble-bridge.py"
expect update "$WORK/home-update/.local/bin/osmolcontrol"
absent update "$WORK/home-update/Games/mywhoosh-linux"

installed_hd update-hd
run update-hd 0 --hd
calls update-hd ""

# Asking for the other edition installs it next to the existing one.
installed_hd second
run second 0 --standard
calls second "install --dircon"

# Bad input stops before anything is installed.
run bad-option 1 --foo
says bad-option "unknown option --foo"
absent bad-option "$WORK/home-bad-option/.local/bin/osmolcontrol"

if ((failures)); then
    printf '%d installer check(s) failed\n' "$failures" >&2
    exit 1
fi
echo "installer checks passed"
