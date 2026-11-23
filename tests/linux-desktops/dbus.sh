set -euo pipefail
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || exec dbus-run-session -- bash "$0"
. "$(dirname "$0")/lib.sh"

has_owner() {
  dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus.NameHasOwner "string:$1" | grep -q "boolean true"
}

check dbus_kwin_service_receives_focus_and_notices_kwin_leaving
check dbus_mutter_idle_monitor
check dbus_screensaver_idle_time

sleep 600 & a=$!
sleep 600 & b=$!
sleep 600 & c=$!
for variant in modern legacy; do
  gjs "$(dirname "$0")/gnome-shell-harness.js" "$variant" "$a,$b,$c" &
  harness=$!
  wait_for has_owner org.gnome.Shell
  check dbus_gnome_extension_focus KAIROS_EXPECT_PIDS="$a,$b,$c"
  kill "$harness"
  wait "$harness" 2>/dev/null || true
done
kill "$a" "$b" "$c"
