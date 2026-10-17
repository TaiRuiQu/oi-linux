# OI Linux

A real x86-64 **Alpine Linux** training environment that runs on a Windows 10/11 machine, with a
single **NTFS workspace directory shared live** with the guest. Windows VS Code edits the files;
the Linux terminal compiles and runs them:

```text
D:\OIWorkspace\main.cpp          ← edited by VS Code on Windows, one and the same file
oi-linux:~$ g++ main.cpp -o main
oi-linux:~$ ./main
```

* the Linux machine is a **real** virtual machine (QEMU WHPX when it initialises, TCG otherwise),
  never a compatibility layer,
* the Windows directory is the **authoritative copy** of the user's source code; the Linux disk is
  disposable and can be reset at any time,
* the installer is **complete offline**: QEMU, the Alpine base image, the toolchain and the
  WebView2 runtime are all bundled,
* the project is licensed **GPL-3.0-or-later** (its own code only – see *Third-party licences*).

The normative document is [`OILinux_PRD.md`](OILinux_PRD.md) (the frozen architecture baseline);
everything in this repository implements it. Design notes live in [`docs/`](docs/), and the
record of what was actually verified on real hardware is
[`docs/verification.md`](docs/verification.md).

## Repository layout

| Path | Content |
|---|---|
| `src/` | React + TypeScript + xterm.js front end (Tauri web view) |
| `src-tauri/` | Tauri shell: window, command surface, event wiring |
| `src-tauri/crates/oi-core/` | The engine: VM lifecycle, QMP, host SFTP share, storage, SSH, updater |
| `integration-tests/` | Blocking acceptance tests (`ntfs-share`, `chmod-exec`, `boot-reset`, `upgrade-relink`) |
| `vm/` | Alpine base image build: package list, `rootfs/` overlay, installer scripts |
| `scripts/` | Host-side helpers (resource staging, icon generation, image inspection) |
| `docs/` | Architecture, protocol, threat model, test plan, decision log |

## Building the Alpine base image

The base image is a normal Alpine installation with the toolchain pre-installed, the per-machine
identity contract of §5.3 of the PRD, and no reusable credentials.

```powershell
# Windows (this repository's primary platform)
powershell -ExecutionPolicy Bypass -File vm\build-base-image.ps1
# → vm\out\base.qcow2, vm\out\base.qcow2.sha256
```

```bash
# CI (Linux, equivalent semantics via alpine-make-vm-image)
vm/build.sh
```

Both paths install the same package list (`vm/packages.txt`), the same `vm/rootfs/` overlay and
the same post-install hardening, and both end with a boot test.

## Development

```powershell
npm install
npm run build                 # front-end type check + bundle
cargo build --workspace       # engine, shell and acceptance tests
cargo test --workspace        # unit tests
npm run tauri dev             # run the application
```

The acceptance tests need a built base image and QEMU, and drive the *production* code paths:

```powershell
cd integration-tests
$env:OI_TEST_DIR = 'D:\oi-linux-tests'
cargo run --bin ntfs-share     # §11.2 P0 file sharing
cargo run --bin chmod-exec     # §11.2 P0 execution and permissions
cargo run --bin boot-reset     # §11.3 P0 lifecycle and data safety
cargo run --bin upgrade-relink # §4.3/§10.2 re-association and update channels
cargo run --bin perf-baseline  # §11.4 performance baseline
```

They create a real environment in `OI_TEST_DIR` (default `D:\oi-linux-tests`), boot the real
guest, and print a `PASS`/`FAIL` report; a non-zero exit code means at least one blocking
criterion failed.

Every push and pull request also builds the complete offline installer: the run's
`oi-linux-installer` artefact is the ready-to-run `.exe` with QEMU, the verified Alpine base
image, the WebView2 runtime and the licence inventory inside it. A tag makes `release.yml`
publish the same bytes as a GitHub release.

## Where the data lives

| Path | Content |
|---|---|
| `C:\Program Files\OI Linux\` | the installed application (QEMU, resources) |
| `D:\OI-Linux-Data\` | Linux data directory: base image, writable overlay, keys, metadata, logs, state |
| `D:\OIWorkspace\` | the Windows workspace: **the user's real files** |

`base.qcow2` is immutable for the life of an environment; every Linux change lands in
`user.qcow2`. A reset rebuilds `user.qcow2` and never touches the workspace or the base image.

## Third-party licences

OI Linux is GPL-3.0-or-later. It distributes other people's work alongside its own, each under
its own licence: QEMU (GPL-2.0-or-later), Alpine Linux and its packages (MIT and per-package
licences), OpenSSH (BSD-style), SSHFS/FUSE (GPL-2.0-or-later), GCC/GDB (GPL-3.0-or-later) and
the Microsoft WebView2 runtime (Microsoft terms). The machine-readable inventory that ships with
the installer is generated into `src-tauri/resources/licenses/third-party.json`; source-offer
obligations are satisfied by publishing the corresponding sources with each release.
