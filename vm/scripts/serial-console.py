"""Drive a QEMU serial console over TCP.

Used while building the Alpine base image: the live ISO boots with the console on a TCP socket,
so the build script can log in as root (the live system has no password) and run the installer
without any interactive ceremony. Output is streamed to stdout so the build log shows exactly
what the guest did.
"""

import argparse
import socket
import sys
import time


def connect(host: str, port: int, timeout: float = 30.0) -> socket.socket:
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            sock = socket.create_connection((host, port), timeout=5.0)
            sock.settimeout(1.0)
            return sock
        except OSError as exc:  # the listener is not up yet
            last = exc
            time.sleep(0.5)
    raise SystemExit(f"could not connect to the serial console on {host}:{port}: {last}")


def drain(sock: socket.socket, seconds: float, echo: bool = True) -> str:
    """Read whatever arrives within `seconds` and return it."""
    deadline = time.time() + seconds
    chunks = []
    while time.time() < deadline:
        try:
            data = sock.recv(65536)
        except socket.timeout:
            continue
        except OSError:
            break
        if not data:
            break
        text = data.decode("utf-8", "replace")
        chunks.append(text)
        if echo:
            sys.stdout.write(text)
            sys.stdout.flush()
    return "".join(chunks)


def wait_for(sock: socket.socket, needle: str, timeout: float = 120.0) -> str:
    deadline = time.time() + timeout
    seen = []
    while time.time() < deadline:
        chunk = drain(sock, 1.0)
        seen.append(chunk)
        if needle in "".join(seen):
            return "".join(seen)
    raise SystemExit(f"timed out waiting for {needle!r}; last output:\n{''.join(seen)[-2000:]}")


def send(sock: socket.socket, line: str) -> None:
    sock.sendall((line + "\n").encode())


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--script", help="a shell script to upload and run")
    parser.add_argument("--command", help="a single command to run")
    parser.add_argument(
        "--upload",
        help="local file to transfer into the guest (base64, chunked)",
    )
    parser.add_argument("--upload-to", help="destination path inside the guest")
    parser.add_argument("--wait-for", default="#")
    parser.add_argument("--output-seconds", type=float, default=30.0)
    parser.add_argument("--login", action="store_true", help="log in as root first")
    parser.add_argument("--first-login", action="store_true", help="wait for a fresh login prompt")
    parser.add_argument("--enter", action="store_true", help="press Enter before waiting")
    parser.add_argument(
        "--probe-seconds",
        type=float,
        default=0.0,
        help="connect, print whatever arrives, press Enter and print again",
    )
    parser.add_argument("--no-wait", action="store_true", help="do not wait for a prompt")
    args = parser.parse_args()

    sock = connect(args.host, args.port, timeout=180.0)
    if args.probe_seconds:
        print("--- before input ---")
        drain(sock, args.probe_seconds)
        send(sock, "")
        print("--- after Enter ---")
        drain(sock, args.probe_seconds)
        return 0

    if args.first_login:
        wait_for(sock, "login:", timeout=900.0)
        send(sock, "root")
        time.sleep(2.0)
    else:
        # a fresh TCP connection to the socket gets no data until something is typed, so the
        # prompt has to be provoked before it can be recognised
        send(sock, "")
    if not args.no_wait and (
        args.login or args.first_login or args.enter or args.script or args.command or args.upload
    ):
        wait_for(sock, args.wait_for, timeout=120.0)

    if args.upload:
        import base64

        if not args.upload_to:
            raise SystemExit("--upload needs --upload-to")
        with open(args.upload, "rb") as handle:
            payload = base64.b64encode(handle.read()).decode()
        encoded_path = args.upload_to + ".b64"
        send(sock, f"rm -f {encoded_path}")
        time.sleep(0.5)
        chunk = 512
        for start in range(0, len(payload), chunk):
            piece = payload[start : start + chunk]
            send(sock, f"echo -n '{piece}' >> {encoded_path}")
            time.sleep(0.03)
        time.sleep(2.0)
        send(sock, f"base64 -d {encoded_path} > {args.upload_to} && rm -f {encoded_path} && "
                   f"sha256sum {args.upload_to} | cut -d' ' -f1 && echo OI-UPLOAD-OK")
        text = drain(sock, 20.0)
        import hashlib

        digest = hashlib.sha256(open(args.upload, "rb").read()).hexdigest()
        if digest not in text:
            raise SystemExit(
                f"the uploaded file does not match the local copy (expected sha256 {digest})"
            )
        return 0

    if args.script:
        with open(args.script, "r", encoding="utf-8") as handle:
            body = handle.read()
        import base64

        encoded = base64.b64encode(body.encode()).decode()
        # base64 keeps the console from interpreting the script's quoting
        send(sock, f"echo {encoded} | base64 -d > /tmp/oi-provision.sh")
        time.sleep(1.0)
        send(sock, "chmod +x /tmp/oi-provision.sh")
        time.sleep(0.5)
        send(sock, "sh /tmp/oi-provision.sh; echo OI-SCRIPT-EXIT=$?")
    elif args.command:
        send(sock, f"{args.command}; echo OI-COMMAND-EXIT=$?")
    else:
        send(sock, "")

    drain(sock, args.output_seconds)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
