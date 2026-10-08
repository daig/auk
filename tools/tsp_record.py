#!/usr/bin/env python3
"""Run a program in a PTY behind a fake TSP terminal, and record what it sends.

The fake terminal:
- answers the TSP hello (before DA1, as Tern does), `blobs` queries, DA1/DA2, CPR, the kitty
  keyboard query, XTVERSION, window-size reports and color queries;
- acknowledges every frame of a listening surface and sends `resize` when one first draws;
- can drive the program from a scenario of steps (wait for a frame, type keys, send events).

Outputs, next to --out:
- <out>.jsonl     every logical TSP message, in Tern's recording format (book:11579):
                  {"t": unix_ms, "dir": "out"|"in", "verb": ..., "params": {...}, "body": ...}
                  plus `osc` lines for titles, cwd, shell marks and user variables, as
                  `surface-play` accepts them
- <out>.pty       the raw bytes the program wrote (with --raw)
- <out>.meta.json the command, size, versions and per-verb counts

Usage:
  tsp_record.py --out fixtures/tsp/name [--cols 120 --rows 40] [--scenario steps.json]
                [--env K=V ...] [--cwd DIR] [--timeout 60] [--raw] -- program args...

Scenario steps (a JSON list), run in order:
  {"wait": "open"|"frame"|"idle"|"exit", "contains": "...", "ms": 1500, "timeout": 30}
  {"keys": "text"}            type text (escapes like \\r are JSON escapes)
  {"key": "enter"|"escape"|"tab"|"backspace"|"ctrl+c"|"ctrl+d"|...}
  {"event": {...}}            send a TSP event; "$sf" is the last listening surface, and
                              "$find:k=el,tag=button" is the id of the first matching node
  {"sleep": ms}
  {"signal": "TERM"|"INT"|"KILL"}
"""

from __future__ import annotations

import argparse
import base64
import binascii
import fcntl
import hashlib
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time
from pathlib import Path

KINDS = [
    "col", "row", "card", "section", "rule", "spacer", "text", "md", "code", "diff", "ansi",
    "math", "image", "kv", "table", "tree", "badge", "kbd", "icon", "spinner", "shimmer",
    "elapsed", "progress", "rate", "list", "item", "tabs", "editor", "input", "status", "seg",
    "overlay", "toast", "rows", "picker", "prefs", "tool", "checklist", "agent", "chart",
    "meter", "block", "effort", "el",
]
FEATURES = [
    "blobs", "settle", "adopt", "dock", "program-palette", "reduce-motion", "aside", "scroll",
    "styles", "flow",
]
# Environment that makes SDKs or omp think they run inside a multiplexer (spec §7.2).
MUX_VARS = {"TMUX", "TMUX_PANE", "STY", "WMUX"}
MUX_PREFIXES = ("ZELLIJ", "CMUX_", "WMUX_")
PARAM = re.compile(rb"([A-Za-z0-9_-]+)=([\x21-\x3a\x3c-\x7e]*);")
CELL = (8, 17)


def now_ms() -> int:
    return int(time.time() * 1000)


