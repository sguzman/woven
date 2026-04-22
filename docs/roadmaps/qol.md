# Woven — Quality of Life roadmap

This roadmap covers UI/UX and workflow improvements that make Woven feel like a modern, high‑leverage Mathematica notebook frontend.

## Cell selection (multi-select)
- [ ] Add a checkbox on the left gutter for each cell
- [ ] Click checkbox toggles that cell’s membership in the selection set
- [ ] Ctrl+click toggles cell membership without clearing existing selection
- [ ] Shift+click selects a contiguous range (anchor + target)
- [ ] Add “Select all / Select none / Invert selection” actions
- [ ] Show selection count in the toolbar/status bar

## Cell operations (batch)
- [ ] Delete selected cells (with confirmation)
- [ ] Duplicate selected cells (preserve relative order)
- [ ] Move selected cells up/down as a block
- [ ] Copy selected cells to clipboard (structured text format)
- [ ] Paste cells from clipboard into notebook
- [ ] Export selected cells (e.g., JSON, plain text)
- [ ] Clear outputs for selected cells (keep inputs)

## Cell grouping (Mathematica-style)
- [ ] Model cell groups: input cell + associated output cells (as a unit)
- [ ] Single selection can target either group or individual member
- [ ] Copy/cut acts on the whole group by default (configurable)
- [ ] Collapse/expand group (hide outputs) with a right-side toggle
- [ ] Per-cell collapse (hide a single output cell) inside a group
- [ ] “Collapse all outputs / Expand all outputs” actions
- [ ] Persist collapsed state in notebook file

## Notebook tabs (sessions)
- [ ] Add tab bar UI
- [ ] `Ctrl+N` opens a new tab (new notebook + new kernel session)
- [ ] `Ctrl+W` closes current tab (prompt to save if dirty)
- [ ] `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle tabs
- [ ] Each tab has independent: cells, history, kernel session, config overrides
- [ ] Per-tab restart kernel and per-tab logs context/span fields

## Editing ergonomics
- [ ] Command palette (e.g., `Ctrl+P`) for actions (evaluate, move, copy, collapse)
- [ ] Keyboard navigation between cells (up/down, jump to input)
- [ ] Auto-focus new cell input and keep cursor position stable
- [ ] Snippets / templates for common WL constructs
- [ ] Input formatting helpers (indentation / bracket matching)

## Execution flow UX
- [ ] Visual running indicator + duration per cell/group
- [ ] Evaluation queue (evaluate multiple cells; show pending/running/completed)
- [ ] Cancel evaluation (send WSTP abort message)
- [ ] Evaluate visible / evaluate selection
- [ ] Rerun last evaluation in tab

## Search & organization
- [ ] Search across notebook inputs/outputs
- [ ] Filter to cells with errors/messages
- [ ] Tagging / bookmarks for cells
- [ ] Outline panel (cell headings) for long notebooks

## Reliability & safety
- [ ] “Dirty” state tracking with autosave option
- [ ] Crash recovery (write journal / periodic snapshot)
- [ ] Guardrails against accidental mass delete
- [ ] Configurable limits (max output size, max messages) with truncation UI

