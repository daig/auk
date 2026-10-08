#!/usr/bin/env python3
"""Record what a real program writes to a terminal, for the reference-grid tests (`terminal`
suite): run it in a PTY, drive it from a scenario, and keep the raw output bytes in order with
the resizes between them.

Outputs, next to --out:
- <out>.vt.jsonl   one object per line, in stream order:
                   {"t": ms, "out": "<base64>"}     bytes the program wrote (one read)
                   {"t": ms, "resize": [cols, rows]} the terminal was resized here
                   {"t": ms, "in": "<base64>"}      what the scenario typed (not replayed)
- <out>.meta.json  the command, size, environment, program version, scenario and counts

Usage:
  vt_record.py --out fixtures/terminal/name [--cols 80 --rows 24] [--scenario steps.json]
               [--env K=V ...] [--cwd DIR] [--timeout 60] [--note TEXT] -- program args...

The program runs with a fixed environment: a fresh temporary HOME, TERM=xterm-256color,
COLORTERM=truecolor and LANG/LC_ALL=en_US.UTF-8, plus --env. `{fixtures}` in arguments and
--env values is replaced by the absolute path of fixtures/terminal, and `{tmp}` by a fresh
temporary directory (which is also the default cwd and HOME).

Scenario steps (a JSON list), run in order:
  {"wait": "idle", "ms": 300}              until the program has been quiet for ms
  {"wait": "text", "text": "...", "timeout": 10}  until output since the last step contains it
  {"wait": "exit", "timeout": 10}
  {"keys": "text"}                         type text (JSON escapes like \\r or \\u001b)
  {"resize": [cols, rows]}                 resize the PTY (the program gets SIGWINCH)
  {"sleep": ms}

The fake terminal answers DA1 (`CSI ? 62 ; 22 c`) so programs that ask don't stall; it answers
nothing else, and it interprets nothing. After the scenario, the program is sent SIGHUP if it is
still running.
"""

from __future__ import annotations

import argparse
import base64
import datetime
import fcntl
import hashlib
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "terminal"
DA1_QUERIES = (b"\x1b[c", b"\x1b[0c")
DA1_ANSWER = b"\x1b[?62;22c"


def winsize(cols: int, rows: int) -> bytes:
    return struct.pack("HHHH", rows, cols, cols * 8, rows * 16)


class Recorder:
    def __init__(self, args: argparse.Namespace, tmp: Path) -> None:
        self.args = args
        self.tmp = tmp
        self.start = time.monotonic()
        self.lines: list[dict] = []
        self.since_step = b""
        self.tail = b""
        self.last_output = time.monotonic()
        self.pid = 0
        self.fd = -1
        self.status: int | None = None

    def ms(self) -> int:
        return int((time.monotonic() - self.start) * 1000)

    def subst(self, s: str) -> str:
        return s.replace("{fixtures}", str(FIXTURES)).replace("{tmp}", str(self.tmp))

    def env(self) -> dict:
        env = {
            "PATH": "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.tmp),
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8",
            "LC_ALL": "en_US.UTF-8",
            "TMPDIR": str(self.tmp) + "/",
            "USER": os.environ.get("USER", "user"),
        }
        for kv in self.args.env:
            k, _, v = kv.partition("=")
            env[k] = self.subst(v)
        return env

    def spawn(self, argv: list[str]) -> None:
        env = self.env()
        cwd = self.subst(self.args.cwd) if self.args.cwd else str(self.tmp)
        pid, fd = pty.fork()
        if pid == 0:
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, winsize(self.args.cols, self.args.rows))
                os.chdir(cwd)
                os.execvpe(argv[0], argv, env)
            finally:
                os._exit(127)
        self.pid, self.fd = pid, fd

    def pump(self, timeout: float) -> bool:
        """Reads what is ready within `timeout` seconds. Returns False once the PTY is closed."""
        if self.fd < 0:
            time.sleep(timeout)
            return False
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return True
        try:
            data = os.read(self.fd, 65536)
        except OSError:
            data = b""
        if not data:
            os.close(self.fd)
            self.fd = -1
            self.reap(block=True)
            return False
        self.lines.append({"t": self.ms(), "out": base64.b64encode(data).decode()})
        self.since_step += data
        self.last_output = time.monotonic()
        window = self.tail + data
        for q in DA1_QUERIES:
            if q in window and q not in self.tail:
                self.write(DA1_ANSWER, record=False)
        self.tail = window[-8:]
        return True

    def reap(self, block: bool = False) -> None:
        if self.status is not None:
            return
        try:
            pid, status = os.waitpid(self.pid, 0 if block else os.WNOHANG)
        except ChildProcessError:
            return
        if pid:
            self.status = os.waitstatus_to_exitcode(status)

    def write(self, data: bytes, record: bool = True) -> None:
        if self.fd < 0:
            raise SystemExit("the program exited before the scenario typed into it")
        if record:
            self.lines.append({"t": self.ms(), "in": base64.b64encode(data).decode()})
        os.write(self.fd, data)

    def wait_until(self, done, timeout: float, what: str) -> None:
        deadline = time.monotonic() + timeout
        while not done():
            if time.monotonic() > deadline:
                raise SystemExit(f"timed out waiting for {what}; last output: {self.since_step[-600:]!r}")
            if not self.pump(0.02) and not done():
                raise SystemExit(f"the program exited while waiting for {what}")

    def step(self, step: dict) -> None:
        if "wait" in step:
            kind = step["wait"]
            timeout = step.get("timeout", 15)
            if kind == "idle":
                quiet = step.get("ms", 300) / 1000
                self.wait_until(lambda: time.monotonic() - self.last_output >= quiet, timeout, "idle")
            elif kind == "text":
                text = step["text"].encode()
                self.wait_until(lambda: text in self.since_step, timeout, repr(step["text"]))
            elif kind == "exit":
                deadline = time.monotonic() + timeout
                while self.fd >= 0:
                    if time.monotonic() > deadline:
                        raise SystemExit(f"timed out waiting for exit; last output: {self.since_step[-600:]!r}")
                    self.pump(0.05)
            else:
                raise SystemExit(f"unknown wait {kind!r}")
        elif "keys" in step:
            self.write(step["keys"].encode())
        elif "resize" in step:
            cols, rows = step["resize"]
            # Read what is pending first, so the resize lands after it in the stream.
            while self.fd >= 0 and select.select([self.fd], [], [], 0)[0]:
                self.pump(0)
            self.lines.append({"t": self.ms(), "resize": [cols, rows]})
            if self.fd >= 0:
                fcntl.ioctl(self.fd, termios.TIOCSWINSZ, winsize(cols, rows))
                os.kill(self.pid, signal.SIGWINCH)
        elif "sleep" in step:
            deadline = time.monotonic() + step["sleep"] / 1000
            while time.monotonic() < deadline:
                self.pump(max(0.0, min(0.02, deadline - time.monotonic())))
        else:
            raise SystemExit(f"unknown step {step!r}")
        if "keys" in step or "resize" in step:
            self.since_step = b""


