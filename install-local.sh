#!/usr/bin/env bash
# Builds razer-control-revived from this checkout and installs it system-wide:
# the razer-daemon systemd user service, the razer-cli tool, the razer-gui
# settings app, and their system assets (udev rules, service unit, desktop
# entry, icon, device database).
#
# Distro-agnostic by construction: only cargo, coreutils install(1), systemd,
# and udev are required; everything else (setcap, icon caches, KDE caches) is
# optional and skipped with a warning when absent.
#
# Usage:
#   ./install-local.sh              install (or upgrade in place)
#   ./install-local.sh --uninstall  remove everything installed by this script
#   ./install-local.sh --uninstall --purge   also remove config and device data

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR=/usr/bin
DATA_DIR=/usr/share/razercontrol
UDEV_RULE=/etc/udev/rules.d/99-hidraw-permissions.rules
SERVICE_UNIT=/usr/lib/systemd/user/razercontrol.service
DESKTOP_ENTRY=/usr/share/applications/razer-gui.desktop
ICON=/usr/share/icons/hicolor/scalable/apps/razer-gui.svg
SERVICE_NAME=razercontrol.service
USER_CONFIG_DIR="$HOME/.local/share/razercontrol"
PLASMOID_DIR="$HOME/.local/share/plasma/plasmoids/com.github.encomjp.razercontrol"

# Artifacts installed by the pre-workspace releases under different names;
# removed on install so upgrades do not leave stale launchers behind.
LEGACY_FILES=(
    /usr/bin/razer-settings
    /usr/share/applications/com.encomjp.razer-settings.desktop
    /usr/share/icons/hicolor/scalable/apps/com.github.encomjp.razercontrol.svg
)

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
    BOLD=$'\e[1m' DIM=$'\e[2m' RED=$'\e[31m' GREEN=$'\e[32m' YELLOW=$'\e[33m' CYAN=$'\e[36m' RESET=$'\e[0m'
else
    BOLD='' DIM='' RED='' GREEN='' YELLOW='' CYAN='' RESET=''
fi

banner() {
    printf '%s' "${CYAN}${BOLD}"
    printf '  ┌──────────────────────────────────────────────┐\n'
    printf '  │      razer-control-revived  ·  installer     │\n'
    printf '  └──────────────────────────────────────────────┘\n'
    printf '%s' "${RESET}"
}

section() { printf '\n%s%s%s\n' "${BOLD}" "$1" "${RESET}"; }
ok()      { printf '  %s[ ok ]%s %s\n' "${GREEN}" "${RESET}" "$1"; }
skip()    { printf '  %s[skip]%s %s\n' "${YELLOW}" "${RESET}" "$1"; }
detail()  { printf '         %s%s%s\n' "${DIM}" "$1" "${RESET}"; }
die()     { printf '  %s[fail]%s %s\n' "${RED}" "${RESET}" "$1" >&2; exit 1; }

require() {
    command -v "$1" > /dev/null || die "required tool not found: $1 ($2)"
}

preflight() {
    section "Preflight"
    [[ $EUID -ne 0 ]] || die "run as your regular user; sudo is used only where needed"
    require cargo "install a Rust toolchain, e.g. via rustup"
    require sudo "system files are installed with sudo"
    require udevadm "udev is required for hidraw device access"
    require systemctl "the daemon runs as a systemd user service"
    systemctl --user show-environment > /dev/null 2>&1 \
        || die "no systemd user session found (systemctl --user unavailable)"
    ok "running as $USER, tooling present"
    sudo -v || die "sudo authentication failed"
    ok "sudo authenticated"
}

build() {
    section "Build"
    detail "cargo build --release (razer-daemon, razer-cli, razer-gui)"
    cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml" \
        -p razer-daemon -p razer-cli -p razer-gui
    ok "release binaries built"
}

install_files() {
    section "Install"
    # The build can outlast sudo's cached credential; re-validate so the
    # install steps run without a surprise mid-flow prompt.
    sudo -v || die "sudo authentication failed"
    sudo install -Dm755 "$SCRIPT_DIR/target/release/razer-daemon" "$BIN_DIR/razer-daemon"
    sudo install -Dm755 "$SCRIPT_DIR/target/release/razer-cli" "$BIN_DIR/razer-cli"
    sudo install -Dm755 "$SCRIPT_DIR/target/release/razer-gui" "$BIN_DIR/razer-gui"
    ok "binaries -> $BIN_DIR/{razer-daemon,razer-cli,razer-gui}"

    sudo install -Dm644 "$SCRIPT_DIR/data/devices/laptops.json" "$DATA_DIR/laptops.json"
    ok "device database -> $DATA_DIR/laptops.json"

    sudo install -Dm644 "$SCRIPT_DIR/data/udev/99-hidraw-permissions.rules" "$UDEV_RULE"
    ok "udev rule -> $UDEV_RULE"

    sudo install -Dm644 "$SCRIPT_DIR/data/services/systemd/razercontrol.service" "$SERVICE_UNIT"
    ok "systemd user unit -> $SERVICE_UNIT"

    sudo install -Dm644 "$SCRIPT_DIR/crates/razer-gui/data/razer-gui.desktop" "$DESKTOP_ENTRY"
    sudo install -Dm644 "$SCRIPT_DIR/crates/razer-gui/data/icon.svg" "$ICON"
    ok "desktop entry and icon installed"
}

