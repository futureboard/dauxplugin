# DAUxHost

**DAUxHost.exe** loads and runs `.dauxplug` plugins **out-of-process** from the
DAW, hosts their editors, and can scan plugin metadata. Running plugins in a
separate process means a plugin crash cannot take down Futureboard, and lets the
host bridge plugins written in different languages/runtimes.

## Modes

```
DAUxHost --mode=MODE [options]
```

| Mode | Status | What it does |
|---|---|---|
| `--mode=scan`     | ✅ implemented | Scan a dir / single plugin, dump metadata (text or `--json`). |
| `--mode=headless` | ✅ implemented | Load, `prepare`, process one passthrough block, report. (Default.) |
| `--mode=plugin`   | 🚧 skeleton | Load + serve audio/param/state over IPC. |
| `--mode=editor`   | 🚧 skeleton | Load + host the editor window; reparent into the DAW. |

Options: `--plugin=PATH`, `--scan-dir=DIR`, `--ipc=NAME`, `--sample-rate=N`,
`--block-size=N`, `--json`, `--help`.

`DAUxScan` is a separate, smaller executable that reuses the loader + scanner
(no editor/IPC code) for fast metadata enumeration:

```
DAUxScan [--json] <dir>
DAUxScan [--json] --plugin <file>
```

## Examples

```powershell
# Headless self-test (the first-milestone smoke test):
DAUxHost.exe --mode=headless --plugin=plugins\daux_gain_cpp.dauxplug

# Metadata as JSON:
DAUxHost.exe --mode=scan --plugin=plugins\daux_gain_cpp.dauxplug --json

# Scan a folder:
DAUxScan.exe plugins
```

## Architecture

```
                +------------------ DAW / Futureboard ------------------+
                |                                                       |
                |   process bridge (control: named pipe;                |
                |   audio: shared-memory ring)   <----- IPC ----->      |
                +-------------------------------------------------------+
                                         |
                                         v
        +------------------------- DAUxHost.exe -------------------------+
        |  main.cpp        CLI + mode dispatch                          |
        |  host_runtime    run_headless / run_plugin / run_editor       |
        |  plugin_loader    metadata dump (uses daux_core PluginModule) |
        |  plugin_scan      recursive *.dauxplug scanner                |
        |  plugin_editor    editor window hosting (skeleton)            |
        |  ipc_server       transport seam (skeleton)                   |
        +---------------------------------------------------------------+
                                         |
                                         v
                          .dauxplug  (C++ / Rust / .NET)
```

The actual loading, ABI validation, and RAII lifetime management live in
`daux_core` (`daux::PluginModule` / `daux::PluginInstance`), so DAUxHost code
always speaks in terms of `.dauxplug` regardless of the underlying OS module.

## IPC protocol (planned)

> Not yet implemented — `ipc_server.cpp` is the seam. This documents intent.

- **Control channel**: a duplex named pipe (`\\.\pipe\daux-<endpoint>`) carrying
  framed, length-prefixed messages (create / prepare / set-param / get-state /
  shutdown and their replies). Format TBD (likely a compact binary header +
  payload).
- **Audio channel**: a shared-memory ring buffer for `process()` so audio data
  never travels through the pipe. The DAW writes inputs + a
  `daux_process_context`, signals the host, the host runs `process()` in place,
  and signals completion.
- **Editor**: `--mode=editor` creates a top-level window, calls
  `create_editor` → `attach(hwnd)` → `show`, pumps the OS message loop +
  editor `idle()`, and **reparents** the editor window into a host-provided
  HWND across the process boundary. `resize_editor` host callbacks are
  forwarded to the window.

## Roadmap

1. Named-pipe control transport + message schema.
2. Shared-memory audio ring + realtime-safe handoff.
3. Editor window hosting and cross-process reparenting.
4. Crash detection / supervision + automatic restart.
5. Sandboxing (job objects / restricted tokens) for untrusted plugins.
