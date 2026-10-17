#!/usr/bin/env python3
"""Build the OI Linux base image by driving the Alpine live medium over one serial connection.

The live medium exposes a single serial port; QEMU's serial back-ends differ in how they behave
when a client disconnects and reconnects, and a second connection can end up accepted by the
kernel but never serviced by QEMU. This driver therefore does the whole build in **one** session:
log in as root, upload the payload, run the installer, wait for the completion marker, power off.

It is used by both `vm/build-base-image.ps1` (Windows) and `vm/build.sh` (Linux), so the two
platforms share the exact same build logic. Output is streamed to stdout and, optionally, to a
transcript file.
"""

import argparse
import base64
import hashlib
import os
import re
import socket
import sys
import time


class Session:
    def __init__(self, host: str, port: int, transcript_path: str | None):
        self.sock = self._connect(host, port)
        self.transcript = open(transcript_path, "wb") if transcript_path else None

    @staticmethod
    def _connect(host: str, port: int, timeout: float = 600.0) -> socket.socket:
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            try:
                sock = socket.create_connection((host, port), timeout=5.0)
                sock.settimeout(1.0)
                return sock
            except OSError as exc:
                last = exc
                time.sleep(0.5)
        raise SystemExit(f"could not connect to the serial console on {host}:{port}: {last}")

    def drain(self, seconds: float) -> str:
        end = time.time() + seconds
        chunks = []
        while time.time() < end:
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
            self._emit(data)
        return b"".join(chunks).decode("utf-8", "replace")

    def _emit(self, data: bytes) -> None:
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
        if self.transcript:
            self.transcript.write(data)
            self.transcript.flush()

    def wait_for(self, needle: str, timeout: float) -> bool:
        end = time.time() + timeout
        seen = []
        while time.time() < end:
            seen.append(self.drain(1.0))
            if needle in "".join(seen):
                return True
        return False

    def send(self, line: str) -> None:
        self.sock.sendall((line + "\n").encode())
        time.sleep(0.3)

    def close(self) -> None:
        try:
            self.sock.close()
        finally:
            if self.transcript:
                self.transcript.close()


def upload(session: Session, local_path: str, remote_path: str) -> None:
    size = os.path.getsize(local_path)
    digest = hashlib.sha256(open(local_path, "rb").read()).hexdigest()
    print(f"\n[build-image] uploading {local_path} ({size} bytes, sha256 {digest[:16]}…)", flush=True)
    with open(local_path, "rb") as handle:
        payload = base64.b64encode(handle.read()).decode()
    encoded = remote_path + ".b64"
    session.send(f"rm -f {encoded}")
    time.sleep(0.3)
    chunk = 400
    for start in range(0, len(payload), chunk):
        session.send(f"echo -n '{payload[start:start + chunk]}' >> {encoded}")
        time.sleep(0.04)
    time.sleep(1.0)
    session.send(f"base64 -d {encoded} > {remote_path} && rm -f {encoded} && "
                 f"sha256sum {remote_path} | cut -d' ' -f1")
    text = session.drain(25.0)
    if digest not in text:
        raise SystemExit("the uploaded payload does not match the local copy")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--payload", required=True, help="local tarball with rootfs/ and install-base.sh")
    parser.add_argument("--mirror", default="https://dl-cdn.alpinelinux.org/alpine")
    parser.add_argument("--branch", default="v3.24")
    parser.add_argument("--transcript", help="write everything the guest prints to this file")
    parser.add_argument("--login-timeout", type=float, default=600.0)
    parser.add_argument("--install-timeout", type=float, default=7200.0)
    args = parser.parse_args()

    session = Session(args.host, args.port, args.transcript)
    try:
        print("[build-image] waiting for the live medium", flush=True)
        session.send("")
        if not session.wait_for("login:", args.login_timeout):
            raise SystemExit("the live medium never showed a login prompt")
        session.send("root")
        if not session.wait_for("#", 180.0):
            raise SystemExit("the root login did not reach a shell prompt")
        print("\n[build-image] shell ready", flush=True)

        upload(session, args.payload, "/root/oi-payload.tar.gz")

        print("\n[build-image] installing the base system (this is the slow part)", flush=True)
        session.send(
            "set -o pipefail; rm -rf /root/oi-rootfs /root/scripts && "
            "tar -xzf /root/oi-payload.tar.gz -C /root && "
            "mv /root/rootfs /root/oi-rootfs && cp /root/scripts/install-base.sh /root/ && "
            f"sh /root/install-base.sh {args.mirror} {args.branch} 2>&1 | tee /root/install.log; "
            "echo INSTALL-EXIT=$?"
        )
        deadline = time.time() + args.install_timeout
        captured = []
        exit_code = None
        # The console echoes the command we typed, so `INSTALL-EXIT=$?` appears immediately;
        # only the executed line contains a *resolved* status.
        marker = re.compile(r"INSTALL-EXIT=(\d+)")
        while time.time() < deadline:
            captured.append(session.drain(5.0))
            match = marker.search("".join(captured))
            if match:
                exit_code = int(match.group(1))
                break
        transcript = "".join(captured)
        if exit_code is None:
            tail = "\n".join(transcript.splitlines()[-40:])
            print(f"\n[build-image] the installer did not report a status; last output:\n{tail}", flush=True)
            return 1
        if exit_code != 0:
            tail = "\n".join(transcript.splitlines()[-40:])
            print(f"\n[build-image] the installer failed with status {exit_code}; last output:\n{tail}", flush=True)
            return 1
        if "OI-INSTALL-OK" not in transcript:
            print("\n[build-image] the installer exited 0 but did not confirm completion", flush=True)
            return 1

        print("\n[build-image] installation finished, powering the live medium off", flush=True)
        session.send("poweroff -f")
        session.drain(15.0)
        return 0
    finally:
        session.close()


if __name__ == "__main__":
    raise SystemExit(main())
