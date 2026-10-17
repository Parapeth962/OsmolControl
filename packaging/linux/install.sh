#!/usr/bin/env bash
# OsmolControl + MyWhoosh for Linux: the one script to run.
#
#   ./install.sh            asks which MyWhoosh to install, then does everything
#   ./install.sh --hd       MyWhoosh HD without asking
#   ./install.sh --standard MyWhoosh without asking
#
# It installs the few system packages it needs (apt, dnf or pacman; asks for your password once),
# installs OsmolControl, downloads MyWhoosh and prepares it to ride with OsmolControl. Run it as
# your normal user, not with sudo.
set -euo pipefail

HERE="$(dirname "$(realpath "$0")")"
OSMOL="$HOME/.local/bin/osmolcontrol"

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
say() { printf '  %s\n' "$*"; }
die() { printf '\n\033[31mProblem: %s\033[0m\n' "$*" >&2; exit 1; }
ask() { local answer; read -r -p "  $1 " answer </dev/tty; echo "$answer"; }

[[ $EUID -ne 0 ]] || die "run this as your normal user, without sudo (it asks for the password itself)"
[[ "$(uname -m)" == x86_64 ]] || die "only 64-bit PCs (x86_64) are supported"

bold "OsmolControl + MyWhoosh installer"

# --- which MyWhoosh -------------------------------------------------------------------------
edition=""
for arg in "$@"; do
    case "$arg" in
        --hd) edition=hd ;;
        --standard) edition=standard ;;
        *) die "unknown option $arg" ;;
    esac
done
if [[ -z "$edition" ]]; then
    bold "Which MyWhoosh do you want?"
    say "1) MyWhoosh      - recommended, runs on most PCs (download about 4 GB)"
    say "2) MyWhoosh HD   - nicer graphics, needs a strong graphics card (about 15 GB)"
    case "$(ask "Type 1 or 2 and press Enter [1]:")" in
        2) edition=hd ;;
        *) edition=standard ;;
    esac
fi
need_gb=8
[[ "$edition" == hd ]] && need_gb=20
free_gb=$(( $(df -Pk "$HOME" | awk 'NR==2 {print $4}') / 1024 / 1024 ))
((free_gb >= need_gb)) || die "not enough disk space: $need_gb GB needed in your home folder, $free_gb GB free"

# --- system packages ------------------------------------------------------------------------
bold "Step 1/3: system packages"
if command -v apt-get >/dev/null; then
    packages=(python3 curl ca-certificates tar libnotify-bin bluez mesa-vulkan-drivers xdg-user-dirs)
    install_cmd=(sudo apt-get install -y)
    refresh=(sudo apt-get update)
elif command -v dnf >/dev/null; then
    packages=(python3 curl ca-certificates tar libnotify bluez mesa-vulkan-drivers xdg-user-dirs)
    install_cmd=(sudo dnf install -y)
    refresh=()
elif command -v pacman >/dev/null; then
    packages=(python curl ca-certificates tar libnotify bluez bluez-utils xdg-user-dirs)
    install_cmd=(sudo pacman -S --needed --noconfirm)
    refresh=()
else
    die "this Linux has neither apt, dnf nor pacman; install python3, curl, tar and bluez yourself and rerun"
fi
say "Installing: ${packages[*]}"
say "Your password may be asked for (this is sudo, the normal way to install software)."
((${#refresh[@]} == 0)) || "${refresh[@]}"
"${install_cmd[@]}" "${packages[@]}" || die "installing system packages failed"
if ! systemctl is-active --quiet bluetooth; then
    say "Turning on the Bluetooth service"
    sudo systemctl enable --now bluetooth || say "Could not start Bluetooth; check that your PC has it switched on"
fi

# --- OsmolControl ---------------------------------------------------------------------------
bold "Step 2/3: OsmolControl"
mkdir -p "$HOME/.local/bin"
if [[ -x "$HERE/osmolcontrol" ]]; then
    install -m 755 "$HERE/osmolcontrol" "$OSMOL"
    say "Installed from this package"
else
    repo="$(cd "$HERE/../.." && pwd)"
    [[ -f "$repo/Cargo.toml" ]] || die "osmolcontrol is missing from this folder; download the complete package again"
    say "No ready-made OsmolControl here: building it from the source code (5-10 minutes)."
    if command -v apt-get >/dev/null; then
        sudo apt-get install -y build-essential pkg-config libdbus-1-dev
    elif command -v dnf >/dev/null; then
        sudo dnf install -y gcc pkgconf-pkg-config dbus-devel
    else
        sudo pacman -S --needed --noconfirm base-devel pkgconf dbus
    fi
    if ! command -v cargo >/dev/null && [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
        say "Installing the Rust compiler (rustup.rs, the official installer)"
        curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
    fi
    PATH="$HOME/.cargo/bin:$PATH"
    (cd "$repo" && cargo build --locked --release -p bikecontrol) || die "building OsmolControl failed"
    install -m 755 "$repo/target/release/osmolcontrol" "$OSMOL"
    say "Built and installed"
fi

# --- MyWhoosh -------------------------------------------------------------------------------
bold "Step 3/3: MyWhoosh (the longest part)"
flags=(--dircon)
[[ "$edition" == hd ]] && flags+=(--hd)
"$HERE/mywhoosh/mywhoosh.sh" install "${flags[@]}"

title="MyWhoosh"
[[ "$edition" == hd ]] && title="MyWhoosh HD"
bold "Done!"
cat <<EOF
  How to ride:
   1. Switch on the trainer and the controller. Put the chain on the BIG front chainring.
   2. Start "$title" from the desktop or the application menu.
      OsmolControl opens by itself; press "Start ride" in it.
   3. In MyWhoosh: Device Connection -> BLE -> Power Source -> OsmolControl.
      Turn off Virtual Shifting in MyWhoosh's settings. Shift with your controller.

  MyWhoosh updates itself: when a new version is out, the next start downloads it first.
  Something wrong? Run:  ~/Games/$( [[ "$edition" == hd ]] && echo mywhoosh-hd-linux || echo mywhoosh-linux )/bin/mywhoosh.sh doctor
EOF
