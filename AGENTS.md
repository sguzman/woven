# Woven agent conventions

This repo will be worked on by multiple agents and humans. Keep changes small, testable, and observable.

## Non-negotiables

- Always run `cargo test` and `cargo build` after changes.
- Prefer automated tests; do not rely on manual QA.
- Use `tracing` for logs/spans; avoid `println!` except in throwaway experiments.
- Add new behavior behind config whenever it affects policies/parameters/tuning.

## Logging conventions

- Use spans for lifecycles: app startup, kernel session, evaluation requests.
- Include stable identifiers in fields (e.g., `eval_id`, `cell_id`).
- In `mode = "dev"`, logs must be written to `logs/` in addition to stdout.

## Config conventions

- Config is TOML.
- Precedence:
  1) `config/woven.toml` (required)
  2) `config/woven.local.toml` (optional; gitignored)
  3) `WOVEN__...` environment variables (optional)
- Keep config keys stable; prefer adding new keys over changing meaning.

## Docs and roadmaps

- Put docs under `docs/`.
- Put feature roadmaps under `docs/roadmaps/`.
- Roadmaps use Markdown checkboxes (`- [ ]` / `- [x]`) and should be kept up-to-date.

