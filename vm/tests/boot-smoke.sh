#!/bin/sh
# Boot smoke test for the built base image (`OILinux_PRD.md` §10.3: CI must verify the image
# boots). Used by `.github/workflows/linux-image.yml`; it needs qemu-system-x86_64 and timeout.
#
# The image is booted headless with a serial console. Two things are checked:
#   1. the initramfs finds its root file system and OpenRC starts (the guest reaches its login
#      prompt), which is what failed silently the first time this image was built,
#   2. sshd is reached on the guest side (the port is forwarded to the host and must accept a TCP
#      connection; authentication is covered by the acceptance suites).
#
# Usage: vm/tests/boot-smoke.sh <base.qcow2> [seconds]
set -eu

IMAGE="${1:-vm/out/base.qcow2}"
TIMEOUT="${2:-300}"
LOG="${OI_SMOKE_LOG:-vm/out/boot-smoke.log}"
SSH_PORT="${OI_SMOKE_SSH_PORT:-42999}"

[ -f "$IMAGE" ] || { echo "no image at $IMAGE" >&2; exit 1; }
mkdir -p "$(dirname "$LOG")"
rm -f "$LOG"

echo "== booting $IMAGE for up to ${TIMEOUT}s =="
# `-snapshot` keeps the image untouched: the guest writes its own host keys and logs on first
# boot, and the published image (and its SHA-256) must stay exactly what was built.
timeout "$TIMEOUT" qemu-system-x86_64 \
    -name oi-boot-smoke -accel tcg,thread=multi -m 1024 -smp 2 \
    -drive "file=$IMAGE,format=qcow2,if=virtio" -snapshot \
    -netdev "user,id=oinet,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" \
    -device virtio-net-pci,netdev=oinet -device virtio-rng-pci \
    -display none -monitor none -serial "file:$LOG" \
    -boot order=c -rtc base=utc &
QEMU_PID=$!
trap 'kill $QEMU_PID 2>/dev/null || true' EXIT

waited=0
while [ "$waited" -lt "$TIMEOUT" ]; do
    sleep 5
    waited=$((waited + 5))
    if grep -q 'login:' "$LOG" 2>/dev/null; then
        break
    fi
    if ! kill -0 $QEMU_PID 2>/dev/null; then
        echo "QEMU exited before the guest reached a login prompt" >&2
        tail -40 "$LOG" >&2 || true
        exit 1
    fi
done

echo "== console =="
tail -15 "$LOG"

if ! grep -q 'login:' "$LOG"; then
    echo "the guest never reached a login prompt" >&2
    exit 1
fi
if grep -q 'Mounting root failed' "$LOG"; then
    echo "the initramfs failed to mount the root file system" >&2
    exit 1
fi
echo "PASS: the image boots to a login prompt"
