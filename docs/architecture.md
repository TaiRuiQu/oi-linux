# Architecture

This document describes how OI Linux is put together and why. `OILinux_PRD.md` is the normative
specification; what follows is the implementation view of it.

## The one rule everything follows

> The Windows NTFS workspace directory is where the user's important files live. The Alpine
> virtual machine is a rebuildable training environment.

Every design decision below exists to keep that true: the workspace is never created, moved,
rewritten or deleted by the application, and the Linux side can be thrown away and rebuilt at any
time without touching it.

## Processes and boundaries

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ Windows host                                                                │
│                                                                             │
│  ┌──────────────────────────────┐        ┌────────────────────────────────┐ │
│  │ Tauri shell (src-tauri)      │        │ Host share service (rust)      │ │
│  │  window, command surface,    │        │  SSH server, SFTP subsystem    │ │
│  │  event bus, engine ownership │        │  bound to 127.0.0.1:<random>   │ │
│  └──────────────┬───────────────┘        └───────────────▲────────────────┘ │
│                 │ owns                                   │ SFTP             │
│  ┌──────────────▼───────────────┐                        │ (loopback or     │
│  │ oi-core engine               │                        │  reverse tunnel) │
│  │  VM lifecycle, QMP, storage, │        ┌───────────────┴────────────────┐ │
│  │  SSH client, seed disk, logs │        │ NTFS workspace directory       │ │
│  └──────────────┬───────────────┘        │ D:\OIWorkspace\…               │ │
│                 │ spawns                 └───────────────▲────────────────┘ │
│  ┌──────────────▼───────────────┐                        │ sshfs mount      │
│  │ QEMU (WHPX or TCG)           │        ┌───────────────┴────────────────┐ │
│  │  Job Object kill-on-close    │        │ Alpine Linux guest             │ │
│  │  loopback-only QMP + hostfwd │◄──────►│  sshd, bash, g++, gdb, sshfs   │ │
│  └──────────────────────────────┘  SSH   │  /home/student/workspace       │ │
│                                  hostfwd└────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

There is no daemon and no service: the Tauri process owns everything. When it dies, the Job Object
takes QEMU with it, so nothing keeps writing to the qcow2 overlay behind the user's back.

## Crates and modules

| Module | Responsibility |
|---|---|
| `oi_core::vm` | QEMU argument construction, WHPX/TCG selection, QMP client, Job Object, lifecycle FSM, dynamic port allocation, seed disk |
| `oi_core::share` | Host SFTP service: SSH server, path containment, Windows file identity, simulated POSIX modes, SQLite metadata |
| `oi_core::ssh` | Guest SSH client (command execution and PTY sessions) and the share transport bridge |
| `oi_core::storage` | Data directory layout, qcow2 handling, initialisation/re-association/reset, keys, settings, failure counter |
| `oi_core::engine` | The orchestration layer the UI and the acceptance tests both drive |
| `oi_core::updater` | Stable/Beta channels, version comparison (including downgrades), signature verification |
| `oi_core::logging` | Rolling local logs, retention, redaction on export |
| `src-tauri` | Window, command surface, event wiring, external tool launchers |

`oi-core` is deliberately GUI-free so that `integration-tests/` exercises the *production* code
paths without launching the shell.

## Lifecycle

```text
UNINITIALIZED -> STOPPED -> STARTING -> RUNNING -> STOPPING -> STOPPED
                                   \-> START_ERROR
RUNNING + NotMounted | Mounting | Mounted | Degraded | Recovering | Error
```

* Opening the window only shows the management page; Linux starts when the user asks.
* Starting: bind the share listener (its port goes into the seed disk) → spawn QEMU → wait for
  QMP → wait for SSH → mount the workspace → verify with a real read/write/execute test.
* A share failure is reported separately and never turns into a VM failure; Linux stays usable and
  the mount is retried with a 1/2/4/8/16/30 second backoff, plus a manual retry.
* Stopping: `poweroff` inside the guest over SSH → QMP `system_powerdown` → and only after an
  explicit user confirmation, a forced termination. A guest-initiated `reboot` is allowed; the user
  reconnects manually.

## Storage model

```text
D:\OI-Linux-Data\
├── environment.json   schema, image identity, base hash, creation info
├── base.qcow2         immutable for the life of the environment
├── user.qcow2         the writable overlay, backing file "base.qcow2" (relative)
├── config.json        resources, workspace mapping, channel, accelerator preference
├── ssh\               both directions' identities and pinned host keys (owner-only ACL)
├── metadata.db        simulated POSIX modes, keyed by verified NTFS file identity
├── logs\              rolling files with 7-day / 32 MiB retention
└── state\             single-instance lock, startup failure counter, QMP socket, seed disk
```

A reset prepares `user.new.qcow2`, verifies its chain, switches atomically, verifies again and only
then removes the previous overlay. The base image and the workspace are not touched.

## Accelerator selection

`qemu -accel whpx` is started with a 64 MiB probe machine and observed for ~0.9 s. It is used only
if it stays alive; otherwise TCG (`-accel tcg,thread=multi`) is used. Nothing about the product
depends on the outcome, and a failed WHPX probe is never counted as a start failure.

## Networking

* One QEMU user-mode NIC, one `hostfwd` rule, bound to `127.0.0.1` with a random port.
* QMP on a second random loopback port.
* The guest reaches the host share service either directly (`10.0.2.2:<share port>`, probed for
  real) or through an SSH reverse tunnel requested by the host (`tcpip-forward`), which needs no
  inbound listener on the guest at all.
* Nothing is ever bound to a LAN-reachable address, on either side.

## The toolchain

The Alpine base image carries `build-base`, `gcc`, `g++`, `gdb`, `git`, `bash`, `coreutils`,
`time`, `openssh`, `sshfs`, `fuse3`, `sudo`, `vim`, `nano`, `less`, `tzdata`, `ca-certificates`
and the bootloader packages; it is built by `vm/scripts/install-base.sh` (Windows) or
`vm/build.sh` (Linux CI) from the same `vm/packages.txt` and `vm/rootfs/` overlay, and it contains
no SSH host keys and no cloud-init.
