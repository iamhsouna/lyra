# ✦ Lyra

**Write a song in your terminal.** Lyra is a local, full-control app for
[MiniMax-Music3](https://huggingface.co/audio-cpp/MiniMax-Music3-GGUF) running on
[`audio.cpp`](https://github.com/0xShug0/audio.cpp) — with a terminal UI **and**
a web UI that share one core: the same fields, presets, validation, and runner.

No cloud, no Python, no DAW. Pick a style and mood, edit your lyrics, hit
generate, and a `.wav` lands on your GPU.

```
┌ ✦ Lyra   1 Song │ 2 Advanced │ 3 Components │ 4 Run │ 5 Presets │ 6 Help ┐
├───────────────────────────────────┬───────────────────────────────────────┤
│  ▶ Style        bright pop rock…  │  Caption                              │
│    Mood         uplifting         │  Bright pop rock with clean drums …   │
│    Vocals       a clear female …  │  — uplifting mood, a clear female     │
│    Caption o…   (empty)           │  vocal, polished studio production.   │
│    Lyrics       [verse] City li…  │                                       │
│    Length (s)   30                │  Lyrics                               │
│    Steps        30                │  [verse] City lights are shining low  │
│    Output       ~/Music/lyra-…    │  I keep moving with the glow          │
│                                   │  [chorus] Turn it up and let it fly   │
├───────────────────────────────────┴───────────────────────────────────────┤
│ Tab switch · ↑/↓ field · ←/→ adjust · Enter edit · g generate · Ctrl-Q quit│
└────────────────────────────────────────────────────────────────────────────┘
```

## Features

- **Two front-ends, one core** — a tabbed TUI and a browser UI over the same
  `Config`, command builder, presets, and job runner.
- **All the model controls** — guidance scales, top-k, seed, flow/perf toggles,
  memory options, backend, and per-component GGUF selection.
- **A workflow that isn't a straightjacket** — edit any setting in any order;
  go back and forth freely; no forced question sequence.
- **Live generation** — the audio.cpp log streams into the UI with a spinner and
  elapsed time; abort any time; regenerate with one key.
- **Presets & history** — save named configs and review past runs
  (`~/.config/lyra/`).
- **Zero-config detection** — finds the runtime, model directory, backend, and
  the best available component weights automatically.
- **Single static-ish binary** — the web server is embedded; `lyra web` just
  works.

## Requirements

| | |
| --- | --- |
| Runtime | [`audio.cpp`](https://github.com/0xShug0/audio.cpp) built with `audiocpp_cli` |
| Model | MiniMax-Music3-GGUF components (LM + flow + depth + condition encoder + vocoder + configs) |
| GPU | NVIDIA (CUDA), AMD (HIP/ROCm), Apple (Metal) — CPU works but is much slower than realtime |
| Rust | 1.80+ (edition 2024; built and tested on 1.98) |

Quick setup:

```bash
# 1. build the runtime (CUDA on Linux, Metal on macOS, --backend cpu/vulkan/hip)
git clone https://github.com/0xShug0/audio.cpp
cd audio.cpp
git -c 'url.https://github.com/.insteadOf=git@github.com:' submodule update --init --recursive
scripts/build_linux.sh --backend cuda --target audiocpp_cli   # macOS: scripts/build_metal.sh --target audiocpp_cli

# 2. fetch the model (~9 GB) with audio.cpp's model manager
python3 tools/model_manager_v2.py install minimax_music3_q4_0
```

Lyra looks for the binary under `~/audio.cpp/build/*-release/bin/audiocpp_cli`
and the model in `~/models/MiniMax-Music3-GGUF`. Override with flags (below);
if you installed the model elsewhere, pass `--model <dir>`.

## Install

```bash
git clone https://github.com/iamhsouna/lyra
cd lyra
cargo build --release
# optional: install to ~/.cargo/bin
cargo install --path .
```

## Usage — TUI

```bash
lyra
```

| Key | Action |
| --- | --- |
| `Tab` / `Shift-Tab` / `1`-`6` | switch tabs |
| `↑` `↓` (or `j` `k`) | move between fields |
| `←` `→` (or `h` `l`) | adjust / cycle the selected value |
| `Enter` | edit text · toggle a switch · cycle a list |
| `Space` | toggle / cycle |
| `g` or `Ctrl-G` | generate |
| `a` | abort the running job |
| `r` | reset to defaults |
| `?` | open the help tab |
| `s` (Run tab) | reroll the seed |
| `n` (Run tab) | new output path |
| `Ctrl-Q` / `Ctrl-C` | quit (aborts if generating) |

Text editing: `Enter` saves, `Esc` cancels. The lyrics editor treats `Enter`
as a new line and `Tab` (or `Ctrl-Enter`) as save.

### Tabs

| Tab | Contents |
| --- | --- |
| **1 Song** | style, mood, vocals, caption override, lyrics, length, steps, output path |
| **2 Advanced** | flow/AR guidance, top-k, seed, flow-uncond interval/warmup, chunk hop, ensemble takes/prefix, mem-saver, pipeline overlap, graph/weight context |
| **3 Components** | backend + which language-model / flow-transformer / depth-decoder GGUF to load |
| **4 Run** | resolved command + warnings, live log, status, regenerate |
| **5 Presets** | save / load / delete named configs |
| **6 Help** | workflow and every option explained |

## Usage — Web UI

```bash
lyra web                 # http://127.0.0.1:8282
lyra web --host 0.0.0.0 --port 9000
```

The page mirrors the TUI: the same setting tabs and preview, a **Generate** /
**Abort** bar, live log streaming, preset save/load/delete, a download link for
the finished `.wav`, and recent-run history. Keyboard: `1`-`6` switch tabs,
`Ctrl+Enter` generates, `Esc` aborts.

> The web UI has no authentication. Bind to `127.0.0.1` (the default) or only
> expose it on a trusted network.

## Options

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bin <path>` | auto-detect | `audiocpp_cli` binary |
| `--model <dir>` | `~/models/MiniMax-Music3-GGUF` | model directory |
| `--backend <name>` | inferred from the binary | `cuda` / `metal` / `vulkan` / `hip` / `cpu` |
| `--host <addr>` | `127.0.0.1` | web bind address |
| `--port <port>` | `8282` | web bind port |
| `--dry-run` | | print the `audiocpp_cli` command and exit |
| `--print-config` | | print resolved paths/components + warnings, then exit |
| `-h`, `--help` | | help |

### Song settings

| Field | Default | Notes |
| --- | --- | --- |
| Style | bright pop rock | caption style; the last option is `custom...` |
| Mood | uplifting | one word added to the caption |
| Vocals | a clear female vocal | last option is instrumental |
| Caption override | *(empty)* | if set, replaces the composed caption |
| Lyrics | a short starter | `[verse]`/`[chorus]`/`[bridge]`/`[outro]` tags supported |
| Length (s) | `30` | AR frame budget — a cap, not a hard length |
| Steps | `30` | flow-matching Euler steps per chunk (lower = faster) |
| Output file | `~/Music/lyra-<epoch>.wav` | `~` expands to `$HOME` |

### Advanced settings

| Field | Default | Notes |
| --- | --- | --- |
| Flow guidance | `1.7` | flow CFG scale; `0` disables CFG |
| AR guidance | `1.5` | semantic/depth AR CFG scale; `0` disables |
| Top-k | `50` | sampling for semantic and residual codes |
| Seed | `0` | change it to reroll |
| Flow uncond interval | `1` | evaluate the flow uncond branch every N steps (`2`-`3` = faster) |
| Flow uncond warmup | `2` | steps per chunk that always evaluate both CFG branches |
| Flow chunk hop | `0` | AR frames (`0` = model default; `150` = faster) |
| Ensemble takes | `1` | generate K takes sharing one AR pass |
| Ensemble prefix frames | `0` | shared AR prefix across takes (intro lock) |
| Mem saver | `true` | load big stages only when needed (lower peak VRAM) |
| Pipeline overlap | `false` | overlap AR with denoise/vocode (**needs mem saver off**) |
| Graph ctx (MiB) | `32` | runtime graph arena size |
| Weight ctx (MiB) | `32` | weight context size |

### Components

| Field | Default | Notes |
| --- | --- | --- |
| Backend | auto | `cuda` / `metal` / `vulkan` / `hip` / `cpu` |
| Language model | best on disk | prefers `q4_k`, then `q8_0`, `q4_0`, `bf16` |
| Flow transformer | best on disk | same preference |
| Depth decoder | best on disk | prefers `q8_0`, then `q4_k`, `bf16` |

## Presets and history

Both live under `~/.config/lyra/` (or `$XDG_CONFIG_HOME/lyra/`):

- `presets.json` — a map of name → full config.
- `history.json` — the last 100 runs (`at`, `out`, `caption`, `success`, `wall_ms`).

## HTTP API

Local JSON API used by the web UI:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/` | the single-page UI |
| `GET` | `/api/health` | liveness probe (`{ok, version}`) |
| `GET` | `/api/state` | fields, values, options, preview, warnings/errors, paths, presets, history, status |
| `GET` | `/api/log?since=N` | incremental job log: `{job_id, next, lines, running, status, success, out, wall_ms, exists}` |
| `POST` | `/api/field` | `{id, action: "cycle-"\|"cycle+"\|"toggle"\|"reroll"\|"set", value?}` |
| `POST` | `/api/select` | `{id, index}` (select fields; `0` = auto where applicable) |
| `POST` | `/api/generate` | start a job (409 if one is running, 400 with `errors` if setup is incomplete) |
| `POST` | `/api/abort` | kill the running job |
| `POST` | `/api/reset` | restore default settings (keeps the output path) |
| `GET` | `/api/download?path=…` | download a `.wav` under `$HOME` |
| `POST` | `/api/presets/save` | `{name}` → save current config |
| `POST` | `/api/presets/load` | `{name}` |
| `POST` | `/api/presets/delete` | `{name}` |

The browser UI streams the log by polling `/api/log` (incremental, cursor-based).
`tiny_http` buffers chunked responses, so a long-lived Server-Sent Events stream is
not reliable — polling is used instead.

## Architecture

```
src/
  core.rs     Config, the field model, options catalog, argv builder,
              validation, presets, history
  runner.rs   spawns audiocpp_cli, streams its log, tracks status/abort
  tui.rs      tabbed terminal UI (ratatui + crossterm)
  web.rs      tiny_http server + embedded single-page UI (incremental log poll)
  detect.rs   finds the binary, model dir, backend, and component GGUFs
  main.rs     CLI parsing, TUI event loop, `web` subcommand
```

Lyra never links `audio.cpp`: it shells out to `audiocpp_cli` with an explicit
argv, so the runtime keeps its own update cadence and Lyra stays a small,
easy-to-audit binary.

## Troubleshooting

- **"audiocpp_cli not found"** — build the runtime (see Requirements) or pass
  `--bin`.
- **"no language model / flow / depth GGUF in model dir"** — download the model
  components; see the model card.
- **CUDA build fails with `__cxa_call_terminate`** — CUDA 12.x with a host GCC
  newer than 13; build with `g++-13` (the updater does this automatically).
- **CPU-only build** — the runtime was built without `--cuda`; rebuild.
- **Generation is slow** — lower `Steps`, `Length`, or set
  `Flow uncond interval` to `2`-`3`; enable `Pipeline overlap` (with mem saver
  off) if you have VRAM headroom.

## Related

- [`audio.cpp`](https://github.com/0xShug0/audio.cpp) — the inference runtime.
- [`MiniMax-Music3-GGUF`](https://huggingface.co/audio-cpp/MiniMax-Music3-GGUF) — the weights.
- [`MiniMax-Music3`](https://huggingface.co/MiniMaxAI/MiniMax-Music3) — the upstream model.
