# Woven — Quality of Life roadmap

This roadmap covers UI/UX and workflow improvements that make Woven feel like a modern, high‑leverage Mathematica notebook frontend.

## Cell selection (multi-select)
- [x] Add a checkbox on the left gutter for each cell
- [x] Click checkbox toggles that cell’s membership in the selection set
- [x] Ctrl+click toggles cell membership without clearing existing selection
- [x] Shift+click selects a contiguous range (anchor + target)
- [x] Add “Select all / Select none / Invert selection” actions
- [x] Show selection count in the toolbar/status bar

## Cell operations (batch)
- [x] Delete selected cells (with confirmation)
- [x] Duplicate selected cells (preserve relative order)
- [x] Move selected cells up/down as a block
- [x] Copy selected cells to clipboard (structured text format)
- [x] Paste cells from clipboard into notebook
- [x] Export selected cells (e.g., JSON, plain text)
- [x] Clear outputs for selected cells (keep inputs)

## Cell grouping (Mathematica-style)
- [x] Model cell groups: input cell + associated output cells (as a unit)
- [x] Single selection can target either group or individual member
- [x] Copy/cut acts on the whole group by default (configurable)
- [x] Collapse/expand group (hide outputs) with a right-side toggle
- [x] Per-cell collapse (hide a single output cell) inside a group
- [x] “Collapse all outputs / Expand all outputs” actions
- [x] Persist collapsed state in notebook file

## Notebook tabs (sessions)
- [x] Add tab bar UI
- [x] `Ctrl+N` opens a new tab (new notebook + new kernel session)
- [x] `Ctrl+W` closes current tab (prompt to save if dirty)
- [x] `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle tabs
- [x] Each tab has independent: cells, history, kernel session, config overrides
- [x] Per-tab restart kernel and per-tab logs context/span fields

## Editing ergonomics
- [x] Command palette (e.g., `Ctrl+P`) for actions (evaluate, move, copy, collapse)
- [x] Keyboard navigation between cells (up/down, jump to input)
- [x] Auto-focus new cell input and keep cursor position stable
- [x] Snippets / templates for common WL constructs
- [x] Input formatting helpers (indentation / bracket matching)

## Execution flow UX
- [x] Visual running indicator + duration per cell/group
- [x] Evaluation queue (evaluate multiple cells; show pending/running/completed)
- [x] Cancel evaluation (send WSTP abort message)
- [x] Evaluate visible / evaluate selection
- [x] Rerun last evaluation in tab

## Search & organization
- [x] Search across notebook inputs/outputs
- [x] Filter to cells with errors/messages
- [x] Tagging / bookmarks for cells
- [x] Outline panel (cell headings) for long notebooks

## Reliability & safety
- [x] “Dirty” state tracking with autosave option
- [x] Crash recovery (write journal / periodic snapshot)
- [x] Guardrails against accidental mass delete
- [x] Configurable limits (max output size, max messages) with truncation UI
