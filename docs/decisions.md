# Decision log

Decisions taken while implementing `OILinux_PRD.md`, and the evidence behind them. The PRD's own
decision table (its §14) is the product record; this file is the engineering record of the things
that were *measured* rather than specified.

## D1. Guest → host networking: the direct route does not work on Windows, the reverse tunnel does

**Question (PRD §15.1).** Can the guest reach a host listener bound to `127.0.0.1` through QEMU's
user-mode "host" address `10.0.2.2`, or does the product have to use `guestfwd` or another bridge?

**Measurement.** On a Windows Server 2022 host with QEMU 11.1 and a guest on user-mode networking:

* `ping 10.0.2.2` from the guest succeeds (0% loss),
* a listener on the host bound to `127.0.0.1:45991` receives **no connection** when the guest
  connects to `10.0.2.2:45991`,
* a listener bound to `0.0.0.0:45992` also receives **no connection**.

So on this platform the user-mode host alias is not a usable TCP bridge to host services.

**Decision.** The engine probes the direct route for real on every launch it does not already have
an answer for and records the result, but the product's share transport is the **SSH reverse
tunnel** (`tcpip-forward`) opened over the connection the host already owns. That is one of the
options the PRD explicitly allows (§7.2), needs no inbound listener on the guest, and keeps every
byte inside the authenticated SSH session. `AppConfig::share_transport` remembers which transport
worked so later mounts skip the probe.

The guest's sshd consequently has to allow remote forwarding (`AllowTcpForwarding remote`), which
is the only forwarding direction that is enabled; `GatewayPorts no` keeps the listener on the
guest's loopback interface, and the listener only exists for as long as the host's SSH session
does.

## D2. The seed disk must be read case-insensitively

