# Test plan

The blocking criteria of `OILinux_PRD.md` §11 are implemented as runnable suites. Every suite boots
the **real** Alpine guest through the **production** engine (`oi_core::engine`) and drives it over
the **production** SSH client; nothing is simulated or reimplemented for the tests.

## Running them

```powershell
cd integration-tests
$env:OI_TEST_DIR = 'D:\oi-linux-tests'    # where the test environments live
cargo run --bin ntfs-share                # §11.2 file sharing
cargo run --bin chmod-exec                # §11.2 execution and permissions
cargo run --bin boot-reset                # §11.3 lifecycle and data safety
cargo run --bin upgrade-relink            # §4.3/§10.2 re-association and update channels
cargo run --bin perf-baseline             # §11.4 performance baseline
```

Each suite prints one `[PASS]`/`[FAIL]` line per criterion with the evidence it observed, and exits
non-zero if anything failed. Environments are created once and re-associated on later runs, so a
re-run only pays for the boot.

Requirements: a built base image (`vm\out\base.qcow2`), QEMU (found through `OI_LINUX_QEMU` or
`C:\Program Files\qemu`), and a machine that can run QEMU — hardware virtualisation is *not*
required, TCG is exercised on purpose.

## Coverage

### §11.2 P0 — the shared workspace (`ntfs-share`)

| Criterion | How it is checked |
|---|---|
| 1 Windows create → Linux read | file written on the Windows side, read back in the guest |
| 1 atomic replace | temporary file renamed over the original, next Linux read must see the new content |
| 2 Linux create/rename/delete → Windows | guest writes, renames and deletes; the Windows directory is inspected directly |
| 4 `chmod ±x` | `chmod -x` then `./main` must fail, `chmod +x` then it must run, and the bit must survive a remount |
| 5 permissions follow the entity | Windows rename keeps the mode; a Windows replacement resets it to the default |
| 7 locked file | a Windows handle with no sharing makes the guest's write fail and leaves the original content intact |
| 7 share outage | the host service is stopped: Linux stays usable, the state is reported as degraded, and after the service is restored a full read/write round trip is verified |
| 8 hostile SFTP paths | 14 spellings (`..`, absolute, UNC, ADS, device names, trailing dot/space …) plus a real junction and a symlink attempt, all against the production service |
| 9 names | Chinese names, spaces and case-only differences behave predictably, with no escape |
| 10 listener | the port is random, above 1024, never 2222, and `netstat` shows it bound to `127.0.0.1` only |

### §11.2 P0 — execution and permissions (`chmod-exec`)

| Criterion | How it is checked |
|---|---|
| 3 compile and run | `g++ -O2 -std=c++17 main.cpp -o main && ./main` from the shared directory, twice, over the same output name |
| 3 output is a real ELF | `file` reports a dynamically linked ELF64, `readelf` finds `INTERP` |
| 4 permissions persist | `./main` after a full stop/start cycle |
| 5 entity semantics | rename keeps `0755`; an editor-style replacement lands in `0644` |
| 6 dynamic linking, `execve`, `mmap` | a probe program maps a shared data file `MAP_SHARED` and `MAP_PRIVATE`, writes through the private map, and verifies the file was not modified |
| 7.3 compiler output | a file produced by `g++` is created executable |

### §11.3 P0 — lifecycle and data safety (`boot-reset`)

| Criterion | How it is checked |
|---|---|
| TCG start | the guest boots and answers over SSH with no hardware acceleration available |
| guest persistence | data written inside the Linux disk survives a stop/start cycle |
| workspace protection | the Windows workspace is untouched by restarts and by a reset |
| workspace switching | refused while running, accepted while stopped |
| offline core flow | the guest is started with QEMU's `restrict=on`: SSH, the share, and a `g++` build must still work while the internet is unreachable |
| reset semantics | the overlay is rebuilt, `base.qcow2` and `environment.json` are unchanged, guest-only data is gone, the workspace still works |
| reset safety | refused while running |
| failure handling | three genuine start failures are counted, nothing is cleaned up automatically, and the environment starts again once the fault is repaired |
| crash containment | a worker process starts QEMU and exits without cleaning up; the overlay must be free and no QEMU may be left behind (Job Object) |

### §4.3/§10.2 — re-association and updates (`upgrade-relink`)

| Criterion | How it is checked |
|---|---|
| re-association | a fresh settings file opens an existing data directory; environment id, base hash and creation time are unchanged |
| tamper detection | a modified `base.qcow2` makes re-association refuse |
| no accidental overwrite | creating a new environment over an existing one is refused |
| channel switching | Stable ⇄ Beta accepted in the same application; `environment.json` and the overlay are untouched; the backing chain still points at the relative `base.qcow2`; the environment still starts |

### §11.4 — performance baseline (`perf-baseline`)

Measurements, not thresholds: 100/1000/5000 files listed, found and read through the share; a C++17
compile+run cycle; the same work inside the guest's ext4 for comparison; and the memory footprints
of QEMU and the host process. The report is written as JSON so that later optimisations can be
compared against it.

### Unit tests

`cargo test --workspace` covers the parts that do not need a virtual machine: QEMU argument
construction (loopback-only forwards, structured argv with spaces and non-ASCII paths), the mode
database, path validation, ISO image writing, version comparison and update signature verification,
the failure counter, log redaction and the storage layout.

## Environment notes

* The machine these suites were developed on is an Alibaba Cloud ECS instance running Windows
  Server 2022 (build 10.0.20348) with 8 GiB of RAM and **no nested virtualisation**, so it maps to
  the PRD's "Windows 10 x64, 8 GiB, no VT-x/AMD-V → TCG" test row. Win32, NTFS and the Win32
  APIs the product uses are the same as on Windows 10/11; the installer still targets 10/11.
* That machine's `C:\Users\<user>\Documents` folder is redirected by a cloud-desktop filter
  driver. Newly built executables under that folder are denied directory creation on every volume
  (the same bytes work from any other folder). The verification run therefore executes the
  binaries from a copy outside the repository; this is a host limitation, and the suites do not
  depend on their own location (`CARGO_MANIFEST_DIR` and the base image path are compiled in).
* `OI_TEST_DIR` can point anywhere; each suite keeps its environment under that root and
  re-associates it on later runs, which is also how the re-installation case is tested.

## Not automated here

* **WHPX on real hardware.** The PRD makes a WHPX run a release-time manual gate; the machine the
  suites were developed on has no nested virtualisation, so the TCG path is the automated one and
  the WHPX path is exercised by its probe (which fails cleanly and falls back).
* **A physical restore of the system drive.** Re-association is tested by discarding the settings
  file, which is what a reinstall loses.
* **The installed NSIS package.** The release workflow builds it; the acceptance suites test the
  application and the engine, not the installer wrapper.
