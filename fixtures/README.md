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
| `omp-session` | omp 18.8.4, three prompts (an overview, an edit with a diff, a shell command) in a scratch copy of tern-sdk, under `tools/tsp_record.py` with `scenarios/omp-session.json`. About 99 s and 559 frames. |
| `omp-session-own` | omp's own log of the same session (`PI_TUI_TSP_RECORD`), for cross-checking the recorder. |

Re-record with the commands in each `.meta.json` (`argv`, `cwd`, `env`). Recordings of model output aren't reproducible; keep the originals.

## `scenarios/`

Step lists for `tools/tsp_record.py --scenario` (see the tool's docstring).
