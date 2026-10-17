#!/bin/sh
# Builds the Alpine base image on Linux CI (`OILinux_PRD.md` §10.3).
#
# This is the exact counterpart of `vm\build-base-image.ps1`: it boots the official Alpine
# "standard" ISO's kernel with a serial console, then runs `vm/scripts/install-base.sh` inside the
# guest. Both platforms therefore produce the same image from the same package list and the same
# `vm/rootfs/` overlay — there is no second, subtly different build recipe.
#
# Requirements on the runner: qemu-system-x86_64, python3, the ISO (downloaded and checksum
# verified below) and root (for the loop mount of the ISO).
#
# Usage: vm/build.sh [output-dir]
set -eu

OUT="${1:-vm/out}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BRANCH="${OI_ALPINE_BRANCH:-v3.24}"
RELEASE="${OI_ALPINE_RELEASE:-3.24.2}"
MIRROR="${OI_ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
SERIAL_PORT="${OI_SERIAL_PORT:-44600}"
WORK="${OI_BUILD_DIR:-$REPO/vm/build}"

mkdir -p "$OUT" "$WORK/images" "$WORK/logs"

ISO="$WORK/alpine-standard-$RELEASE-x86_64.iso"
if [ ! -f "$ISO" ]; then
    echo "== downloading the Alpine standard ISO ($RELEASE) =="
    curl -fL --retry 3 -o "$ISO" \
        "$MIRROR/$BRANCH/releases/x86_64/alpine-standard-$RELEASE-x86_64.iso"
fi
echo "== ISO =="
sha256sum "$ISO"

echo "== extracting the kernel and initramfs =="
mkdir -p "$WORK/iso"
if mountpoint -q "$WORK/iso"; then umount "$WORK/iso"; fi
mount -o loop,ro "$ISO" "$WORK/iso"
cp "$WORK/iso/boot/vmlinuz-lts" "$WORK/images/vmlinuz-lts"
cp "$WORK/iso/boot/initramfs-lts" "$WORK/images/initramfs-lts"
umount "$WORK/iso"

echo "== creating the target disk =="
DISK="$WORK/images/base-building.qcow2"
rm -f "$DISK"
qemu-img create -f qcow2 "$DISK" 8G

echo "== booting the live medium =="
qemu-system-x86_64 \
    -name oi-image-build -accel tcg,thread=multi -m 2048 -smp 2 \
    -kernel "$WORK/images/vmlinuz-lts" -initrd "$WORK/images/initramfs-lts" \
    -append 'modules=loop,squashfs,sd-mod,usb-storage alpine_dev=cdrom console=tty0 console=ttyS0,115200' \
    -cdrom "$ISO" \
    -drive "file=$DISK,format=qcow2,if=virtio,cache=writeback" \
    -netdev user,id=oinet -device virtio-net-pci,netdev=oinet -device virtio-rng-pci \
    -serial "tcp:127.0.0.1:$SERIAL_PORT,server=on,wait=off" \
    -display none -monitor none -rtc base=utc &
QEMU_PID=$!
trap 'kill $QEMU_PID 2>/dev/null || true' EXIT

sleep 5
DRIVER="$REPO/vm/scripts/serial-console.py"
python3 "$DRIVER" --port "$SERIAL_PORT" --first-login --wait-for '#' \
    --command 'echo live-ready' --output-seconds 10 >/dev/null

echo "== uploading the installer =="
tar -czf "$WORK/oi-payload.tar.gz" -C "$REPO/vm" rootfs scripts/install-base.sh
python3 "$DRIVER" --port "$SERIAL_PORT" --upload "$WORK/oi-payload.tar.gz" \
    --upload-to /root/oi-payload.tar.gz
python3 "$DRIVER" --port "$SERIAL_PORT" --command \
    "rm -rf /root/oi-rootfs /root/scripts && tar -xzf /root/oi-payload.tar.gz -C /root && \
     mv /root/rootfs /root/oi-rootfs && cp /root/scripts/install-base.sh /root/ && \
     sh /root/install-base.sh $MIRROR $BRANCH > /root/install.log 2>&1; echo INSTALL-EXIT=\$?" \
    --output-seconds 3600 | tee "$OUT/install.log"

if ! grep -q 'OI-INSTALL-OK' "$OUT/install.log"; then
    echo "the installation did not finish; see $OUT/install.log" >&2
    exit 1
fi

echo "== shutting the live medium down =="
python3 "$DRIVER" --port "$SERIAL_PORT" --command 'poweroff -f' --output-seconds 20 >/dev/null || true
wait $QEMU_PID 2>/dev/null || true

echo "== publishing =="
qemu-img convert -O qcow2 "$DISK" "$OUT/base.qcow2"
qemu-img check "$OUT/base.qcow2"
sha256sum "$OUT/base.qcow2" | cut -d' ' -f1 > "$OUT/base.qcow2.sha256"
echo "base image: $OUT/base.qcow2"
cat "$OUT/base.qcow2.sha256"
