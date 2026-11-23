set -euo pipefail
. "$(dirname "$0")/lib.sh"

export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -m 700 -p "$XDG_RUNTIME_DIR"
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway >/tmp/sway.log 2>&1 &
wait_for test -S "$XDG_RUNTIME_DIR/wayland-1"
export WAYLAND_DISPLAY=wayland-1
# SWAYSOCK is not exported on purpose: the daemon has to find the socket in the runtime dir.
wait_for sh -c 'ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock'

check sway_focus_follows_focused_windows
check ext_idle_notify_reports_idle
check supervisor_selects_the_desktops_backends KAIROS_EXPECT_BACKENDS=sway,ext-idle-notify

foot -a probe sleep 600 &
wait_for pgrep -x foot
check session_is_found_from_other_processes -i HOME=/root
