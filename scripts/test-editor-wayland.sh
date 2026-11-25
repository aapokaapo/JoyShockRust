#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

weston=${WESTON:-weston}
command -v "$weston" >/dev/null
command -v dbus-run-session >/dev/null

if [[ -z ${JOYSHOCKRUST_TEST_BRIDGE:-} ]]; then
    cargo build --locked --offline -p joyshockrust-linux
    JOYSHOCKRUST_TEST_BRIDGE="${CARGO_TARGET_DIR:-target}/debug/joyshockrust-linux"
fi
if [[ ! -x $JOYSHOCKRUST_TEST_BRIDGE ]]; then
    printf 'Bridge executable unavailable at %s\n' "$JOYSHOCKRUST_TEST_BRIDGE" >&2
    exit 1
fi
export JOYSHOCKRUST_TEST_BRIDGE
JOYSHOCKRUST_TEST_BRIDGE="$(realpath -- "$JOYSHOCKRUST_TEST_BRIDGE")"

# GTK must initialize on the same thread that runs each test. Use a fresh
# test process per case, including when libtest is set to one thread.
test_list="$(cargo test --locked --offline -p joyshockrust-editor -- --ignored --list)"
mapfile -t tests < <(sed -n 's/^\(.*\): test$/\1/p' <<< "$test_list")
if (( ${#tests[@]} == 0 )); then
    printf 'No ignored editor tests were discovered.\n' >&2
    exit 1
fi

runtime="$(mktemp -d "${TMPDIR:-/tmp}/joyshockrust-wayland.XXXXXX")"
weston_pid=
cleanup() {
    if [[ -n $weston_pid ]]; then
        kill "$weston_pid" 2>/dev/null || true
        wait "$weston_pid" 2>/dev/null || true
    fi
    rm -rf -- "$runtime"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$runtime"
export WAYLAND_DISPLAY=joyshockrust-test
export GDK_BACKEND=wayland GSK_RENDERER=cairo GTK_A11Y=none
"$weston" --backend=headless-backend.so --use-pixman --shell=kiosk-shell.so \
    --socket="$WAYLAND_DISPLAY" --width=1152 --height=750 --no-config \
    --idle-time=0 --log="$runtime/weston.log" &
weston_pid=$!

for ((attempt = 0; attempt < 50; attempt++)); do
    [[ -S $runtime/$WAYLAND_DISPLAY ]] && break
    kill -0 "$weston_pid" 2>/dev/null || break
    sleep 0.1
done
if [[ ! -S $runtime/$WAYLAND_DISPLAY ]]; then
    printf 'Weston did not create its Wayland socket.\n' >&2
    cat "$runtime/weston.log" >&2 || true
    exit 1
fi

for test in "${tests[@]}"; do
    dbus-run-session -- cargo test --locked --offline -p joyshockrust-editor \
        -- --ignored --exact "$test"
done
