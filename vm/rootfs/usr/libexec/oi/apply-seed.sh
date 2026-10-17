#!/bin/sh
# OI Linux guest side of the boot-time seed contract (`OILinux_PRD.md` §5.3).
#
# The host attaches a small read-only ISO9660 medium carrying the per-machine SSH identities
# and the current share endpoints. This script consumes it on every boot: the endpoints of the
# host share service change on each launch, so the trust anchors have to be refreshed together
# with them. Nothing here is a resident daemon - the script runs once and exits.
#
# It is deliberately tolerant: a missing or damaged seed leaves the existing configuration in
# place instead of leaving the guest without sshd.

set -u

SEED_LABEL="OI_SEED"
STUDENT_USER="student"
STUDENT_HOME="/home/student"
SEED_MNT="/run/oi-seed"
STATE_DIR="/var/lib/oi"
HOST_KEY="/etc/ssh/ssh_host_ed25519_key"

log() {
    printf 'oi-seed: %s\n' "$*"
    # also send it to the kernel log so that it shows up on the serial console
    printf 'oi-seed: %s\n' "$*" > /dev/kmsg 2>/dev/null || true
}

# The ISO stores level-1 names (`SEED.JSON`). Linux's isofs driver maps names to lower case by
# default, and which mapping is in effect depends on the kernel configuration, so every lookup
# is case-insensitive instead of assuming one spelling.
seed_path() {
    find "$SEED_MNT" -maxdepth 1 -iname "$1" 2>/dev/null | head -n1
}

have_seed_files() {
    [ -n "$(seed_path "SEED.JSON")" ]
}

# Is `dev` the seed medium? The label is checked first, but the authoritative test is whether the
# read-only medium actually carries our files: `blkid` in the guest is busybox and its option
# handling differs between builds, so a label lookup alone is not enough.
is_seed_device() {
    dev="$1"
    label="$(blkid "$dev" 2>/dev/null | sed -n 's/.*LABEL="\([^"]*\)".*/\1/p')"
    if [ "$label" = "$SEED_LABEL" ]; then
        return 0
    fi
    mkdir -p "$SEED_MNT"
    if mount -t iso9660 -o ro "$dev" "$SEED_MNT" 2>/dev/null; then
        if have_seed_files; then
            return 0
        fi
        umount "$SEED_MNT" 2>/dev/null || true
    fi
    return 1
}

find_seed_device() {
    modprobe isofs 2>/dev/null || true
    tries=0
    while [ "$tries" -lt 40 ]; do
        for dev in /dev/vd[a-z] /dev/sd[a-z]; do
            [ -b "$dev" ] || continue
            if is_seed_device "$dev"; then
                printf '%s' "$dev"
                return 0
            fi
        done
        tries=$((tries + 1))
        sleep 1
    done
    return 1
}

seed_dev="$(find_seed_device || true)"
if [ -z "$seed_dev" ]; then
    log "no read-only seed medium labelled $SEED_LABEL was found; keeping the existing configuration"
    exit 0
fi
log "seed medium found on $seed_dev"

mkdir -p "$SEED_MNT"
if ! mountpoint -q "$SEED_MNT"; then
    if ! mount -t iso9660 -o ro "$seed_dev" "$SEED_MNT" 2>/dev/null; then
        log "the seed medium could not be mounted; keeping the existing configuration"
        exit 0
    fi
fi

mkdir -p "$STATE_DIR" "$STUDENT_HOME/.ssh"

# --- guest sshd host key ---------------------------------------------------------------
# Injected rather than generated so that the Windows client can pin it from the very first
# connection instead of trusting whatever answers first.
guest_key="$(seed_path "GUESTKEY")"
if [ -n "$guest_key" ]; then
    install -m 600 -o root -g root "$guest_key" "$HOST_KEY.oi-new"
    if ! cmp -s "$HOST_KEY.oi-new" "$HOST_KEY" 2>/dev/null; then
        mv "$HOST_KEY.oi-new" "$HOST_KEY"
        log "guest sshd host key updated from the seed medium"
    else
        rm -f "$HOST_KEY.oi-new"
    fi
fi

# Keep the public half in step with the private key: a left-over `.pub` from an earlier boot
# would only produce "public key does not match private key" warnings from sshd.
if [ -f "$HOST_KEY" ]; then
    if ssh-keygen -y -f "$HOST_KEY" > "$HOST_KEY.pub" 2>/dev/null; then
        chmod 644 "$HOST_KEY.pub"
    fi
fi

# --- host -> guest authentication ------------------------------------------------------
client_key="$(seed_path "CLIENT.PUB")"
if [ -n "$client_key" ]; then
    install -m 600 -o "$STUDENT_USER" -g "$STUDENT_USER" \
        "$client_key" "$STUDENT_HOME/.ssh/authorized_keys"
    log "authorized_keys refreshed for $STUDENT_USER"
fi

# --- guest -> host share service -------------------------------------------------------
# The guest authenticates to the Windows SFTP service with its own identity; the public half
# of it is what the host authorises.
sftp_key="$(seed_path "SFTPKEY")"
if [ -n "$sftp_key" ]; then
    install -m 600 -o "$STUDENT_USER" -g "$STUDENT_USER" \
        "$sftp_key" "$STUDENT_HOME/.ssh/id_ed25519"
fi

known_hosts_file="$(seed_path "WINHOSTS")"
if [ -n "$known_hosts_file" ]; then
    install -m 600 -o "$STUDENT_USER" -g "$STUDENT_USER" \
        "$known_hosts_file" "$STUDENT_HOME/.ssh/known_hosts"
    log "host share service endpoints pinned in known_hosts"
fi

seed_json="$(seed_path "SEED.JSON")"
if [ -n "$seed_json" ]; then
    install -m 600 -o root -g root "$seed_json" "$STATE_DIR/seed.json"
fi

# --- fuse / sshfs prerequisites --------------------------------------------------------
# sshfs runs as the unprivileged student user, so the FUSE device has to be usable by them.
if ! lsmod 2>/dev/null | grep -q '^fuse'; then
    modprobe fuse 2>/dev/null || true
fi
if [ -e /dev/fuse ]; then
    chmod 666 /dev/fuse
fi

umount "$SEED_MNT" 2>/dev/null || true

generation="$(sed -n 's/.*"generation": *\([0-9]*\).*/\1/p' "$STATE_DIR/seed.json" 2>/dev/null | head -n1)"
log "seed configuration applied (generation ${generation:-unknown})"
exit 0