grant_daemon_capability() {
    section "Privileges"
    # The kernel restricts /sys/class/powercap/*/energy_uj to root as a
    # side-channel mitigation (PLATYPUS), so the user-session daemon cannot
    # read CPU package power on its own. A file capability on the daemon
    # binary lets exactly this one binary bypass the read check, keeping the
    # mitigation intact for every other process. Re-applied on every install
    # because file capabilities are lost when the binary is replaced.
    if command -v setcap > /dev/null; then
        sudo setcap cap_dac_read_search+ep "$BIN_DIR/razer-daemon"
        ok "cap_dac_read_search granted to razer-daemon (CPU wattage telemetry)"
    else
        skip "setcap not found (libcap): CPU wattage will show as unavailable"
    fi
}

remove_legacy() {
    section "Legacy cleanup"
    local removed=0
    for file in "${LEGACY_FILES[@]}"; do
        if [[ -e $file ]]; then
            sudo rm -f "$file"
            detail "removed $file"
            removed=1
        fi
    done
    if [[ $removed -eq 1 ]]; then
        ok "pre-workspace artifacts removed"
    else
        skip "no pre-workspace artifacts found"
    fi
}

activate() {
    section "Activate"
    sudo udevadm control --reload-rules
    sudo udevadm trigger --subsystem-match=hidraw
    ok "udev rules reloaded"

    systemctl --user daemon-reload
    systemctl --user enable "$SERVICE_NAME" > /dev/null 2>&1 || true
    if systemctl --user restart "$SERVICE_NAME"; then
        ok "$SERVICE_NAME enabled and (re)started"
    else
        skip "daemon did not start cleanly; the summary below has the check command"
    fi
}

refresh_caches() {
    section "Desktop integration"
    if command -v gtk-update-icon-cache > /dev/null; then
        sudo gtk-update-icon-cache -f -t /usr/share/icons/hicolor > /dev/null 2>&1 || true
        ok "icon cache refreshed"
    else
        skip "gtk-update-icon-cache not found"
    fi
    if command -v kbuildsycoca6 > /dev/null; then
        kbuildsycoca6 --noincremental > /dev/null 2>&1 || true
        ok "KDE application cache refreshed"
    elif command -v kbuildsycoca5 > /dev/null; then
        kbuildsycoca5 --noincremental > /dev/null 2>&1 || true
        ok "KDE application cache refreshed"
    else
        skip "no KDE cache tool found"
    fi
    if [[ -d $PLASMOID_DIR ]]; then
        cp -r "$SCRIPT_DIR/kde-widget/package/." "$PLASMOID_DIR/"
        ok "KDE plasmoid updated in place"
    fi
}

summary() {
    section "Done"
    if systemctl --user is-active --quiet "$SERVICE_NAME"; then
        ok "daemon is running (systemctl --user status $SERVICE_NAME)"
    else
        skip "daemon is not active yet; check: systemctl --user status $SERVICE_NAME"
    fi
    detail "settings app:  razer-gui  (also in your application launcher)"
    detail "command line:  razer-cli"
    detail "uninstall:     ./install-local.sh --uninstall [--purge]"
}

uninstall() {
    local purge=$1
    section "Uninstall"
    systemctl --user disable --now "$SERVICE_NAME" > /dev/null 2>&1 || true
    ok "$SERVICE_NAME stopped and disabled"

    local files=(
        "$BIN_DIR/razer-daemon" "$BIN_DIR/razer-cli" "$BIN_DIR/razer-gui"
        "$UDEV_RULE" "$SERVICE_UNIT" "$DESKTOP_ENTRY" "$ICON"
        "${LEGACY_FILES[@]}"
    )
    for file in "${files[@]}"; do
        if [[ -e $file ]]; then
            sudo rm -f "$file"
            detail "removed $file"
        fi
    done
    ok "installed files removed"

    if [[ $purge -eq 1 ]]; then
        sudo rm -rf "$DATA_DIR"
        rm -rf "$USER_CONFIG_DIR"
        ok "purged $DATA_DIR and $USER_CONFIG_DIR"
    else
        skip "kept $DATA_DIR and $USER_CONFIG_DIR (use --purge to remove)"
    fi

    sudo udevadm control --reload-rules
    systemctl --user daemon-reload
    ok "udev and systemd reloaded"
}

usage() {
    sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

main() {
    local mode=install purge=0
    for arg in "$@"; do
        case $arg in
            --uninstall) mode=uninstall ;;
            --purge) purge=1 ;;
            -h | --help)
                usage
                exit 0
                ;;
            *) die "unknown option: $arg (see --help)" ;;
        esac
    done
    [[ $mode == uninstall || $purge -eq 0 ]] || die "--purge only applies with --uninstall"

    banner
    if [[ $mode == uninstall ]]; then
        [[ $EUID -ne 0 ]] || die "run as your regular user; sudo is used only where needed"
        require sudo "system files are removed with sudo"
        sudo -v || die "sudo authentication failed"
        uninstall "$purge"
    else
        preflight
        build
        install_files
        grant_daemon_capability
        remove_legacy
        activate
        refresh_caches
        summary
    fi
}

main "$@"
