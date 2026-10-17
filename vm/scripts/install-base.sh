#!/bin/sh
# OI Linux base image installer.
#
# Runs as root inside the Alpine live medium (booted from `alpine-standard-*.iso` with the
# console on a serial port) and installs the immutable base system onto $OI_DISK. This is the
# Windows-side equivalent of the `alpine-make-vm-image` job used by CI: the same branch, the same
# pinned package list, the same rootfs overlay and the same post-install hardening.
#
# The result boots under UEFI (GRUB EFI, removable path) *and* BIOS (GRUB i386-pc), contains no
# SSH host keys and no cloud-init, and applies its per-machine identities from the boot-time seed
# disk written by the application.
#
# Usage: sh install-base.sh [mirror] [branch]

set -eu

MIRROR="${1:-https://mirrors.tuna.tsinghua.edu.cn/alpine}"
BRANCH="${2:-v3.24}"
DISK="${OI_DISK:-/dev/vda}"
MNT="${OI_MNT:-/mnt}"
KEYSDIR=/etc/apk/keys
OVERLAY=/root/oi-rootfs

say() { printf '\n[oi] %s\n' "$*"; }
die() { printf '\n[oi] ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------------------------
say "checking the live environment"
[ "$(id -u)" = "0" ] || die "must run as root"
[ -b "$DISK" ] || die "$DISK is not a block device"
[ -d "$OVERLAY" ] || die "the rootfs overlay is missing from $OVERLAY (upload vm/rootfs first)"

# ---------------------------------------------------------------------------------------------
say "bringing up the network"
ip link set eth0 up 2>/dev/null || true
udhcpc -i eth0 -q -n -t 20 >/dev/null 2>&1 || true
if ! ip -4 addr show eth0 | grep -q 'inet '; then
    die "eth0 did not get an address from QEMU's user-mode network"
fi
ip -4 addr show eth0 | grep 'inet '

# ---------------------------------------------------------------------------------------------
say "installing host tooling"
# The live medium's own repository only carries the base system and the installer scripts, so the
# network mirror is added for everything else. Both are kept: the ISO packages stay available as
# a fallback if the mirror is slow.
{
    [ -d /media/cdrom/apks ] && echo "/media/cdrom/apks"
    echo "$MIRROR/$BRANCH/main"
    echo "$MIRROR/$BRANCH/community"
} > /etc/apk/repositories
cat /etc/apk/repositories
apk update >/dev/null
apk add --no-cache e2fsprogs e2fsprogs-extra sfdisk dosfstools grub grub-bios grub-efi bash vim >/dev/null

# ---------------------------------------------------------------------------------------------
say "partitioning $DISK"
umount "$MNT"/boot/efi 2>/dev/null || true
umount "$MNT" 2>/dev/null || true
swapoff -a 2>/dev/null || true
for part in "${DISK}"[0-9]* ; do
    [ -b "$part" ] || continue
    swapoff "$part" 2>/dev/null || true
done
wipefs -a "$DISK" >/dev/null 2>&1 || true
sfdisk "$DISK" <<'PARTITIONS'
label: gpt
size=200MiB, type=U, name=ESP
size=1MiB,  type=21686148-6449-6E6F-744E-656564454649, name=BIOS
type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=root
PARTITIONS
# The live medium runs busybox mdev rather than udev, so the partition nodes have to be
# re-created explicitly after the table changes.
blockdev --rereadpt "$DISK" >/dev/null 2>&1 || true
mdev -s >/dev/null 2>&1 || true
waited=0
while [ "$waited" -lt 30 ]; do
    [ -b "${DISK}1" ] && [ -b "${DISK}3" ] && break
    sleep 1
    mdev -s >/dev/null 2>&1 || true
    waited=$((waited + 1))
done
[ -b "${DISK}1" ] || die "the ESP was not created"
[ -b "${DISK}3" ] || die "the root partition was not created"
ls -l "${DISK}1" "${DISK}2" "${DISK}3"

# ---------------------------------------------------------------------------------------------
say "creating file systems"
# The live medium only loads the modules it needs to boot, so the file system modules have to be
# pulled in from the module loop before anything can be mounted.
modprobe ext4 2>/dev/null || true
modprobe vfat 2>/dev/null || true
modprobe fuse 2>/dev/null || true
mkfs.vfat -F32 -n OISYS "${DISK}1" >/dev/null
mkfs.ext4 -F -L oiroot -q "${DISK}3"
ROOT_UUID=$(blkid "$DISK"3 2>/dev/null | sed -n 's/.*UUID="\([^"]*\)".*/\1/p' | head -n1)
ESP_UUID=$(blkid "$DISK"1 2>/dev/null | sed -n 's/.*UUID="\([^"]*\)".*/\1/p' | head -n1)
[ -n "$ROOT_UUID" ] || die "could not read the root file system UUID"
[ -n "$ESP_UUID" ] || die "could not read the ESP UUID"
mount -t ext4 "${DISK}3" "$MNT"
mkdir -p "$MNT/boot/efi"
mount -t vfat "${DISK}1" "$MNT/boot/efi"
say "root UUID $ROOT_UUID, ESP UUID $ESP_UUID"

# ---------------------------------------------------------------------------------------------
say "configuring the package repositories"
mkdir -p "$MNT/etc/apk/keys"
cp -a "$KEYSDIR/." "$MNT/etc/apk/keys/"
{
    echo "$MIRROR/$BRANCH/main"
    echo "$MIRROR/$BRANCH/community"
} > "$MNT/etc/apk/repositories"
cat "$MNT/etc/apk/repositories"

# ---------------------------------------------------------------------------------------------
say "installing the base system (this takes a while under TCG)"
PACKAGES="alpine-base linux-virt openssh sshfs fuse3 sudo bash coreutils time git build-base \
gcc g++ gdb vim nano less tzdata ca-certificates e2fsprogs e2fsprogs-extra dosfstools file \
grub grub-bios grub-efi"
# shellcheck disable=SC2086
apk --root "$MNT" --initdb --arch x86_64 --keys-dir "$KEYSDIR" \
    --repositories-file "$MNT/etc/apk/repositories" \
    add --no-cache $PACKAGES

# ---------------------------------------------------------------------------------------------
say "overlaying the OI Linux system files"
# every file the product depends on lives in `vm/rootfs/` so that the image and the application
# cannot drift apart
for rel in etc/init.d/oi-seed etc/ssh/sshd_config etc/sudoers.d/student etc/modules \
           etc/network/interfaces etc/motd etc/hostname etc/fstab etc/default/grub etc/inittab \
           usr/libexec/oi/apply-seed.sh; do
    if [ -f "$OVERLAY/$rel" ]; then
        mkdir -p "$MNT/$(dirname "$rel")"
        cp "$OVERLAY/$rel" "$MNT/$rel"
    fi
done
chmod 755 "$MNT/etc/init.d/oi-seed" "$MNT/usr/libexec/oi/apply-seed.sh"
chmod 600 "$MNT/etc/ssh/sshd_config"
chmod 440 "$MNT/etc/sudoers.d/student"

# ---------------------------------------------------------------------------------------------
say "entering the target for configuration"
# The pseudo file systems have to be available before anything runs inside the chroot (`mkinitfs`
# looks at /proc and /sys).
mount --bind /dev "$MNT/dev"
mount -t proc proc "$MNT/proc"
mount -t sysfs sys "$MNT/sys"
cp /etc/resolv.conf "$MNT/etc/resolv.conf" 2>/dev/null || true

say "generating the initramfs"
for kdir in "$MNT"/lib/modules/*; do
    kv=$(basename "$kdir")
    chroot "$MNT" /sbin/mkinitfs "$kv"
done
ls -l "$MNT"/boot

# ---------------------------------------------------------------------------------------------
say "configuring the guest"
cat > "$MNT/etc/fstab" <<FSTAB
UUID=$ROOT_UUID  /          ext4  defaults,noatime  0 1
UUID=$ESP_UUID   /boot/efi  vfat  defaults          0 2
FSTAB

chroot "$MNT" /bin/sh -s <<'GUESTSETUP'
set -eu

# --- the training account ---------------------------------------------------------------
if ! id student >/dev/null 2>&1; then
    adduser -D -g "OI Linux student" -s /bin/bash student
fi
# An unusable random password, but *not* a locked account: OpenSSH refuses accounts whose
# shadow entry is locked, which would block the public key authentication the product relies on.
UNUSABLE_PW="$(head -c 32 /dev/urandom | base64 | tr -d '\n')"
echo "student:$UNUSABLE_PW" | chpasswd
addgroup student wheel 2>/dev/null || true
mkdir -p /home/student/.ssh /home/student/workspace
chmod 700 /home/student/.ssh
chown -R student:student /home/student

mkdir -p /etc/sudoers.d
chmod 750 /etc/sudoers.d

# --- OpenRC runlevels (the same set `setup-alpine` creates) -----------------------------
add_runlevel() {
    level="$1"; shift
    for svc in "$@"; do
        rc-update add "$svc" "$level" >/dev/null 2>&1 || true
    done
}
add_runlevel sysinit devfs dmesg mdev hwdrivers
add_runlevel boot modules sysctl hostname bootmisc syslog networking urandom oi-seed
add_runlevel default sshd local
add_runlevel shutdown mount-ro killprocs savecache

# --- make sure the workspace mount point exists before the share can appear --------------
mkdir -p /home/student/workspace
chmod 755 /home/student/workspace
chown student:student /home/student/workspace

# --- keep the locale sane for UTF-8 terminals -------------------------------------------
echo "export LANG=en_US.UTF-8" > /etc/profile.d/oi-locale.sh
chmod 644 /etc/profile.d/oi-locale.sh

# --- a first-login hint that matches the product's advice -------------------------------
mkdir -p /etc/profile.d
cat > /etc/profile.d/oi-hint.sh <<'HINT'
if [ -n "${PS1:-}" ] && [ "${OI_HINT_SHOWN:-}" != "1" ]; then
    export OI_HINT_SHOWN=1
    if [ -d /home/student/workspace ]; then
        cd /home/student/workspace 2>/dev/null || true
    fi
fi
HINT
chmod 644 /etc/profile.d/oi-hint.sh

sync
GUESTSETUP

# ---------------------------------------------------------------------------------------------
say "installing the boot loaders"
# UEFI: the removable path is what a machine without NVRAM entries (and our launcher) uses.
chroot "$MNT" grub-install --target=x86_64-efi --efi-directory=/boot/efi --boot-directory=/boot \
    --bootloader-id=oi-linux --removable --no-nvram
# BIOS: keeps the image usable on machines where the firmware configuration prefers it.
chroot "$MNT" grub-install --target=i386-pc --boot-directory=/boot "$DISK"
chroot "$MNT" grub-mkconfig -o /boot/grub/grub.cfg

# ---------------------------------------------------------------------------------------------
say "cleaning up"
umount "$MNT/proc" "$MNT/sys" 2>/dev/null || true
umount "$MNT/dev" 2>/dev/null || true
# The base image must not carry reusable credentials (`OILinux_PRD.md` §5.3).
rm -f "$MNT"/etc/ssh/ssh_host_*
rm -f "$MNT"/etc/resolv.conf
rm -rf "$MNT"/var/cache/apk/*
rm -rf "$MNT"/var/lib/sshd
rm -f "$MNT"/root/.ash_history "$MNT"/root/.viminfo
rm -f "$MNT"/tmp/*
umount "$MNT/boot/efi"
umount "$MNT"

say "base image finished"
sync
echo "OI-INSTALL-OK"
