# Woven

Woven is a Rust + `egui` notebook-style frontend for the Wolfram Language (Mathematica), with an explicit goal of replacing/modernizing plotting using `egui_plot`.

## Features

- **Notebook Interface**: Interactive cell groups with input and output, supporting execution states (idle, running, error).
- **Wolfram Kernel Integration**: Uses WSTP (Wolfram Symbolic Transfer Protocol) for reliable execution.
- **Plot Modernization**: Integrates `egui_plot` for fast and native data visualization.
- **Command Palette**: A quick-access menu (Esc) to trigger actions like evaluation, collapse/expand all outputs, and copy/paste as JSON.
- **Theming**: Configurable Dark and Light visual themes.
- **Autosave**: Best-effort automatic saving of the notebook context.

## Requirements

- A Wolfram installation that provides `WolframKernel` and WSTP (e.g., Wolfram Engine / Mathematica).
- `wolframscript` on `PATH` is strongly recommended (used by WSTP discovery/linking).

## Running

Config is required at `config/woven.toml` and defaults to `mode = "dev"` (which writes logs under `logs/`).

```bash
cargo run
```

## Configuration

Woven uses TOML for configuration. The configuration hierarchy is:
1. `config/woven.toml` (required)
2. `config/woven.local.toml` (optional, for local overrides and persisted theme settings)
3. Environment variables prefixed with `WOVEN__` (e.g., `WOVEN__UI__THEME=light`)
4. An optional extra TOML file path supplied via `WOVEN_CONFIG` environment variable.

## Logging

Logging is based on the `tracing` framework.
- By default, in `mode = "dev"`, a daily log file is written to `logs/woven.log`.
- Log level defaults to `info`.
- Both log levels and file paths can be overridden in the configuration file.

## Testing

```bash
cargo test
```

Kernel integration tests (if/when added) should be gated behind an env var like `WOVEN_TEST_KERNEL=1`.
