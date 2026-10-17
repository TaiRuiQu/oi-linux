"""Run commands inside a guest over the QEMU serial console.

The guest images used by the acceptance tests expose a getty on the serial port, so this driver
logs in as root (no password in a freshly built image) and runs a list of commands, printing
everything it sees. Used for image bring-up and diagnosis.
"""

import socket
import sys
import time

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 44477
COMMAND_FILE = sys.argv[2] if len(sys.argv) > 2 else None
SETTLE = float(sys.argv[3]) if len(sys.argv) > 3 else 6.0


def connect():
    deadline = time.time() + 90
    while time.time() < deadline:
        try:
            sock = socket.create_connection(("127.0.0.1", PORT), timeout=5)
            sock.settimeout(1.0)
            return sock
        except OSError:
            time.sleep(0.5)
    raise SystemExit("could not reach the serial console")


def pump(sock, seconds):
    end = time.time() + seconds
    out = []
    while time.time() < end:
        try:
            data = sock.recv(65536)
        except socket.timeout:
            continue
        except OSError:
            break
        if not data:
            break
        text = data.decode("utf-8", "replace")
        out.append(text)
        sys.stdout.write(text)
        sys.stdout.flush()
    return "".join(out)


def wait_for(sock, needle, seconds):
    end = time.time() + seconds
    seen = []
    while time.time() < end:
        seen.append(pump(sock, 1.0))
        if needle in "".join(seen):
            return True
    return False


def main():
    sock = connect()
    time.sleep(1)
    sock.sendall(b"\n")
    window = pump(sock, 5)
    if "login:" in window:
        print("\n[driver] logging in as root", flush=True)
        sock.sendall(b"root\n")
        time.sleep(1)
        if not wait_for(sock, "#", 180):
            raise SystemExit("the login did not reach a shell prompt")
    print("\n[driver] shell ready", flush=True)

    commands = []
    if COMMAND_FILE:
        with open(COMMAND_FILE, "r", encoding="utf-8") as handle:
            commands = [line.rstrip("\n") for line in handle if line.strip()]
    for command in commands:
        print(f"\n[driver] $ {command}", flush=True)
        sock.sendall((command + "\n").encode())
        pump(sock, SETTLE)
    print("\n[driver] done", flush=True)


if __name__ == "__main__":
    main()
