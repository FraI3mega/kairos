set -euo pipefail
. "$(dirname "$0")/lib.sh"

check supervisor_picks_up_a_session_that_starts_late DISPLAY=:98 &
test_run=$!
sleep 8
Xvfb :98 -screen 0 1280x800x24 >/dev/null 2>&1 &
DISPLAY=:98 wait_for xdotool getdisplaygeometry
DISPLAY=:98 openbox >/dev/null 2>&1 &
wait "$test_run"