class Recorder:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.cols, self.rows = args.cols, args.rows
        self.out_path = Path(args.out)
        self.jsonl = open(self.out_path.with_suffix(".jsonl"), "w", encoding="utf-8")
        self.raw = open(self.out_path.with_suffix(".pty"), "wb") if args.raw else None
        self.buf = b""
        self.counts: dict[str, int] = {}
        self.surfaces: dict[str, dict] = {}
        self.last_listening: str | None = None
        self.nodes: dict[str, dict] = {}  # sf/id -> {"k", "p", "parent", "order"}
        self.order = 0
        self.blobs: set[str] = set()
        self.chunk: dict | None = None
        self.kitty: list[int] = [0]
        self.frames: list[tuple[float, str]] = []
        self.opens: list[float] = []
        self.last_output = time.monotonic()
        self.pending_acks: list[tuple[float, str, int]] = []
        self.drawn: set[str] = set()
        self.unanswered: dict[str, int] = {}
        self.user_vars: dict[str, str] = {}
        self.pid = 0
        self.fd = -1
        self.exit_status: int | None = None

    # ------------------------------------------------------------------ process
    def spawn(self, argv: list[str]) -> None:
        env = {k: v for k, v in os.environ.items() if k not in MUX_VARS and not k.startswith(MUX_PREFIXES)}
        env.update(
            TERM="xterm-256color",
            COLORTERM="truecolor",
            TERM_PROGRAM="auk",
            TERM_PROGRAM_VERSION="0.0.0-recorder",
        )
        env.setdefault("LANG", "en_US.UTF-8")
        for pair in self.args.env:
            key, _, value = pair.partition("=")
            env[key] = value
        winsize = struct.pack("HHHH", self.rows, self.cols, self.cols * CELL[0], self.rows * CELL[1])
        pid, fd = pty.fork()
        if pid == 0:  # child
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, winsize)
                if self.args.cwd:
                    os.chdir(self.args.cwd)
                os.execvpe(argv[0], argv, env)
            except Exception as exc:  # noqa: BLE001
                os.write(2, f"exec failed: {exc}\n".encode())
                os._exit(127)
        self.pid, self.fd = pid, fd

    def write(self, data: bytes) -> None:
        os.write(self.fd, data)

    def poll_exit(self) -> bool:
        if self.exit_status is not None:
            return True
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if pid:
            self.exit_status = os.waitstatus_to_exitcode(status)
            return True
        return False

    # ------------------------------------------------------------------ recording
    def record(self, direction: str, verb: str, params: dict, body) -> None:
        self.counts[f"{direction} {verb}"] = self.counts.get(f"{direction} {verb}", 0) + 1
        line = {"t": now_ms(), "dir": direction, "verb": verb, "params": params, "body": body}
        self.jsonl.write(json.dumps(line, ensure_ascii=False, separators=(",", ":")) + "\n")

    def send_tsp(self, verb: str, obj: dict) -> None:
        body = json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
        self.write(b"\x1b_tsp;" + verb.encode() + b";" + body.encode() + b"\x1b\\")
        self.record("in", verb, {}, obj)

    # ------------------------------------------------------------------ parsing
    def feed(self, data: bytes) -> None:
        if self.raw:
            self.raw.write(data)
        self.last_output = time.monotonic()
        self.buf += data
        buf, i, n = self.buf, 0, len(self.buf)
        while i < n:
            if buf[i] != 0x1B:
                j = buf.find(b"\x1b", i)
                i = n if j == -1 else j
                continue
            if i + 1 >= n:
                break
            kind = buf[i + 1]
            if kind in (0x5F, 0x5D, 0x50):  # APC, OSC, DCS
                end, size = self.find_terminator(buf, i + 2)
                if end is None:
                    if n - i > 32 << 20:  # give up on a runaway string
                        i = n
                    break
                payload = buf[i + 2 : end]
                if kind == 0x5F:
                    self.apc(payload)
                elif kind == 0x5D:
                    self.osc(payload)
                else:
                    self.dcs(payload)
                i = end + size
            elif kind == 0x5B:  # CSI
                j = i + 2
                while j < n and 0x30 <= buf[j] <= 0x3F:
                    j += 1
                while j < n and 0x20 <= buf[j] <= 0x2F:
                    j += 1
                if j >= n:
                    break
                self.csi(buf[i + 2 : j].decode("latin-1"), chr(buf[j]))
                i = j + 1
            else:
                i += 2
        self.buf = buf[i:]

    @staticmethod
    def find_terminator(buf: bytes, start: int) -> tuple[int | None, int]:
        st = buf.find(b"\x1b\\", start)
        bel = buf.find(b"\x07", start)
        if bel != -1 and (st == -1 or bel < st):
            return bel, 1
        if st != -1:
            return st, 2
        return None, 0

    def csi(self, params: str, final: str) -> None:
        reply: bytes | None = None
        if final == "c" and params in ("", "0"):
            reply = b"\x1b[?62;52c"
        elif final == "c" and params.startswith(">"):
            reply = b"\x1b[>1;10;0c"
        elif final == "n" and params == "6":
            reply = b"\x1b[1;1R"
        elif final == "n" and params == "5":
            reply = b"\x1b[0n"
        elif final == "u" and params == "?":
            reply = f"\x1b[?{self.kitty[-1]}u".encode()
        elif final == "u" and params.startswith(">"):
            self.kitty.append(int(params[1:] or 0))
        elif final == "u" and params.startswith("<"):
            for _ in range(int(params[1:] or 1)):
                if len(self.kitty) > 1:
                    self.kitty.pop()
        elif final == "u" and params.startswith("="):
            flags, _, mode = params[1:].partition(";")
            flags_n, mode_n = int(flags or 0), int(mode or 1)
            top = self.kitty[-1]
            self.kitty[-1] = flags_n if mode_n == 1 else (top | flags_n if mode_n == 2 else top & ~flags_n)
        elif final == "q" and params == ">":
            reply = b"\x1bP>|auk-recorder 0.0\x1b\\"
        elif final == "t" and params in ("14", "16", "18"):
            w, h = CELL
            reply = {
                "14": f"\x1b[4;{self.rows * h};{self.cols * w}t",
                "16": f"\x1b[6;{h};{w}t",
                "18": f"\x1b[8;{self.rows};{self.cols}t",
            }[params].encode()
        elif final in "hlmHJKABCDGfrsuSTXP@" or params.startswith("?") and final in "hl":
            return  # drawing and mode changes: not queries
        elif final in "cnqtp" or "$" in params:
            key = f"CSI {params}{final}"
            self.unanswered[key] = self.unanswered.get(key, 0) + 1
        if reply:
            self.write(reply)

    def osc(self, payload: bytes) -> None:
        text = payload.decode("utf-8", "replace")
        num, _, rest = text.partition(";")
        if num in ("10", "11", "12") and rest == "?":
            color = {"10": "e6e6/e6e6/e6e6", "11": "1c1c/1d1d/2121", "12": "e6e6/e6e6/e6e6"}[num]
            self.write(f"\x1b]{num};rgb:{color}\x1b\\".encode())
        elif num == "4" and rest.endswith(";?"):
            index = rest.split(";")[0]
            self.write(f"\x1b]4;{index};rgb:8080/8080/8080\x1b\\".encode())
        elif num in ("0", "1", "2", "7", "9", "133", "777"):
            self.record("out", "osc", {}, text)
        elif num == "1337":
            self.record("out", "osc", {}, text)
            if rest.startswith("SetUserVar="):
                name, _, value = rest[len("SetUserVar=") :].partition("=")
                try:
                    self.user_vars[name] = base64.b64decode(value).decode("utf-8", "replace")
                except (binascii.Error, ValueError):
                    self.user_vars[name] = value

    def dcs(self, payload: bytes) -> None:
        if payload.startswith(b"+q"):  # XTGETTCAP: report "not found"
            self.write(b"\x1bP0+r\x1b\\")

    # ------------------------------------------------------------------ TSP
    def apc(self, payload: bytes) -> None:
        if not payload.startswith(b"tsp;") or len(payload) < 6 or payload[5:6] != b";":
            return
        verb = chr(payload[4])
        rest = payload[6:]
        params: dict[str, str] = {}
        pos = 0
        while (m := PARAM.match(rest, pos)) is not None:
            params[m.group(1).decode()] = m.group(2).decode()
            pos = m.end()
        body = rest[pos:]
        if "c" in params:
            self.chunked(verb, params, body)
            return
        if self.chunk is not None:
            self.drop_chunk("interrupted")
        self.message(verb, params, body)

    def chunked(self, verb: str, params: dict, body: bytes) -> None:
        cid = params.pop("c")
        more = params.pop("m", None) == "1"
        if self.chunk and (self.chunk["verb"], self.chunk["c"]) != (verb, cid):
            self.drop_chunk("interrupted")
        if self.chunk is None:
            self.chunk = {"verb": verb, "c": cid, "params": params, "body": b""}
        self.chunk["body"] += body
        if not more:
            done, self.chunk = self.chunk, None
            self.message(done["verb"], done["params"], done["body"])

    def drop_chunk(self, why: str) -> None:
        chunk, self.chunk = self.chunk, None
        if chunk:
            self.send_tsp("e", {"ev": "error", "msg": f"chunked {chunk['verb']} message c={chunk['c']} dropped: {why}"})

    def message(self, verb: str, params: dict, body: bytes) -> None:
        if verb == "b":
            text = body.decode("ascii", "replace")
            self.record("out", "b", params, text)
            try:
                self.blobs.add(hashlib.sha256(base64.b64decode(text, validate=True)).hexdigest())
            except (binascii.Error, ValueError):
                self.send_tsp("e", {"ev": "error", "msg": "bad base64 in blob"})
            return
        try:
            obj = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            self.record("out", verb, params, body.decode("utf-8", "replace"))
            if verb in "qofbtsx":
                self.send_tsp("e", {"ev": "error", "msg": f"malformed {verb} body: {exc}"})
            return
        self.record("out", verb, params, obj)
        handler = getattr(self, f"on_{verb}", None)
        if handler and isinstance(obj, dict):
            handler(obj)

    def on_q(self, q: dict) -> None:
        if q.get("q") == "hello" and 1 in (q.get("v") or []):
            self.send_tsp("r", {
                "r": "hello", "v": 1, "term": "auk", "ver": "0.0.0-recorder", "kinds": KINDS,
                "features": FEATURES, "apc": 65536, "credits": 2, "cols": self.cols,
                "cell": {"w": CELL[0], "h": CELL[1]}, "dark": True, "reduceMotion": False,
                "hour12": False,
            })
        elif q.get("q") == "blobs":
            self.send_tsp("r", {"r": "blobs", "have": [i for i in q.get("ids") or [] if i in self.blobs]})

    def on_o(self, o: dict) -> None:
        sid = o.get("id")
        if not isinstance(sid, str):
            return
        listen = o.get("listen", True) is not False
        self.surfaces[sid] = {"mode": o.get("mode", "inline"), "listen": listen, "open": True}
        if listen:
            self.last_listening = sid
        self.opens.append(time.monotonic())

    def on_x(self, x: dict) -> None:
        if isinstance(x.get("id"), str) and x["id"] in self.surfaces:
            self.surfaces[x["id"]]["open"] = False

    def on_f(self, f: dict) -> None:
        sf, s = f.get("sf"), f.get("s")
        for op in f.get("ops") or []:
            self.apply(sf, op)
        self.frames.append((time.monotonic(), json.dumps(f, ensure_ascii=False)))
        surface = self.surfaces.get(sf)
        if surface and surface["listen"] and surface["open"] and not self.args.no_ack:
            self.pending_acks.append((time.monotonic() + self.args.ack_delay / 1000, sf, s))

    def apply(self, sf: str, op: list) -> None:
        if not isinstance(op, list) or not op:
            return
        name = op[0]
        if name == "add" and len(op) >= 5 and isinstance(op[4], dict):
            self.index(sf, op[4], op[2])
        elif name == "set" and len(op) >= 3 and isinstance(op[2], dict):
            node = self.nodes.get(f"{sf}/{op[1]}")
            if node:
                for key, value in op[2].items():
                    if value is None:
                        node["p"].pop(key, None)
                    else:
                        node["p"][key] = value
        elif name == "del" and len(op) >= 2:
            doomed = {f"{sf}/{op[1]}"}
            for key in list(self.nodes):  # nodes are indexed parent-first
                if self.nodes[key]["parent"] in doomed:
                    doomed.add(key)
            for key in doomed:
                self.nodes.pop(key, None)

    def index(self, sf: str, node: dict, parent) -> None:
        nid = node.get("id")
        if not isinstance(nid, str):
            return
        self.order += 1
        self.nodes[f"{sf}/{nid}"] = {"k": node.get("k"), "p": dict(node.get("p") or {}), "parent": f"{sf}/{parent}", "order": self.order}
        for child in node.get("c") or []:
            if isinstance(child, dict):
                self.index(sf, child, nid)

    # ------------------------------------------------------------------ driving
    def find(self, query: str) -> str:
        want = dict(part.split("=", 1) for part in query.split(","))
        for key, node in sorted(self.nodes.items(), key=lambda kv: kv[1]["order"]):
            if all((node["k"] if k == "k" else str(node["p"].get(k))) == v for k, v in want.items()):
                return key.split("/", 1)[1]
        raise LookupError(f"no node matches {query}")

    def resolve(self, value):
        if isinstance(value, dict):
            return {k: self.resolve(v) for k, v in value.items()}
        if isinstance(value, list):
            return [self.resolve(v) for v in value]
        if value == "$sf":
            return self.last_listening
        if isinstance(value, str) and value.startswith("$find:"):
            return self.find(value[len("$find:") :])
        return value

    def key_bytes(self, name: str) -> bytes:
        kitty = self.kitty[-1] & 1  # "disambiguate escape codes"
        if name == "enter":
            return b"\r"
        if name == "tab":
            return b"\t"
        if name == "backspace":
            return b"\x7f"
        if name == "escape":
            return b"\x1b[27u" if kitty else b"\x1b"
        if name.startswith("ctrl+") and len(name) == 6:
            letter = name[-1]
            return f"\x1b[{ord(letter)};5u".encode() if kitty else bytes([ord(letter) & 0x1F])
        arrows = {"up": "A", "down": "B", "right": "C", "left": "D"}
        if name in arrows:
            return f"\x1b[{arrows[name]}".encode()
        raise ValueError(f"unknown key {name}")

    def run(self, argv: list[str], steps: list[dict]) -> int:
        self.spawn(argv)
        deadline = time.monotonic() + self.args.timeout
        step_i, step_started, frames_at, opens_at = 0, time.monotonic(), 0, 0
        eof = False
        while time.monotonic() < deadline:
            ready, _, _ = select.select([] if eof else [self.fd], [], [], 0.02)
            if ready:
                try:
                    data = os.read(self.fd, 65536)
                except OSError:
                    data = b""
                if data:
                    self.feed(data)
                else:
                    eof = True
            now = time.monotonic()
            for ack in [a for a in self.pending_acks if a[0] <= now]:
                self.pending_acks.remove(ack)
                _, sf, s = ack
                if self.surfaces.get(sf, {}).get("open"):
                    self.send_tsp("e", {"ev": "ack", "sf": sf, "s": s})
                    if sf not in self.drawn:
                        self.drawn.add(sf)
                        self.send_tsp("e", {"ev": "resize", "sf": sf, "cols": self.cols, "cell": {"w": CELL[0], "h": CELL[1]}, "visible": True})
            exited = self.poll_exit()
            if step_i < len(steps):
                step = steps[step_i]
                done, timeout = False, step.get("timeout", 30)
                if "wait" in step:
                    what = step["wait"]
                    if what == "open":
                        done = len(self.opens) > opens_at
                    elif what == "frame":
                        needle = step.get("contains")
                        done = any(needle is None or needle in text for _, text in self.frames[frames_at:])
                    elif what == "idle":
                        done = now - self.last_output >= step.get("ms", 1000) / 1000
                    elif what == "exit":
                        done = exited
                    if not done and now - step_started > timeout:
                        print(f"step {step_i + 1} timed out: {step}", file=sys.stderr)
                        done = True
                elif "sleep" in step:
                    done = now - step_started >= step["sleep"] / 1000
                elif not exited:
                    if "keys" in step:
                        self.write(step["keys"].encode())
                    elif "key" in step:
                        self.write(self.key_bytes(step["key"]))
                    elif "event" in step:
                        self.send_tsp("e", self.resolve(step["event"]))
                    elif "signal" in step:
                        os.kill(self.pid, getattr(signal, f"SIG{step['signal']}"))
                    done = True
                else:
                    done = True
                if done:
                    step_i, step_started = step_i + 1, now
                    frames_at, opens_at = len(self.frames), len(self.opens)
            elif exited and eof:
                break
            elif exited and now - self.last_output > 0.2:
                break
        if not self.poll_exit():
            print("timeout: terminating the program", file=sys.stderr)
            os.kill(self.pid, signal.SIGTERM)
            time.sleep(1)
            if not self.poll_exit():
                os.kill(self.pid, signal.SIGKILL)
                os.waitpid(self.pid, 0)
        self.finish(argv)
        return 0

    def finish(self, argv: list[str]) -> None:
        self.jsonl.close()
        if self.raw:
            self.raw.close()
        meta = {
            "argv": argv,
            "cwd": self.args.cwd or os.getcwd(),
            "cols": self.cols,
            "rows": self.rows,
            "env": self.args.env,
            "recorded": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "recorder": "tools/tsp_record.py",
            "exit_status": self.exit_status,
            "counts": dict(sorted(self.counts.items())),
            "user_vars": self.user_vars,
            "unanswered_queries": self.unanswered,
            "note": self.args.note,
        }
        self.out_path.with_suffix(".meta.json").write_text(json.dumps(meta, indent=2) + "\n")
        print(json.dumps({"exit": self.exit_status, "counts": meta["counts"], "unanswered": self.unanswered}), file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--out", required=True, help="output path without extension")
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--scenario", help="JSON file with a list of steps")
    parser.add_argument("--env", action="append", default=[], help="extra K=V for the program")
    parser.add_argument("--cwd")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--ack-delay", type=float, default=4, help="ms between a frame and its ack")
    parser.add_argument("--no-ack", action="store_true")
    parser.add_argument("--raw", action="store_true", help="also save the raw PTY output")
    parser.add_argument("--note", default="")
    parser.add_argument("argv", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    argv = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
    if not argv:
        parser.error("missing program")
    steps = json.loads(Path(args.scenario).read_text()) if args.scenario else []
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    return Recorder(args).run(argv, steps)


if __name__ == "__main__":
    sys.exit(main())
