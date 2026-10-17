# Verification record

This file records what was actually observed on the machine that built and tested this repository.
It is evidence, not a plan: the commands below can be re-run, and the reports they print are the
same ones quoted here.

## Environment

| Item | Value |
|---|---|
| Host | Alibaba Cloud ECS, Windows Server 2022 (10.0.20348), 8 GiB RAM, 2 vCPU / 4 threads |
| Virtualisation | **no** nested virtualisation — QEMU's WHPX probe fails to load `WinHvPlatform.dll`, so the TCG path is exercised (the PRD's "no VT-x" test row) |
| QEMU | 11.1.0 (`C:\Program Files\qemu`) |
| Alpine base image | 3.24.2, `vm/out/base.qcow2`, 595,656,704 bytes, sha256 `5148c5a528ee03a664fbf63d95f595fcee1d8cd060870a5888afc586461902fe` |
| Shared workspace | a real NTFS directory under `D:` |
| Test roots | `D:\oi-acceptance` (environments live there and are re-associated on re-runs) |

The host's `Documents` folder is redirected by a cloud-desktop filter driver, and executables
located there are denied directory creation on every volume. The verification therefore ran the
test binaries from a copy outside the repository; this is a host limitation, documented in
`docs/test-plan.md`.

## What was run

```powershell
# unit tests (84 in oi-core + 1 in the shell crate)
cargo test --workspace

# front end
npm run typecheck
npm run build

# blocking acceptance suites, each against a real Alpine guest started by the production engine
D:\oi-work\tests\ntfs-share.exe      # §11.2 file sharing          -> 31/31
D:\oi-work\tests\chmod-exec.exe      # §11.2 execution/permissions -> 10/10
D:\oi-work\tests\boot-reset.exe      # §11.3 lifecycle/data safety -> 16/16
D:\oi-work\tests\upgrade-relink.exe  # §4.3/§10.2 re-association   -> 10/10

# performance baseline (§11.4)
D:\oi-work\tests\perf-baseline.exe
```

## Results

### §11.2 P0 file sharing — 31 of 31 criteria passed

Protocol level (no VM, real SSH/SFTP client against the production service):

