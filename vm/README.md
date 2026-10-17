# Building the Alpine base image

The image that ships with OI Linux is a normal Alpine installation with the toolchain
pre-installed, the per-machine identity contract of `OILinux_PRD.md` §5.3 and **no reusable
credentials**. It is the only source of `base.qcow2` for new environments and resets; application
updates never replace an existing environment's base image.

```text
vm/
├── packages.txt              pinned package list (§5.2)
├── rootfs/                   files installed into the image (sshd config, seed consumer, inittab …)
├── scripts/install-base.sh   runs inside the Alpine live medium and installs everything
├── scripts/serial-console.py drives the live medium over a serial console
├── scripts/guest-run.py      small helper for inspecting a guest during development
├── build-base-image.ps1      Windows driver (used by windows-integration CI)
├── build.sh                  Linux driver (used by linux-image CI)
└── tests/boot-smoke.sh       headless boot check used by CI
```

Both drivers do the same thing:

1. boot the official `alpine-standard-*.iso` **kernel** under QEMU (TCG when no accelerator is
   available) with the console on a serial port,
2. upload `rootfs/` plus `install-base.sh` and run the installer,
3. the installer partitions the target disk (ESP + BIOS-boot + ext4 root), installs
   `packages.txt` from the Alpine mirror, applies the overlay, creates the `student` account
   (unusable random password, passwordless sudo), generates the initramfs, installs GRUB for UEFI
   (removable path) and BIOS, and removes every credential that must not ship,
4. publish `base.qcow2` plus its SHA-256 sidecar.

The kernel command line the image boots with is part of the contract:

```text
rootfstype=ext4 modules=ext4 quiet console=tty0 console=ttyS0,115200
```

`rootfstype` is what lets the initramfs mount the root file system before the ext4 module is
loaded; the serial console is what puts OpenRC's output into the application's
`logs\console.log`.

## The boot-time seed disk

The application attaches a small read-only ISO9660 medium (volume label `OI_SEED`) on every
launch. `rootfs/usr/libexec/oi/apply-seed.sh`, started by the `oi-seed` OpenRC service before
`sshd`, installs from it:

| File | Purpose |
|---|---|
| `GUESTKEY` | the guest sshd host private key, generated on Windows for this machine |
| `CLIENT.PUB` | the host's public key, written to `student`'s `authorized_keys` |
| `SFTPKEY` | the guest's private key for the host share service |
| `WINHOSTS` | pinned `known_hosts` lines for the host share endpoint |
| `SEED.JSON` | endpoint metadata, kept for diagnostics |

The script runs on every boot because the share endpoints change on every launch, and it resolves
file names case-insensitively: the ISO stores level-1 names and Linux's `isofs` maps them to lower
case by default.

## Reproducing the shipped image

```powershell
# Windows
powershell -ExecutionPolicy Bypass -File vm\build-base-image.ps1 -Iso D:\alpine-standard-3.24.2-x86_64.iso
```

```bash
# Linux (needs root for the loop mount of the ISO)
sudo -E bash vm/build.sh vm/out
```

Both write `vm/out/base.qcow2` and `vm/out/base.qcow2.sha256`. The acceptance suites copy that
image into their environment; they never modify it.
