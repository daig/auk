# Auk

Auk is a terminal for macOS whose sessions outlive their windows, and in which terminal programs can draw native UI.

- **Native UI for terminal programs.** Over the Tern Surface Protocol (TSP v1), a program describes its interface in-band with elements and CSS, and Auk draws it on the GPU. The same program prints plain text in any other terminal. The [omp](https://github.com/can1357/oh-my-pi) coding agent works unmodified: its transcript, tool cards, diffs, pickers, settings and composer are drawn as real UI.
- **Sessions outlive their windows.** A session daemon owns every pane. Quit the app and your programs keep running; reopen it and they're where you left them. Panes come back after a crash or a reboot, and `auk daemon upgrade` swaps in a new build without stopping anything.
- **The same session anywhere.** `auk web serve` opens a live pane in the browser over WebGPU, native UI included. `auk --host HOST` gives native windows on a remote daemon over SSH, and survives a dropped link.
- **Plugins in Luau.** Command lenses turn a command's plain output into a native view. Blocks are panes made of UI, and window plugins add keys, commands and styles. Plugins hot-reload on save, and a plugin's stylesheet can restyle anything Auk draws.
- **Scriptable.** The `auk` CLI drives the live daemon: `new`, `split`, `run`, `send`, `wait`, `capture` and `events`.
- **A good terminal first.** It has GPU rendering, accurate emulation (vim, less, htop and tmux), find, jumping between prompts, hyperlinks, IME, Option-as-Alt and VoiceOver.

Auk is a clean-room implementation of the design of [Tern](https://stencil.so/tern): the same protocol and plugin API, written from Tern's [public docs](https://docs.stencil.so/tern/) and SDKs. Programs built with the [Tern SDKs](https://github.com/stencil-hq/tern-sdk) and Tern's plugins run in Auk as they are.

## Requirements

Auk is built for macOS 27 on Apple silicon, and is tested on M5-series Macs. The daemon also runs on Linux, as the remote end of `auk --host`.

## Build and install

```sh
source scripts/env.sh   # rustup's toolchain, with the wasm target
scripts/bundle.sh       # builds and signs target/Auk.app
open target/Auk.app     # or copy it to /Applications first
```

`bundle.sh` builds the release `auk` and the browser client. It then signs the app ad hoc with the hardened runtime and the `allow-jit` entitlement (`crates/auk/Auk.entitlements`) that Luau's native code needs.

The CLI is the same binary: run `target/Auk.app/Contents/MacOS/auk`, or symlink it onto your `PATH`. `auk doctor` reports the build and whether native code runs.

## A short tour

```sh
auk ls                          # sessions, tabs and panes
pane=$(auk new)                 # a new tab; prints its pane id
auk run $pane 'make test'       # type a command into it
auk wait --until prompt $pane   # wait until the prompt is back
auk capture $pane               # print what's on screen
auk split $pane right           # a split beside it
auk web serve                   # a URL that opens this machine's sessions in a browser
```

`auk help` lists every verb.

## Plugins

A plugin is a directory with a `plugin.toml` and Luau scripts: a host half (lenses and blocks, run by the daemon) and a window half (keys, commands and styles, run by each window). Install or link one with `auk plugin install SRC` or `auk plugin link DIR`; `auk plugin dir` prints where they live.

The examples in `third_party/tern-sdk/plugins/examples` run in Auk. Among them are a `terraform plan` lens, a JSON explorer block and a pull-request queue. `third_party/tern-sdk/plugins/tern.d.luau` holds the API's types.

## Programs with native UI

Any program can draw native UI with the Tern SDKs for Python, Rust, TypeScript and Go, which are vendored in `third_party/tern-sdk`. Try an example:

```sh
PYTHONPATH=third_party/tern-sdk/python/src python3 third_party/tern-sdk/python/examples/table.py
```

## Development

`scripts/verify.sh` runs every suite: conformance against the SDKs and recorded omp sessions, emulation against `alacritty_terminal`, layout against WebKit, rendering goldens, persistence, replication, plugins, the CLI, the browser client and SSH. It also checks performance. It writes `target/verify/summary.txt`.

| Path | What |
| --- | --- |
| `crates/` | the Cargo workspace; `crates/auk` is the binary |
| `fixtures/` | recorded and hand-written test inputs (see `fixtures/README.md`) |
| `third_party/` | the pinned Tern SDKs and omp sources, the test oracles (see `third_party/README.md`) |
| `tools/` | the TSP and terminal recorders |
| `scripts/` | the build, environment and verification scripts |

## License

Auk is MIT-licensed; see `LICENSE`. The vendored `third_party/` sources keep their own licenses (MIT). The bundled Nerd Fonts symbols are under the license in `crates/text/assets/`.
