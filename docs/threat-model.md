# Threat model

The product runs a Linux machine on a student's Windows PC, with passwordless `sudo` inside the
guest and a shared folder that both sides can write. That is the point of the tool, and it is also
what this document is about: what is assumed to be hostile, what is not, and what keeps the two
sides apart.

## Assumptions

| Component | Trust |
|---|---|
| Windows user account | trusted for its own files; the application runs with the user's privileges and never asks for permanent elevation |
| Windows workspace directory | **the user's data**, never deleted or rewritten by the application |
| Guest Linux, including `student` and anything the student runs as root | **untrusted** for the host: guest programs may be malicious or compromised |
| Host share service | the only inbound surface from the guest; must be safe against a hostile client |
| The network | not trusted, and not used: nothing listens on a LAN-reachable address |

## Boundaries

### 1. Guest → host

The only way in is the host share service, and it is deliberately narrow:

* binds to `127.0.0.1` on a random port, so nothing on the school network can reach it,
* always requires public key authentication with the per-machine guest identity; passwords are
  refused, and so is `auth_none`,
* offers exactly one subsystem (`sftp`) – `exec`, `shell`, `pty` and port forwarding requests are
  answered with a channel failure,
* is rooted at the single configured workspace directory, and refuses every path spelling that
  could leave it (`..`, absolute paths, drive letters, UNC paths, alternate data streams, device
  names, reparse points, case variants),
* never writes outside that root, and reports a locked file as an error instead of losing data.

A compromised guest can therefore read, modify and delete the files in the workspace – which is the
documented, intended behaviour of a *shared* workspace, and the reason the interface tells students
to keep their sources in version control. It cannot reach anything else on the host.

### 2. Host → guest

The host is an SSH client with a per-machine key. The guest's host key is generated on the host at
initialisation time and delivered through the boot-time seed disk, so the very first connection is
verified (there is no trust-on-first-use step and host key checking is never disabled).

The seed medium also carries the guest's own private key for the share service, the
`authorized_keys` entry for the host, and the pinned `known_hosts` line for the host service. It is
a read-only ISO9660 image inside the Linux data directory, which is restricted to the current
Windows user with `icacls`; it is regenerated on every launch because the endpoints change.

### 3. Files on disk

| Material | Protection |
|---|---|
| SSH private keys, `metadata.db`, `environment.json`, logs | data directory ACL restricted to the current Windows user |
| Boot-time seed disk | same ACL; carries private keys, never stored in the workspace |
| Update signing key | never shipped; the public key is pinned in the build, the private key lives only in CI secrets |
| Logs | no terminal content, no source code; export redacts user names, absolute paths and key material |
| Telemetry | none, ever; there is no remote endpoint except the update check the user triggers |

The workspace directory itself keeps whatever ACL the user's own folders have: the application does
not widen it, and the guest's root account is *not* a Windows administrator.

## Threats and mitigations

| Threat | Mitigation |
|---|---|
| Malicious guest reads host files outside the workspace | path validation in three layers (lexical, canonical containment, handle inspection) |
| Malicious guest escapes the SFTP root via a junction or symlink | reparse points are detected and refused, including those appearing between the check and the open |
| Malicious guest uses the share service to run code on the host | the service implements no shell, exec, PTY or forwarding |
| Network attacker reaches the share service | loopback-only binding, random port, public key authentication |
| Another local user reads the private keys or metadata | owner-only ACLs on the data directory, enforced after every write |
| Password spraying against the guest | password and keyboard-interactive authentication are disabled; only the per-machine key is accepted |
| Locked student account breaking login | the account carries an unusable random password rather than being locked, because OpenSSH refuses locked accounts outright |
| Data loss through a silent failure | locked or occupied files produce explicit errors; a failed mount removes the mount point instead of leaving an empty directory; a failed start never cleans anything up |
| A crashed application leaving QEMU writing to the overlay | every QEMU process is placed in a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job; a launch is aborted if the job cannot be created |
| Two instances racing on the same overlay | `state\instance.lock` is opened with no sharing; the second instance refuses to operate |
| A damaged permission database | the share stops, the Windows files are untouched, and the user is asked to repair or rebuild the metadata manually |
| An unsigned update | every update package is verified against the pinned ed25519 public key and its published SHA-256 before anything is executed |

## Explicit non-goals

* Passwordless `sudo` in the guest is a convenience for an educational environment; it is not a
  security boundary, and the guest is assumed to be untrusted from the host's point of view.
* V1 installers are not Authenticode signed. The updater *is* signed, and the release publishes
  SHA-256 sums; the UI says so instead of implying otherwise.
* Deleting files from the guest deletes them from Windows. There is no trash, no snapshot and no
  backup; the interface tells the user to use Git.