The boot-time seed is ISO9660 level 1, so it stores `SEED.JSON`, `CLIENT.PUB`, … Linux's `isofs`
driver maps names to lower case by default, and which mapping is in effect depends on the kernel
configuration. The guest therefore resolves every seed file with `find -iname` instead of assuming
a spelling, and identifies the medium by both its label and the presence of `SEED.JSON` rather than
trusting `blkid` alone (busybox's `blkid` option handling differs between builds).

## D3. The training account carries an unusable password, not a locked one

`passwd -l student` marks the account as locked in `/etc/shadow`, and OpenSSH refuses locked
accounts outright — including for public key authentication ("User student not allowed because
account is locked"). The image instead sets a random, unusable password, which keeps password
authentication impossible (sshd refuses it anyway) while leaving the account usable for keys.

## D4. `rootfstype` on the kernel command line is mandatory for this image

The base image's root file system is ext4 and the kernel has ext4 as a module. Alpine's initramfs
mounts the root device with the file system type from the kernel command line; without
`rootfstype=ext4` the mount fails before the module is loaded and the boot drops to the emergency
shell. `vm/rootfs/etc/default/grub` therefore sets
`rootfstype=ext4 modules=ext4 quiet console=tty0 console=ttyS0,115200`.

The serial console is part of the same decision: `console=ttyS0` last makes the serial port
`/dev/console`, which is what puts OpenRC's output into the application's `logs\console.log`.

## D5. The ACL helper must distinguish files from directories

`icacls <file> /inheritance:r /grant:r *SID:(OI)(CI)F` leaves a *file* with an **empty DACL** —
inheritance flags are only meaningful on containers — which denies access to everyone, including
the user who owns it. The helper now grants `(OI)(CI)F` on directories and `F` on files, and
restores inheritance for the children of a restricted directory instead of granting to each child
individually.

## D6. The share service does not cache, and the mount options are probed

`default_permissions` is required for `chmod -x` to be enforced by the kernel, and the content
cache is disabled (`noauto_cache`, `dir_cache=no`) because the correctness requirement is that a
file another program just replaced is visible on the next read. The preferred option set is tried
first and a minimal set is used when the guest's sshfs refuses it; the acceptance suite verifies
the outcome (`execve`, `MAP_SHARED`, `MAP_PRIVATE`, `chmod`, rename and replace semantics) rather
than the option string.

## D7. Permissions follow the file entity, verified through a handle

The metadata key is (volume serial, file index, creation time) read from an open handle, never from
the path the client sent. A rename keeps the entity and therefore the mode; a save that replaces
the file produces a new entity with the Linux default. The tests assert both directions, including
that a Windows-side rename of an executable keeps it executable and a Windows-side replacement
resets it to `0644`.

## D8. A single instance owns a data directory

The engine takes `state\instance.lock` with no sharing when it opens an environment; a second
instance reports the conflict instead of racing on the overlay. This is what makes "two windows
writing one qcow2" impossible, and it is why the acceptance tests can safely assume exclusive
ownership while they run.

## D9. Diagnostics live where the environment lives

Logs are written into `<data dir>\logs` once an environment exists (the PRD's layout) and into the
application folder before that. Exporting logs copies them with user names, absolute paths, key
material and terminal-independent content redacted, because the PRD forbids shipping telemetry or
terminal contents and asks for redaction on export.

## D10. Update channel switching is a configuration change and nothing else

`stable` ⇄ `beta` rewrites one field of `config.json`. It never touches `environment.json`,
`base.qcow2`, the overlay or the backing chain, and a "downgrade" is offered like any other
version difference (the version comparator treats a prerelease as older than its release, so
`beta → stable` is visible as an upgrade while the reverse is offered as a downgrade). The
acceptance suite asserts exactly that.

## D11. One build recipe for both platforms

The PRD suggests `alpine-make-vm-image` for the Linux CI job. That tool produces a BIOS/syslinux
layout, while the launcher boots UEFI whenever the firmware is present — two recipes would drift
and only one of them would be the image that is actually tested. Both `vm/build-base-image.ps1`
(Windows) and `vm/build.sh` (Linux CI) therefore drive the *same* `vm/scripts/install-base.sh`
inside the official Alpine live medium, from the same `vm/packages.txt` and `vm/rootfs/` overlay.
The result is a hybrid image (UEFI GRUB with the removable path, plus a BIOS GRUB), which is what
the acceptance suites boot.

## D12. The host snapshot is cached

`Engine::host_snapshot` runs two probes (a QEMU accelerator probe and an optional-feature query).
The UI asks for state every two seconds, so the result is cached for 30 seconds and invalidated by
`refresh_host` (used after the "enable WHPX" action). Without this the application would spawn a
QEMU process and a PowerShell every tick, which is exactly the background load the PRD's
performance section measures.

## D13. A failed or half-working share cannot absorb writes

If the mount fails, or the mount succeeds but the read/write/permission health check does not, the
mount point is removed and re-created read-only (`chmod 555`). A student who then compiles into
`/home/student/workspace` gets a clear permission error instead of silently filling the guest disk
with a second copy of their sources (`OILinux_PRD.md` §7.4).

## The PRD's key assumptions (§15) and what the implementation did about them

| Assumption | Outcome |
|---|---|
| 1. Guest → host channel through QEMU user networking | **Measured, and it does not work on Windows**: `10.0.2.2` does not reach a host listener bound to either `127.0.0.1` or `0.0.0.0`. The product uses the SSH reverse tunnel, which the PRD allows, and probes the direct route honestly so the finding stays visible (D1). |
| 2. Executable permissions together with FUSE caching | **Verified**: `default_permissions` plus `attr_timeout=0`/`entry_timeout=0`/`noauto_cache` give `chmod ±x`, `execve` and `MAP_SHARED`/`MAP_PRIVATE` at the same time; the `chmod-exec` suite asserts each of them from the shared directory. |
| 3. SFTP rename and Windows file locks | **Verified**: `posix-rename@openssh.com` replaces the destination; a file held open with no sharing produces an explicit error and the original content survives. |
| 4. TCG performance and 8 GiB hosts | **Measured** by `perf-baseline` on a 2 vCPU / 8 GiB cloud machine (see its report); the product never promises WHPX-speed builds on TCG. |
| 5. WHPX fallback, Job Objects, QMP timeouts | **Verified**: the accelerator probe falls back to TCG with the actual error recorded (`WinHvPlatform.dll` missing here); the crash test leaves no QEMU writing to the overlay; every QMP call has a timeout. |
| 6. Installer dependencies | The bundle is configured for the WebView2 **offline** installer, QEMU and the base image are staged by `scripts/stage-resources.ps1`, and the licence inventory ships as data. The NSIS package itself is produced by the release workflow. |

