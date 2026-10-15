#!/usr/bin/env bash
set -euo pipefail
BIN="${1:?usage: runtime-smoke.sh /path/to/hoard}"
[[ -x "$BIN" ]] || { echo "runtime smoke binary is not executable: $BIN" >&2; exit 1; }
[[ -n "${WAYLAND_DISPLAY:-}" ]] || { echo "WAYLAND_DISPLAY is required for native runtime smoke" >&2; exit 1; }
ROOT="$(mktemp -d -t hoard-native-smoke.XXXXXX)"
cleanup() {
  if [[ -n "${PID:-}" ]] && kill -0 "$PID" 2>/dev/null; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  rm -rf "$ROOT"
}
trap cleanup EXIT
mkdir -p "$ROOT/config" "$ROOT/cache" "$ROOT/data" "$ROOT/state" "$ROOT/home/testdir/subdir"
printf 'alpha\n' > "$ROOT/home/testdir/alpha.txt"
printf 'beta\n' > "$ROOT/home/testdir/beta.txt"
LOG="$ROOT/native.log"
HOME="$ROOT/home" \
XDG_CONFIG_HOME="$ROOT/config" \
XDG_CACHE_HOME="$ROOT/cache" \
XDG_DATA_HOME="$ROOT/data" \
XDG_STATE_HOME="$ROOT/state" \
HOARD_NATIVE_TRACE=1 \
HOARD_NATIVE_SMOKE=1 \
"$BIN" "$ROOT/home/testdir" >"$LOG" 2>&1 &
PID=$!

mapped=0
for _ in $(seq 1 100); do
  if ! kill -0 "$PID" 2>/dev/null; then break; fi
  if command -v hyprctl >/dev/null 2>&1; then
    if hyprctl clients -j > "$ROOT/clients.json" 2>/dev/null; then
      if python3 - "$PID" "$ROOT/clients.json" <<'PY'
import json,sys
pid=int(sys.argv[1]); data=json.load(open(sys.argv[2]))
for w in data:
    if int(w.get('pid') or -1)==pid:
        if not w.get('mapped',False): raise SystemExit(2)
        if w.get('xwayland',False): raise SystemExit(3)
        raise SystemExit(0)
raise SystemExit(1)
PY
      then mapped=1; fi
    fi
  else
    mapped=1
  fi
  sleep 0.05
done

for _ in $(seq 1 200); do
  if ! kill -0 "$PID" 2>/dev/null; then break; fi
  sleep 0.05
done
if kill -0 "$PID" 2>/dev/null; then
  echo "native smoke timed out; candidate remained alive without completing smoke contract" >&2
  cat "$LOG" >&2
  exit 1
fi
set +e
wait "$PID"
rc=$?
set -e
[[ $rc -eq 0 ]] || { echo "native smoke exited $rc" >&2; cat "$LOG" >&2; exit "$rc"; }
[[ $mapped -eq 1 ]] || { echo "native Wayland window was not observed as mapped/native" >&2; cat "$LOG" >&2; exit 1; }
grep -F 'HOARD_NATIVE adapter=' "$LOG" >/dev/null || { cat "$LOG" >&2; echo "adapter trace missing" >&2; exit 1; }
grep -F 'backend=Vulkan' "$LOG" >/dev/null || { cat "$LOG" >&2; echo "Vulkan backend trace missing" >&2; exit 1; }
grep -F 'HOARD_NATIVE configure=' "$LOG" >/dev/null || { cat "$LOG" >&2; echo "surface configure trace missing" >&2; exit 1; }
grep -F 'HOARD_NATIVE presented=' "$LOG" >/dev/null || { cat "$LOG" >&2; echo "frame presentation trace missing" >&2; exit 1; }
grep -F 'HOARD_NATIVE_SMOKE PASS' "$LOG" >/dev/null || { cat "$LOG" >&2; echo "application/native smoke pass marker missing" >&2; exit 1; }
echo "HOARD NATIVE RUNTIME SMOKE PASSED"
