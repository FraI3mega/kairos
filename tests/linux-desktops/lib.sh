T=/kairosd-test
M=platform::linux::desktop_tests

# Extra arguments go to env(1), so `-i` starts the test with an empty environment.
check() {
  local name=$1
  shift
  if env "$@" "$T" --ignored --test-threads=1 --nocapture "$M::$name" >/tmp/check.log 2>&1; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    cat /tmp/check.log
    return 1
  fi
}

wait_for() {
  for _ in $(seq 150); do
    "$@" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  echo "timed out waiting for: $*" >&2
  return 1
}
