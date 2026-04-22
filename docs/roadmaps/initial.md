# Woven v0 roadmap

## Notebook model
- [ ] Multiple input/output cells with stable IDs
- [ ] Cell reordering
- [ ] Persistent notebook format (TOML/JSON v0)
- [ ] Re-evaluate cell / evaluate all
- [ ] Cell status + duration display

## Kernel (WSTP)
- [ ] Enforce `kernel.start_timeout_ms` (no infinite wait)
- [ ] Enforce `kernel.eval_timeout_ms` (cancel/abort behavior)
- [ ] Better `MESSAGEPKT` formatting (human readable)
- [ ] Capture `$Messages` / stderr-ish channels reliably
- [ ] Session reset / kernel restart UI

## Plot interception (egui_plot)
- [ ] Detect plot-like outputs (Graphics/GraphicsBox/ListPlot/Plot)
- [ ] Define internal `PlotPayload` schema (series, styles, axes, ranges)
- [ ] WL-side normalization function for extraction (prototype)
- [ ] Render common plot types with `egui_plot` (points/lines)
- [ ] Interactive pan/zoom defaults and tuning via `[plot]` config

## UI/UX
- [ ] Keyboard shortcuts (evaluate cell, new cell, focus movement)
- [ ] Search within notebook
- [ ] Collapsible outputs
- [ ] Rich output types (tables, images)

## Testing / CI
- [ ] Unit tests for config precedence/validation
- [ ] Integration test for kernel eval gated by env (`WOVEN_TEST_KERNEL=1`)
- [ ] Add GitHub Actions (fmt, clippy, test, build)

