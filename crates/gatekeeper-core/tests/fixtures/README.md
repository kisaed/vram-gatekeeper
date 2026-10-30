# Real llama.cpp output fixtures

Captured from the bundled engine (llama.cpp **b11804**, CUDA 13.4 build, RTX 5090) on 2026-10-21.
They are real output, not hand-written, so tests built on them are not circular.

For the public source copy, machine-specific absolute Windows paths in the
captured load logs have been replaced with `<local-engine-path>`. The parser
signals and measurements are otherwise retained; these are historical fixtures,
not newly reproduced hardware test results.

| File | How it was produced |
|---|---|
| `llama-server-b11804-help.txt` | `llama-server --help` (stdout, 750 lines) |
| `llama-server-b11804-version.txt` | `llama-server --version` (**stderr**, stdout is empty) |
| `llama-server-b11804-load-default-verbosity.log` | Default `--verbosity 3`: **no layer-placement line** |
| `llama-server-b11804-load-full-offload.log` | `-ngl 99 -lv 4` with a 0.5B model: `offloaded 25/25 layers to GPU` |
| `llama-server-b11804-load-explicit-ngl10.log` | `-ngl 10 -lv 4`: `offloaded 10/25 layers to GPU` (user's own split) |
| `llama-server-b11804-load-fit-moved-all-to-cpu.log` | `--fit on --fit-target 31000 -lv 4` (forces fit to shed GPU layers): `offloaded 0/25 layers to GPU` |

Re-capture all of them whenever the pinned engine in `tools/engine-rules.json` changes.

For the public source copy, machine-specific absolute Windows paths in the captured load logs have been replaced with <local-engine-path>. Parser signals and measurements are otherwise retained. These are historical fixtures, not newly reproduced hardware results.

