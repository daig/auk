# Third-party oracles

These are copies, not submodules, pinned to the commits below. Update them deliberately: the protocol changes inside v1 without version bumps.

| Directory | Source | Commit | License |
| --- | --- | --- | --- |
| `tern-sdk/` | https://github.com/stencil-hq/tern-sdk | `19658cb2a3205ce57c67ad8c5c1c54ed80fefa7e` (2026-10-07) | MIT |
| `oh-my-pi/` | https://github.com/can1357/oh-my-pi, TSP-related files only (`packages/wire`, parts of `packages/tui`) | `40e9368ef0458fd9073329cdff4174895f91bc6b` | MIT |

- The SDKs are both the program-side library and the test oracle. The Python SDK needs only the standard library: `PYTHONPATH=third_party/tern-sdk/python/src`.
- The installed omp binary (`~/.local/bin/omp`) is newer than the pinned source. Each recording in `fixtures/tsp/` records the version it came from.
