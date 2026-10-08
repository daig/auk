#!/usr/bin/env python3
"""A scriptable program for terminal tests: it runs inside a pane and lets a test drive it.

    python3 -I tools/pty_probe.py <unix-socket-path>

The probe connects to the test's socket and relays: everything the terminal writes to the
probe's stdin goes to the test, and the test tells the probe what to write to stdout and how to
set its tty. So a test sees exactly the bytes the terminal sends the program, through the real
PTY, daemon and replica.

Each message is one JSON object per line.

Test -> probe:
    {"write": "<base64>"}         write bytes to stdout
    {"tty": "raw"|"cooked"|"raw-echo"}  set the tty mode (raw: non-canonical, no echo)
    {"exit": <code>}              exit with that status
    {"ping": <n>}                 answered with {"pong": <n>} after everything before it is done

Probe -> test:
    {"hello": {"pid": <pid>, "tty": "<name>"}}  once, on connect
    {"read": "<base64>"}          bytes read from stdin
    {"pong": <n>}
"""

import base64
import json
import os
import select
import socket
import sys
import termios


def set_mode(fd, mode, saved):
    if mode == "cooked":
        termios.tcsetattr(fd, termios.TCSANOW, saved)
        return
    attrs = termios.tcgetattr(fd)
    attrs[3] &= ~(termios.ICANON | termios.ISIG | termios.IEXTEN)
    if mode == "raw":
        attrs[3] &= ~termios.ECHO
    else:
        attrs[3] |= termios.ECHO
    attrs[0] &= ~(termios.ICRNL | termios.IXON)
    attrs[6][termios.VMIN] = 1
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attrs)


def main():
    path = sys.argv[1]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    stdin, stdout = sys.stdin.fileno(), sys.stdout.fileno()
    saved = termios.tcgetattr(stdin) if os.isatty(stdin) else None

    def send(obj):
        sock.sendall((json.dumps(obj) + "\n").encode())

    send({"hello": {"pid": os.getpid(), "tty": os.ttyname(stdin) if saved else None}})
    buf = b""
    while True:
        ready, _, _ = select.select([stdin, sock], [], [])
        if stdin in ready:
            data = os.read(stdin, 65536)
            if not data:
                return 0
            send({"read": base64.b64encode(data).decode()})
        if sock in ready:
            chunk = sock.recv(65536)
            if not chunk:
                return 0
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                msg = json.loads(line)
                if "write" in msg:
                    data = base64.b64decode(msg["write"])
                    while data:
                        n = os.write(stdout, data)
                        data = data[n:]
                elif "tty" in msg and saved is not None:
                    set_mode(stdin, msg["tty"], saved)
                elif "exit" in msg:
                    if saved is not None:
                        termios.tcsetattr(stdin, termios.TCSANOW, saved)
                    return int(msg["exit"])
                elif "ping" in msg:
                    send({"pong": msg["ping"]})


if __name__ == "__main__":
    sys.exit(main())