* a legitimate create/read round trip works (positive control),
* 14 hostile path spellings are refused (`..`, absolute paths, drive letters, `\\?\`, UNC, ADS,
  reserved device names, trailing dot/space …), as is a real junction and a symlink attempt,
* the service always requires authentication (wrong key and wrong account both refused) and
  offers only the `sftp` subsystem (exec is answered with a channel failure),
* the listener is on a random loopback port (never 2222, `netstat` shows `127.0.0.1` only),
* protocol version 3 advertises `posix-rename@openssh.com` and `fsync@openssh.com`;
  `posix-rename` replaces an existing file while the plain rename refuses to,
* modes: new files `0644`, directories `0755`, `chmod +x` visible immediately, rename keeps the
  mode of the entity, a replacement resets it to the default,
* a directory with 3000 entries is listed completely (this caught a real bug: the service used to
  send the whole listing as one SFTP packet, which clients dropped).

End to end (real Alpine guest, real sshfs mount):

* Windows create → Alpine reads immediately; an editor-style atomic replace is visible on the
  next read,
* Alpine create/rename/delete → Windows sees it immediately,
* a Windows exclusive lock makes the guest's write fail with a clear error and the original
  content survives; the same write succeeds after the lock is released,
* stopping the share service leaves Linux fully usable (`compiler-ok`) with the share reported as
  `Degraded`; after the service is restarted the mount is re-established and a read/write round
  trip is verified,
* Chinese names and spaces work in both directions; case-only differences address one NTFS file,
* the share port is random and loopback-only,
* the execute bit survives a remount.

### §11.2 P0 execution and permissions — 10 of 10 criteria passed

* `g++ -O2 -std=c++17 main.cpp -o main && ./main` from the shared directory, twice over the same
  output name,
* the output is a dynamically linked ELF64 (`file`, `readelf … INTERP`),
* compiler output is created executable (`0755`),
* `chmod -x` → `./main` fails with a permission error; `chmod +x` → it runs again,
* `execve`, `MAP_SHARED` and `MAP_PRIVATE` work from the share (private writes do not leak into
  the file, shared reads see the file content),
* a Windows rename keeps `0755`; a Windows replacement lands in `0644`,
* the execute bit persists across a full stop/start cycle (`755` before and after, and the binary
  still prints `hello-oi-v2`).

### §11.3 P0 lifecycle and data safety — 16 of 16 criteria passed

* a real Alpine Linux starts with no hardware virtualisation and stops cleanly,
* data written inside the Linux disk survives a stop/start cycle (it uses the graceful stop path,
  which is what the product does),
* the Windows workspace is untouched by restarts and by a reset,
* the workspace directory can only be changed while Linux is stopped,
* **the complete core flow works with the guest isolated from every network**: with QEMU's
  `restrict=on`, the internet is unreachable (`wget-exit=1`) while SSH, the share (mounted) and a
  `g++` build still work,
* reset is refused while running; it rebuilds the overlay, leaves `base.qcow2` and
  `environment.json` byte-identical, deletes guest-only data and leaves the workspace usable,
* three genuine start failures are counted, nothing is cleaned up automatically, and the
  environment starts again once the fault is repaired,
* a crashed worker process leaves no QEMU writing to the overlay (Job Object), verified by the
  overlay being exclusively openable and no QEMU process remaining.

### §4.3/§10.2 re-association and update channels — 10 of 10 criteria passed

* a second instance cannot open an environment that is in use (instance lock),
* an existing environment can be re-associated after a "reinstall" (fresh settings file): identity,
  base hash and creation time unchanged, images not moved or rewritten,
* a modified `base.qcow2` makes re-association refuse (hash mismatch),
* creating a new environment over an existing one is refused,
* Stable ⇄ Beta switching is accepted, leaves `environment.json` and the overlay untouched, keeps
  the relative `base.qcow2` backing chain, and the environment still starts afterwards.

### Unit tests

`cargo test --workspace`: **84 passed** in `oi-core` plus 1 in the shell crate. They cover QEMU
argument construction (loopback-only forward, structured argv with spaces and non-ASCII paths),
the mode database, path validation, ISO writing, version comparison, update signature
verification, the failure counter, log redaction, the storage layout and the localised Windows
version parser.

### Front end

`npm run typecheck` and `npm run build` both succeed (89 modules, `dist/` produced). The Tauri
application starts, writes its log (`OI Linux starting version="0.1.0" alpine="3.24.2"`) and keeps
running; it was launched from a copy outside the redirected folder because of the host limitation
above.

### §11.4 performance baseline

Measured through the production stack (QEMU TCG, reverse-tunnel sshfs with
`default_permissions,noauto_cache,attr_timeout=0,entry_timeout=0`) and compared with the same work
inside the guest's ext4. Every sample reports the number of files it actually saw, and all of them
matched the expected count.

| Files | create (share) | create (ext4) | `ls` (share) | `find -type f` (share) | read all (share) |
|---|---|---|---|---|---|
| 100 | 6.27 s | 0.27 s | 0.13 s | 2.49 s | 6.44 s |
| 1000 | 56.08 s | 1.51 s | 0.28 s | 23.03 s | 63.25 s |
| 5000 | 280.01 s | 7.17 s | 0.88 s | 116.79 s | 318.99 s |

* `ls` and `find` inside ext4 for the same directories: 0.05/0.07/0.21 s and 0.05/0.07/0.21 s.
* Single-file C++17 compile from the share: 35.50 s / 32.14 s / 31.71 s; inside ext4: 29.61 s.
  Running the produced binary from the share: 0.14–0.17 s.
* 64 MiB sequential write through the share: 10.49 s; read: 11.26 s.
* QEMU working set with a 1 GiB guest: 918 MiB; host total memory 7918 MiB.

The share is correctness-first: with the attribute cache disabled every metadata operation is a
round trip, which is why creation and full reads are roughly one to two orders of magnitude
slower than ext4 while a directory listing stays cheap. The numbers are the baseline later work
can be compared against; the PRD deliberately sets no thresholds for V1.

The raw report is `integration-tests/artifacts/perf-baseline.json`.

## Known limitations of the verification

* **WHPX** could not be exercised (no nested virtualisation). The probe's failure and the TCG
  fallback are the behaviour the PRD requires on such a machine; WHPX remains the release-time
  manual gate.
* **The NSIS installer** was not built here (it needs the Tauri bundler's NSIS download); the
  `installer` job of `windows-integration.yml` builds it on every run from the staged resources,
  and `release.yml` publishes it.
* The host limitation described at the top means the verification binaries ran from a copy. The
  production code path is identical; nothing in the suites depends on their own location.