def version_of(program: str) -> str:
    for flag in ("--version", "-V"):
        try:
            out = subprocess.run([program, flag], capture_output=True, text=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            continue
        text = (out.stdout or out.stderr).strip().splitlines()
        if out.returncode == 0 and text:
            return text[0]
    return "unknown"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True)
    ap.add_argument("--cols", type=int, default=80)
    ap.add_argument("--rows", type=int, default=24)
    ap.add_argument("--scenario")
    ap.add_argument("--env", action="append", default=[])
    ap.add_argument("--cwd")
    ap.add_argument("--timeout", type=float, default=60)
    ap.add_argument("--note", default="")
    ap.add_argument("argv", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    argv = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
    if not argv:
        ap.error("no program given")
    steps = json.loads(Path(args.scenario).read_text()) if args.scenario else [{"wait": "exit"}]

    tmp = Path(tempfile.mkdtemp(prefix="vtrec-")).resolve()
    rec = Recorder(args, tmp)
    argv_run = [rec.subst(a) for a in argv]
    try:
        rec.spawn(argv_run)
        for step in steps:
            rec.step(step)
        # Drain, then hang up a program that is still running.
        rec.pump(0.2)
        if rec.fd >= 0:
            os.kill(rec.pid, signal.SIGHUP)
            deadline = time.monotonic() + 5
            while rec.fd >= 0 and time.monotonic() < deadline:
                rec.pump(0.05)
            if rec.fd >= 0:
                os.kill(rec.pid, signal.SIGKILL)
                os.close(rec.fd)
                rec.fd = -1
        rec.reap(block=True)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    stream = out.with_name(out.name + ".vt.jsonl")
    body = "".join(json.dumps(line, separators=(",", ":")) + "\n" for line in rec.lines)
    stream.write_text(body)
    out_bytes = sum(len(base64.b64decode(l["out"])) for l in rec.lines if "out" in l)
    meta = {
        "argv": argv,
        "cwd": args.cwd or "{tmp}",
        "cols": args.cols,
        "rows": args.rows,
        "env": args.env,
        "program": shutil.which(argv_run[0]) or argv_run[0],
        "version": version_of(argv_run[0]),
        "scenario": os.path.relpath(args.scenario, FIXTURES.parent.parent) if args.scenario else None,
        "recorded": datetime.datetime.now().astimezone().strftime("%Y-%m-%dT%H:%M:%S%z"),
        "recorder": "tools/vt_record.py",
        "exit_status": rec.status,
        "counts": {
            "out": sum(1 for l in rec.lines if "out" in l),
            "out_bytes": out_bytes,
            "resize": sum(1 for l in rec.lines if "resize" in l),
            "in": sum(1 for l in rec.lines if "in" in l),
        },
        "sha256": hashlib.sha256(body.encode()).hexdigest(),
        "note": args.note,
    }
    out.with_name(out.name + ".meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"{stream}: {meta['counts']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
