# The shared workspace protocol

`OILinux_PRD.md` §7 requires one thing above all: the Windows folder and `/home/student/workspace`
must be *the same files*, not copies, and `g++ main.cpp -o main && ./main` must work directly from
them. This document describes the mechanism that implements that.

## Shape of the mechanism

```text
guest (Alpine)                         host (Windows)
────────────────                       ──────────────
/home/student/workspace                D:\OIWorkspace
        │                                     │
        └── FUSE ── sshfs ── SSH ──► host SFTP service ── Win32 file APIs ──► NTFS
```

* `sshfs` mounts the host service's SFTP root at `/home/student/workspace`.
* The host service is a small SSH server (russh) that offers **only** the SFTP subsystem, always
  requires public key authentication, and binds to loopback.
* Local tools (`g++`, `gdb`, `tsan`, …) use ordinary POSIX calls; FUSE turns them into SFTP
  requests; the host answers them from NTFS.

Because the root of the SFTP service *is* the workspace directory, "the same file" is literally
true: `main` written by `g++` is a real NTFS file, and VS Code can open it while the guest still
has it mapped.

## Mount options and what they buy

```text
default_permissions,idmap=user,noauto_cache,dir_cache=no,reconnect,
ServerAliveInterval=15,ServerAliveCountMax=4,StrictHostKeyChecking=yes,
UserKnownHostsFile=/home/student/.ssh/known_hosts,IdentitiesOnly=yes,
IdentityFile=/home/student/.ssh/id_ed25519,BatchMode=yes
```

| Option | Why |
|---|---|
| `default_permissions` | the kernel enforces the mode the service reports, so `chmod -x main` really stops `./main` |
| `idmap=user` | `student` on both sides, so ownership is coherent |
| `noauto_cache`, `dir_cache=no` | no content cache that could hide a file another program just replaced |
| `reconnect` | a dropped connection recovers instead of leaving a stale mount |
| `StrictHostKeyChecking` | the host service's key is pinned per machine; nothing is trusted on first use |

A minimal option set is retried when the preferred one is refused by the guest's FUSE/sshfs build.

## Permissions: NTFS identity + SQLite

NTFS has no POSIX mode, so the service keeps one in `metadata.db`, keyed by the *file entity*:

```sql
CREATE TABLE file_mode (
    volume_id     TEXT NOT NULL,   -- NTFS volume serial
    file_id       BLOB NOT NULL,   -- file index, from the open handle
    created_at_fs TEXT NOT NULL,   -- creation time, to notice id reuse
    mode          INTEGER NOT NULL,
    file_kind     TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (volume_id, file_id, created_at_fs)
);
```

* The identity comes from the file system through a handle (`GetFileInformationByHandle`), never
  from the path the client sent.
* A rename keeps the entity, so the mode follows the file.
* An editor that writes a temporary file and renames it over the original creates a *new* entity,
  which starts from the Linux default (`0644` for files, `0755` for directories, directories always
  traversable).
* `SETSTAT`/`FSETSTAT` persist the execute bit transactionally; `STAT`/`LSTAT`/`FSTAT` and
  directory listings report it back.

## Path safety

Every request is validated in three layers before it can touch the file system:

1. **Lexical** – components are checked for `..`, `:`, reserved device names, trailing dots and
   spaces, Windows special paths, control characters and wildcards. A `..` that pops past the root
   is refused; `a/../b` is not.
2. **Containment** – the candidate is canonicalised (resolving every reparse point) and must still
   be under the canonical root, including for paths that do not exist yet.
3. **Handle** – files are opened with `FILE_FLAG_OPEN_REPARSE_POINT` and the resulting object's
   attributes are inspected, so a link swapped in after step 2 is refused rather than followed.

Junctions, symlinks, UNC paths, alternate data streams and drive letters are all refused. The
integration tests assert each of those spellings against the real service.

## Extensions and failure behaviour

* `posix-rename@openssh.com` and `fsync@openssh.com` are advertised and implemented, so a renaming
  save replaces the destination in one Windows file system operation.
* `rename` (protocol version 3) refuses to overwrite, as the specification requires.
* When Windows itself blocks an operation – a file held open exclusively by another program, for
  instance – the service returns an explicit error instead of losing data; the tests hold a file
  open with no sharing and assert that the guest's write fails and the original content survives.
* If the listener dies, the mount goes to `SHARE_DEGRADED`, Linux keeps running, and the share is
  re-established with backoff or from the "retry mount" button. Before a remount the existing mount
  point is removed, so a failed mount can never be mistaken for an empty working directory.

## Windows Terminal and VS Code

Both are optional. When Windows Terminal is present the application writes an SSH configuration
into the data directory (`ssh\windows\config`, owner-only ACL) that pins the current port, the
guest host key and the client key, and opens a tab with it. VS Code simply gets the workspace
directory; compile and run always happen in the Linux terminal, by design.
