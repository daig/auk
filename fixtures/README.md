# Fixtures

## `tsp/`: recorded TSP sessions

Each `.jsonl` file uses Tern's recording format (book:11579): one message per line, `{"t","dir","verb","params","body"}`. `out` lines are what the program wrote; `in` lines are what the terminal answered. `.pty` files hold the raw bytes the program wrote, and `.meta.json` files hold the command, size, environment and per-verb counts.

| Fixture | Source |
| --- | --- |
| `docs-ask` | The book's canonical CLI session (book:10827-10871), taken by `tools/docs_fixtures.py`. The `s` and `f` bodies come from the blocks that spell out the elided CSS and form. |
| `docs-ls` | The book's `listen:false` table (book:11134-11149). |
| `docs-regions` | The book's inline surface with `main`, `dock` and `layer` (book:11205-11221). The `o` is implied by the book and added. |
| `docs-frames` | The book's two frames to a flow surface (book:11298-11314). The `o` is implied by the book and added. |
| `python-{table,ask,progress,chat}` | The tern-sdk Python examples at `19658cb`, run under `tools/tsp_record.py` with the scenarios in `scenarios/`. |
| `omp-session` | omp 18.8.5 (re-recorded 2026-10-08; the 18.8.4 recording is in git history at `75f3bb6`), three prompts (an overview, an edit with a diff, a shell command) in a scratch copy of tern-sdk, under `tools/tsp_record.py` with `scenarios/omp-session.json`. About 91 s and 541 frames. |
| `omp-session-own` | omp's own log of the same session (`PI_TUI_TSP_RECORD`), for cross-checking the recorder. omp appends to that file, so empty it before re-recording. |
| `omp-picker` | omp 18.8.5, no prompts (recorded 2026-10-08), under `tools/tsp_record.py` with `scenarios/omp-picker.json` and the recorder's hello, which advertises `picker`: `/model` opens the model hub as a `picker` sheet, `son` is typed, Down/Down/Up, Enter opens the role strip, Right/Right/Left move its chip, Esc cancels it, three backspaces and Esc close the hub; then a `status.model` action on the composer's model control opens the md "Switch model" sheet, `gpt` is typed (no match: the empty state), Down/Down, Esc clears and Esc closes. Nothing is assigned or switched (`~/.omp/agent/config.yml` unchanged). 28 frames. |

Re-record with the commands in each `.meta.json` (`argv`, `cwd`, `env`). Recordings of model output aren't reproducible; keep the originals.

## `terminal/`: recorded terminal sessions

Raw output of real programs in a PTY, for the `terminal` suite's reference grid (Auk against `alacritty_terminal`). Each `<name>.vt.jsonl` holds, in stream order, the program's reads (`{"t","out":<base64>}`), the resizes (`{"t","resize":[cols,rows]}`) and what the scenario typed (`{"t","in"}`, not replayed). `<name>.meta.json` holds the command, size, environment, program version, scenario and counts. `tools/vt_record.py` records them; `terminal/record.sh [name…]` re-records with the exact commands, each in a fresh temporary `HOME`.

| Recording | Program |
| --- | --- |
| `vim` | vim 9.1 with `terminal/vimrc` (syntax colors in truecolor, splits) editing `sample.rs`, resized twice |
| `less` | `less -R` over `less-input.sh`'s colored, wide and tabbed text: search, chop toggle, resizes |
| `htop` | htop 3.5 with no config, showing itself and three `sleep`s: tree view, setup screen, resizes |
| `gitlog` | `git log --color --graph --decorate --stat --all` over the repository `gitlog.sh` builds with fixed dates, paged by less |
| `tmux` | tmux 3.7 with `-f /dev/null` on its own socket, `zsh -f` in each pane: two splits, zoom, resizes |
| `wide`, `emoji` | `zsh -f` printing `wide.txt` (CJK, fullwidth, combining marks, Hangul jamo) and `emoji.txt` (skin tones, ZWJ, flags, keycaps, VS16), resized |
| `resize` | `zsh -f` with colored wrapped output, resized through seven sizes |

htop and tmux show the time, so re-recording gives different bytes; the committed recordings are the reference inputs.

## `scenarios/`

- `*.json`: step lists for `tools/tsp_record.py --scenario` (see the tool's docstring).
- `*.auk`: scenario scripts for `auk shot` (spec §11.3; syntax in `crates/cli/src/scenario.rs`). The `cli` suite runs each in a hermetic HOME (the test shell config, plus `sdk`, `python3` and `w/` for `sdk-table`) and compares every `shot` with `goldens/`.

## `goldens/`

`<scenario>/<shot>.png` and `.json` (the layout): what `auk shot` drew, offscreen with wgpu. Tolerances are in `crates/conformance/src/goldens.rs`. Rewrite them only deliberately, after looking at the new PNGs: `AUK_UPDATE_GOLDENS=1 cargo nextest run -p auk --test cli scenario_`.

## Fonts

`fonts/Ahem.ttf` is the public-domain Ahem test font from web-platform-tests (`fonts/Ahem.ttf`), fetched 2026-10-08. The layout suite sets its probe pages in it so text metrics are exact in both WebKit and Auk.
