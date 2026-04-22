use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::{debug, error, instrument};

use crate::{
    config::{AppConfig, Theme},
    kernel::{EvalResult, KernelSession},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Variables,
    Documentation,
}

#[derive(Debug, Clone)]
struct UiPalette {
    background: egui::Color32,
    panel: egui::Color32,
    panel_alt: egui::Color32,
    card: egui::Color32,
    card_hover: egui::Color32,
    stroke: egui::Stroke,
    subtle_stroke: egui::Stroke,
    text_dim: egui::Color32,
}

impl UiPalette {
    fn for_theme(dark: bool) -> Self {
        if dark {
            Self {
                background: egui::Color32::from_rgb(20, 20, 22),
                panel: egui::Color32::from_rgb(30, 30, 33),
                panel_alt: egui::Color32::from_rgb(26, 26, 29),
                card: egui::Color32::from_rgb(36, 36, 40),
                card_hover: egui::Color32::from_rgb(44, 44, 49),
                stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 78)),
                subtle_stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(52, 52, 58)),
                text_dim: egui::Color32::from_rgb(170, 170, 180),
            }
        } else {
            Self {
                background: egui::Color32::from_rgb(245, 245, 247),
                panel: egui::Color32::from_rgb(250, 250, 252),
                panel_alt: egui::Color32::from_rgb(242, 242, 245),
                card: egui::Color32::from_rgb(255, 255, 255),
                card_hover: egui::Color32::from_rgb(252, 252, 252),
                stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(215, 215, 222)),
                subtle_stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(228, 228, 234)),
                text_dim: egui::Color32::from_rgb(110, 110, 120),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum CellStatus {
    #[default]
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CellGroup {
    id: u64,
    input: String,
    output: Option<EvalResult>,
    collapsed: bool,
    bookmarked: bool,
    tags: Vec<String>,

    #[serde(skip, default)]
    status: CellStatus,
    #[serde(skip, default)]
    last_duration: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotebookFile {
    version: u32,
    groups: Vec<CellGroup>,
}

impl Default for NotebookFile {
    fn default() -> Self {
        Self {
            version: 1,
            groups: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct Tab {
    id: u64,
    title: String,
    notebook_path: PathBuf,
    kernel: KernelSession,

    groups: Vec<CellGroup>,
    selected: usize,
    selection: BTreeSet<u64>,
    selection_anchor: Option<usize>,
    dirty: bool,

    // UX state
    filter: String,
    filter_errors_only: bool,
    filter_messages_only: bool,
    show_palette: bool,

    // Documentation browser state
    doc_query: String,
    doc_results: Vec<String>,
    doc_selected: Option<String>,
    doc_content: String,
    doc_error: Option<String>,
    doc_jobs: VecDeque<DocJob>,
    doc_eval_counter: u64,

    // Execution flow
    eval_queue: VecDeque<EvalJob>,
    last_rerun: Option<usize>,

    // Autosave
    last_autosave_at: Instant,

    // Internal clipboard (cross-platform; we still write to system clipboard for convenience).
    internal_clipboard: Option<String>,

    // Focus management (e.g. after auto-inserting a new cell).
    focus_input_group_id: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct EvalJob {
    idx: usize,
    spawn_next: bool,
}

#[derive(Debug, Clone)]
enum DocJob {
    Search(String),
    Fetch(String),
}

impl Tab {
    fn ensure_one_group(&mut self, next_id: &mut u64) {
        if self.groups.is_empty() {
            self.groups.push(CellGroup {
                id: *next_id,
                input: "1+1".to_string(),
                output: None,
                collapsed: false,
                bookmarked: false,
                tags: Vec::new(),
                status: CellStatus::Idle,
                last_duration: None,
            });
            *next_id += 1;
            self.selected = 0;
        }
    }

    fn load_notebook(&mut self, next_id: &mut u64) -> anyhow::Result<()> {
        if !self.notebook_path.is_file() {
            return Ok(());
        }
        let bytes = fs::read(&self.notebook_path)?;
        let mut file: NotebookFile = serde_json::from_slice(&bytes)?;
        if file.version == 0 {
            file.version = 1;
        }

        for g in &mut file.groups {
            g.status = CellStatus::Idle;
            g.last_duration = None;
        }

        *next_id = (*next_id).max(file.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1);
        self.groups = file.groups;
        self.selected = self.selected.min(self.groups.len().saturating_sub(1));
        Ok(())
    }

    fn save_notebook(&mut self) -> anyhow::Result<()> {
        if let Some(parent) = self.notebook_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = NotebookFile {
            version: 1,
            groups: self.groups.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        fs::write(&self.notebook_path, bytes)?;
        self.dirty = false;
        Ok(())
    }

    fn autosave_snapshot(&mut self) -> anyhow::Result<()> {
        let snapshot_path = autosave_path(&self.notebook_path);
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = NotebookFile {
            version: 1,
            groups: self.groups.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        fs::write(&snapshot_path, bytes)?;
        Ok(())
    }

    fn visible_indices(&self) -> Vec<usize> {
        let needle = self.filter.trim().to_lowercase();
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                if self.filter_errors_only && g.status != CellStatus::Error {
                    return false;
                }
                if self.filter_messages_only {
                    let has_msgs = g.output.as_ref().is_some_and(|o| !o.messages.is_empty());
                    if !has_msgs {
                        return false;
                    }
                }

                if needle.is_empty() {
                    return true;
                }

                let mut hay = g.input.to_lowercase();
                if let Some(out) = &g.output {
                    hay.push('\n');
                    hay.push_str(&out.output_text.to_lowercase());
                    for m in &out.messages {
                        hay.push('\n');
                        hay.push_str(&m.to_lowercase());
                    }
                }
                hay.contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn set_selected(&mut self, idx: usize) {
        self.selected = idx.min(self.groups.len().saturating_sub(1));
    }

    fn toggle_selection_for(&mut self, idx: usize) {
        if let Some(id) = self.groups.get(idx).map(|g| g.id)
            && !self.selection.insert(id)
        {
            self.selection.remove(&id);
        }
    }

    fn select_range(&mut self, from: usize, to: usize) {
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        self.selection.clear();
        for idx in a..=b {
            if let Some(id) = self.groups.get(idx).map(|g| g.id) {
                self.selection.insert(id);
            }
        }
    }

    #[allow(dead_code)]
    fn select_all(&mut self) {
        self.selection = self.groups.iter().map(|g| g.id).collect();
    }

    #[allow(dead_code)]
    fn select_none(&mut self) {
        self.selection.clear();
    }

    #[allow(dead_code)]
    fn invert_selection(&mut self) {
        let mut next = BTreeSet::new();
        for g in &self.groups {
            if !self.selection.contains(&g.id) {
                next.insert(g.id);
            }
        }
        self.selection = next;
    }

    fn selected_indices_in_order(&self) -> Vec<usize> {
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, g)| self.selection.contains(&g.id))
            .map(|(i, _)| i)
            .collect()
    }

    fn delete_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        self.groups.retain(|g| !self.selection.contains(&g.id));
        self.selection.clear();
        self.selected = self.selected.min(self.groups.len().saturating_sub(1));
        self.dirty = true;
    }

    fn duplicate_selected(&mut self, next_id: &mut u64) {
        let selected = self.selected_indices_in_order();
        if selected.is_empty() {
            return;
        }

        // Preserve relative order by inserting duplicates after the last selected index.
        let insert_at = selected.last().copied().unwrap_or(self.selected) + 1;
        let mut clones = Vec::new();
        for idx in selected {
            let mut g = self.groups[idx].clone();
            g.id = *next_id;
            *next_id += 1;
            g.output = None;
            g.status = CellStatus::Idle;
            g.last_duration = None;
            clones.push(g);
        }

        self.groups.splice(insert_at..insert_at, clones);
        self.dirty = true;
    }

    #[allow(dead_code)]
    fn clear_outputs_selected(&mut self) {
        for g in &mut self.groups {
            if self.selection.contains(&g.id) {
                g.output = None;
                g.status = CellStatus::Idle;
                g.last_duration = None;
            }
        }
        self.dirty = true;
    }

    #[allow(dead_code)]
    fn move_selected_block(&mut self, delta: isize) {
        let selected = self.selected_indices_in_order();
        if selected.is_empty() {
            return;
        }

        let len = self.groups.len();
        let first = *selected.first().unwrap();
        let last = *selected.last().unwrap();

        let new_first = (first as isize + delta).clamp(0, (len - (last - first) - 1) as isize);
        let new_first = new_first as usize;
        if new_first == first {
            return;
        }

        let mut chunk = Vec::new();
        // Remove from end to start to keep indices stable.
        for idx in selected.iter().rev() {
            chunk.push(self.groups.remove(*idx));
        }
        chunk.reverse();

        let removed_before_first = selected.len();
        let adjusted_new_first = if new_first > first {
            // Moving down: list is shorter after removal.
            new_first.saturating_sub(removed_before_first)
        } else {
            new_first
        };

        self.groups
            .splice(adjusted_new_first..adjusted_new_first, chunk);
        self.dirty = true;
    }

    fn collapse_all_outputs(&mut self) {
        for g in &mut self.groups {
            g.collapsed = true;
        }
        self.dirty = true;
    }

    fn expand_all_outputs(&mut self) {
        for g in &mut self.groups {
            g.collapsed = false;
        }
        self.dirty = true;
    }

    fn selection_as_json(&self) -> Option<String> {
        let selected: Vec<&CellGroup> = self
            .groups
            .iter()
            .filter(|g| self.selection.contains(&g.id))
            .collect();

        let file = NotebookFile {
            version: 1,
            groups: selected.into_iter().cloned().collect(),
        };
        serde_json::to_string_pretty(&file).ok()
    }

    fn selection_as_text(&self) -> String {
        let mut buf = String::new();
        for g in &self.groups {
            if !self.selection.contains(&g.id) {
                continue;
            }
            buf.push_str("In:\n");
            buf.push_str(&g.input);
            buf.push_str("\n\n");
            if let Some(out) = &g.output {
                buf.push_str("Out:\n");
                buf.push_str(&out.output_text);
                buf.push('\n');
                if !out.messages.is_empty() {
                    buf.push_str("Messages:\n");
                    for m in &out.messages {
                        buf.push_str(m);
                        buf.push('\n');
                    }
                }
                buf.push('\n');
            }
        }
        buf
    }

    fn set_internal_clipboard(&mut self, value: String) {
        self.internal_clipboard = Some(value);
    }

    fn paste_groups_from_internal_clipboard(&mut self, next_id: &mut u64) {
        let Some(s) = self.internal_clipboard.clone() else {
            return;
        };

        if let Ok(file) = serde_json::from_str::<NotebookFile>(&s) {
            let mut groups = file.groups;
            for g in &mut groups {
                g.id = *next_id;
                *next_id += 1;
                g.status = CellStatus::Idle;
                g.last_duration = None;
            }
            let insert_at = self.selected.saturating_add(1);
            self.groups.splice(insert_at..insert_at, groups);
            self.dirty = true;
            return;
        }

        // Fallback: paste as a new input-only group.
        let insert_at = self.selected.saturating_add(1);
        self.groups.splice(
            insert_at..insert_at,
            [CellGroup {
                id: *next_id,
                input: s.to_string(),
                output: None,
                collapsed: false,
                bookmarked: false,
                tags: Vec::new(),
                status: CellStatus::Idle,
                last_duration: None,
            }],
        );
        *next_id += 1;
        self.dirty = true;
    }

    fn enqueue_eval_selected(&mut self) {
        let mut indices = self.selected_indices_in_order();
        if indices.is_empty() {
            indices.push(self.selected);
        }
        for idx in indices {
            self.eval_queue.push_back(EvalJob {
                idx,
                spawn_next: false,
            });
        }
    }

    fn enqueue_eval_all_visible(&mut self) {
        for idx in self.visible_indices() {
            self.eval_queue.push_back(EvalJob {
                idx,
                spawn_next: false,
            });
        }
    }

    fn enqueue_eval_all_groups(&mut self) {
        for idx in 0..self.groups.len() {
            self.eval_queue.push_back(EvalJob {
                idx,
                spawn_next: false,
            });
        }
    }

    fn is_wstp_desync_error(err: &anyhow::Error) -> bool {
        let s = err.to_string();
        s.contains("WSNextPacket called while the current packet has unread data")
            || s.contains("WSTP error (code 22)")
            || s.contains("has no context")
    }

    fn eval_with_recovery(&mut self, eval_id: u64, wl_input: &str) -> anyhow::Result<EvalResult> {
        match self.kernel.evaluate(eval_id, wl_input) {
            Ok(out) => Ok(out),
            Err(err) => {
                if Self::is_wstp_desync_error(&err) {
                    tracing::warn!(
                        tab_id = self.id,
                        eval_id,
                        error = %err,
                        "kernel link desynced; restarting and retrying once"
                    );
                    self.kernel.restart_with_current_config()?;
                    self.kernel.evaluate(eval_id, wl_input)
                } else {
                    Err(err)
                }
            }
        }
    }

    fn tick_eval_queue(&mut self, max_per_frame: usize, next_id: &mut u64) {
        for _ in 0..max_per_frame {
            let Some(job) = self.eval_queue.pop_front() else {
                return;
            };
            let idx = job.idx;
            self.set_selected(idx);
            if let Some(group) = self.groups.get_mut(idx) {
                group.status = CellStatus::Running;
                group.last_duration = None;
            }
            let input = self
                .groups
                .get(idx)
                .map(|g| g.input.clone())
                .unwrap_or_default();
            let started = Instant::now();
            let eval_id = self.id * 1_000_000 + idx as u64;
            let result = self.eval_with_recovery(eval_id, &input);
            let duration = started.elapsed();

            match result {
                Ok(out) => {
                    if let Some(group) = self.groups.get_mut(idx) {
                        group.output = Some(out);
                        group.status = CellStatus::Idle;
                        group.last_duration = Some(duration);
                    }
                    self.last_rerun = Some(idx);
                    self.dirty = true;
                }
                Err(err) => {
                    error!(tab_id = self.id, group_id = %self.groups[idx].id, error = %err, "eval failed");
                    if let Some(group) = self.groups.get_mut(idx) {
                        group.status = CellStatus::Error;
                        group.last_duration = Some(duration);
                    }
                    self.last_rerun = Some(idx);
                    self.dirty = true;
                }
            }

            // Interaction-first behavior: after a manual single-cell evaluation, ensure an empty
            // input cell exists immediately below and move focus there.
            if job.spawn_next && self.eval_queue.is_empty() {
                self.ensure_next_input_cell(idx, next_id);
            }
        }
    }

    fn ensure_next_input_cell(&mut self, idx: usize, next_id: &mut u64) {
        let insert_at = idx.saturating_add(1).min(self.groups.len());

        let reuse_next = self
            .groups
            .get(insert_at)
            .is_some_and(|next| next.input.trim().is_empty() && next.output.is_none());
        if reuse_next {
            let id = self.groups[insert_at].id;
            self.set_selected(insert_at);
            self.selection.clear();
            self.selection.insert(id);
            self.selection_anchor = Some(insert_at);
            self.focus_input_group_id = Some(id);
            return;
        }

        let id = *next_id;
        *next_id += 1;
        self.groups.insert(
            insert_at,
            CellGroup {
                id,
                input: String::new(),
                output: None,
                collapsed: false,
                bookmarked: false,
                tags: Vec::new(),
                status: CellStatus::Idle,
                last_duration: None,
            },
        );
        self.set_selected(insert_at);
        self.selection.clear();
        self.selection.insert(id);
        self.selection_anchor = Some(insert_at);
        self.focus_input_group_id = Some(id);
        self.dirty = true;
    }

    fn enqueue_doc_search(&mut self) {
        let q = self.doc_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.doc_jobs.push_back(DocJob::Search(q));
    }

    fn enqueue_doc_fetch(&mut self, symbol: String) {
        self.doc_jobs.push_back(DocJob::Fetch(symbol));
    }

    fn tick_doc_jobs(&mut self, max_per_frame: usize) {
        for _ in 0..max_per_frame {
            let Some(job) = self.doc_jobs.pop_front() else {
                return;
            };

            match job {
                DocJob::Search(q) => {
                    self.doc_error = None;
                    let wl = format!(
                        "ExportString[Take[Sort[Names[\"*\"<>\"{q}\"<>\"*\"]], UpTo[50]], \"JSON\"]",
                        q = wl_escape_string(&q)
                    );
                    let eval_id = self.id * 1_000_000 + 900_000 + self.doc_eval_counter;
                    self.doc_eval_counter = self.doc_eval_counter.wrapping_add(1);

                    match self.eval_with_recovery(eval_id, &wl) {
                        Ok(out) => {
                            let json = unquote_json_string(&out.output_text);
                            match serde_json::from_str::<Vec<String>>(&json) {
                                Ok(mut results) => {
                                    results.truncate(50);
                                    self.doc_results = results;
                                    self.doc_selected = None;
                                    self.doc_content.clear();
                                }
                                Err(err) => {
                                    self.doc_error = Some(format!(
                                        "failed to parse search results: {err} (raw: {json})"
                                    ));
                                }
                            }
                        }
                        Err(err) => {
                            self.doc_error = Some(format!("doc search failed: {err:#}"));
                        }
                    }
                }
                DocJob::Fetch(sym) => {
                    self.doc_error = None;
                    let wl = format!(
                        "ExportString[With[{{s=\"{s}\"}},<|\"Name\"->s,\"Context\"->Quiet@Check[Context[Symbol[s]],\"\"],\"Usage\"->Quiet@Check[Information[Symbol[s],\"Usage\"],\"\"]|>],\"JSON\"]",
                        s = wl_escape_string(&sym)
                    );
                    let eval_id = self.id * 1_000_000 + 910_000 + self.doc_eval_counter;
                    self.doc_eval_counter = self.doc_eval_counter.wrapping_add(1);

                    match self.eval_with_recovery(eval_id, &wl) {
                        Ok(out) => {
                            let json = unquote_json_string(&out.output_text);
                            match serde_json::from_str::<serde_json::Value>(&json) {
                                Ok(v) => {
                                    self.doc_selected = Some(sym);
                                    self.doc_content =
                                        serde_json::to_string_pretty(&v).unwrap_or_else(|_| json);
                                }
                                Err(err) => {
                                    self.doc_error = Some(format!(
                                        "failed to parse documentation page: {err} (raw: {json})"
                                    ));
                                }
                            }
                        }
                        Err(err) => {
                            self.doc_error = Some(format!("doc fetch failed: {err:#}"));
                        }
                    }
                }
            }
        }
    }
}

pub struct WovenApp {
    config: AppConfig,
    next_tab_id: u64,
    next_group_id: u64,
    tabs: Vec<Tab>,
    active_tab: usize,
    confirm_delete: bool,
    confirm_close_tab: Option<usize>,
    show_navigator: bool,
    show_inspector: bool,
    inspector_tab: InspectorTab,
}

impl WovenApp {
    #[instrument(skip_all)]
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig, kernel: KernelSession) -> Self {
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.text_styles.iter_mut().for_each(|(_, font_id)| {
            font_id.size *= config.ui.font_scale;
        });
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        cc.egui_ctx.set_global_style(style);
        apply_theme(&cc.egui_ctx, config.ui.theme);

        let notebook_path = PathBuf::from(&config.ui.notebook_path);
        let mut tab = Tab {
            id: 1,
            title: "Notebook 1".to_string(),
            notebook_path,
            kernel,
            groups: Vec::new(),
            selected: 0,
            selection: BTreeSet::new(),
            selection_anchor: None,
            dirty: false,
            filter: String::new(),
            filter_errors_only: false,
            filter_messages_only: false,
            show_palette: false,
            doc_query: String::new(),
            doc_results: Vec::new(),
            doc_selected: None,
            doc_content: String::new(),
            doc_error: None,
            doc_jobs: VecDeque::new(),
            doc_eval_counter: 0,
            eval_queue: VecDeque::new(),
            last_rerun: None,
            last_autosave_at: Instant::now(),
            internal_clipboard: None,
            focus_input_group_id: None,
        };

        let mut next_group_id = 1;
        if let Err(err) = tab.load_notebook(&mut next_group_id) {
            debug!(error = %err, "failed to load notebook; starting new");
        }
        tab.ensure_one_group(&mut next_group_id);

        Self {
            config,
            next_tab_id: 2,
            next_group_id,
            tabs: vec![tab],
            active_tab: 0,
            confirm_delete: false,
            confirm_close_tab: None,
            show_navigator: true,
            show_inspector: true,
            inspector_tab: InspectorTab::Variables,
        }
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    fn new_tab(&mut self) -> anyhow::Result<()> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let ts = OffsetDateTime::now_utc()
            .format(&Rfc3339)?
            .replace([':', '.'], "");
        let notebook_path = PathBuf::from(format!("notebooks/untitled-{ts}-{id}.json"));

        let kernel = KernelSession::new(&self.config.kernel)?;

        let mut tab = Tab {
            id,
            title: format!("Notebook {id}"),
            notebook_path,
            kernel,
            groups: Vec::new(),
            selected: 0,
            selection: BTreeSet::new(),
            selection_anchor: None,
            dirty: false,
            filter: String::new(),
            filter_errors_only: false,
            filter_messages_only: false,
            show_palette: false,
            doc_query: String::new(),
            doc_results: Vec::new(),
            doc_selected: None,
            doc_content: String::new(),
            doc_error: None,
            doc_jobs: VecDeque::new(),
            doc_eval_counter: 0,
            eval_queue: VecDeque::new(),
            last_rerun: None,
            last_autosave_at: Instant::now(),
            internal_clipboard: None,
            focus_input_group_id: None,
        };
        tab.ensure_one_group(&mut self.next_group_id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        Ok(())
    }

    fn open_notebook_in_new_tab(&mut self, notebook_path: PathBuf) -> anyhow::Result<()> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;

        let kernel = KernelSession::new(&self.config.kernel)?;
        let title = notebook_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Notebook {id}"));

        let mut tab = Tab {
            id,
            title,
            notebook_path,
            kernel,
            groups: Vec::new(),
            selected: 0,
            selection: BTreeSet::new(),
            selection_anchor: None,
            dirty: false,
            filter: String::new(),
            filter_errors_only: false,
            filter_messages_only: false,
            show_palette: false,
            doc_query: String::new(),
            doc_results: Vec::new(),
            doc_selected: None,
            doc_content: String::new(),
            doc_error: None,
            doc_jobs: VecDeque::new(),
            doc_eval_counter: 0,
            eval_queue: VecDeque::new(),
            last_rerun: None,
            last_autosave_at: Instant::now(),
            internal_clipboard: None,
            focus_input_group_id: None,
        };

        if let Err(err) = tab.load_notebook(&mut self.next_group_id) {
            debug!(error = %err, path = %tab.notebook_path.display(), "failed to load notebook");
        }
        tab.ensure_one_group(&mut self.next_group_id);

        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        Ok(())
    }

    fn request_close_active_tab(&mut self) {
        self.confirm_close_tab = Some(self.active_tab);
    }

    fn close_tab_index(&mut self, idx: usize) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(idx);
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
    }

    fn cycle_tabs(&mut self, delta: isize) {
        let len = self.tabs.len();
        if len == 0 {
            return;
        }
        let cur = self.active_tab as isize;
        let next = (cur + delta).rem_euclid(len as isize) as usize;
        self.active_tab = next;
    }
}

impl eframe::App for WovenApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Global shortcuts:
        if ui.input(|i| i.key_pressed(egui::Key::N) && i.modifiers.ctrl)
            && let Err(err) = self.new_tab()
        {
            error!(error = %err, "failed to create new tab");
        }
        if ui.input(|i| i.key_pressed(egui::Key::W) && i.modifiers.ctrl) {
            self.request_close_active_tab();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Tab) && i.modifiers.ctrl) {
            let backwards = ui.input(|i| i.modifiers.shift);
            self.cycle_tabs(if backwards { -1 } else { 1 });
        }

        // Tab-local shortcuts
        if ui.input(|i| i.key_pressed(egui::Key::P) && i.modifiers.ctrl) {
            self.active_tab_mut().show_palette = true;
        }
        if ui.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.ctrl) {
            if let Err(err) = self.active_tab_mut().save_notebook() {
                error!(error = %err, "save failed");
            }
        }

        // Autosave tick
        if self.config.ui.autosave_enabled {
            let autosave_every =
                Duration::from_millis(self.config.ui.autosave_interval_ms.max(250));
            let tab = self.active_tab_mut();
            if tab.dirty && tab.last_autosave_at.elapsed() >= autosave_every {
                if let Err(err) = tab.autosave_snapshot() {
                    debug!(error = %err, "autosave snapshot failed");
                }
                tab.last_autosave_at = Instant::now();
            }
        }

        // Drive a tiny "queue" by doing at most one eval per frame.
        let active = self.active_tab;
        let (tabs, next_id) = (&mut self.tabs, &mut self.next_group_id);
        tabs[active].tick_eval_queue(1, next_id);
        tabs[active].tick_doc_jobs(1);

        let theme = self.config.ui.theme;
        let dark = theme == Theme::Dark;
        let palette = UiPalette::for_theme(dark);

        let mut last_error: Option<String> = None;
        let mut open_notebook: Option<PathBuf> = None;
        let mut menu_actions: Vec<&'static str> = Vec::new();

        // Top bar
        egui::Panel::top("top_bar")
            .exact_size(44.0)
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .stroke(palette.subtle_stroke),
            )
            .show_inside(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Menu")
                        .on_hover_text("Toggle navigator")
                        .clicked()
                    {
                        self.show_navigator = !self.show_navigator;
                    }
                    if ui
                        .button("Inspector")
                        .on_hover_text("Toggle inspector")
                        .clicked()
                    {
                        self.show_inspector = !self.show_inspector;
                    }

                    ui.add_space(8.0);

                    egui::ComboBox::from_id_salt("tab_select")
                        .selected_text(self.tabs[self.active_tab].title.clone())
                        .show_ui(ui, |ui| {
                            for i in 0..self.tabs.len() {
                                if ui
                                    .selectable_label(
                                        i == self.active_tab,
                                        self.tabs[i].title.clone(),
                                    )
                                    .clicked()
                                {
                                    self.active_tab = i;
                                }
                            }
                        });

                    if self.tabs[self.active_tab].dirty {
                        ui.label("*");
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let theme_label = if dark { "Day" } else { "Night" };
                        if ui
                            .button(theme_label)
                            .on_hover_text("Toggle theme")
                            .clicked()
                        {
                            self.config.ui.theme = if dark { Theme::Light } else { Theme::Dark };
                            apply_theme(ui.ctx(), self.config.ui.theme);
                            if let Err(err) =
                                crate::config::persist_local_theme(self.config.ui.theme)
                            {
                                debug!(error = %err, "failed to persist theme override");
                            }
                        }

                        if ui.button("Run All").clicked() {
                            menu_actions.push("run_all");
                        }
                        // Kernel status indicator (painted circle to avoid missing-glyph squares).
                        let dot = if dark {
                            egui::Color32::from_rgb(90, 210, 120)
                        } else {
                            egui::Color32::from_rgb(40, 170, 80)
                        };
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 6.0, dot);

                        egui::MenuBar::new().ui(ui, |ui| {
                            ui.menu_button("File", |ui| {
                                if ui.button("New Tab (Ctrl+N)").clicked() {
                                    menu_actions.push("new_tab");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Close Tab (Ctrl+W)").clicked() {
                                    menu_actions.push("close_tab");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                ui.separator();
                                if ui.button("Save (Ctrl+S)").clicked() {
                                    menu_actions.push("save");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Save All").clicked() {
                                    menu_actions.push("save_all");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            });

                            ui.menu_button("Edit", |ui| {
                                if ui.button("New Cell").clicked() {
                                    menu_actions.push("new_cell");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Delete Selected…").clicked() {
                                    menu_actions.push("delete_selected");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                ui.separator();
                                if ui.button("Copy selection as text").clicked() {
                                    menu_actions.push("copy_text");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Copy selection as JSON").clicked() {
                                    menu_actions.push("copy_json");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Paste").clicked() {
                                    menu_actions.push("paste");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Duplicate Selected").clicked() {
                                    menu_actions.push("duplicate");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            });

                            ui.menu_button("Kernel", |ui| {
                                if ui.button("Evaluate selection (Ctrl+Enter)").clicked() {
                                    menu_actions.push("eval_selection");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Evaluate visible (Ctrl+Shift+Enter)").clicked() {
                                    menu_actions.push("eval_visible");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Abort").clicked() {
                                    menu_actions.push("abort");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                                if ui.button("Restart kernel").clicked() {
                                    menu_actions.push("restart_kernel");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            });

                            ui.menu_button("View", |ui| {
                                if ui.button("Command palette (Ctrl+P)").clicked() {
                                    menu_actions.push("palette");
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            });
                        });
                    });
                });
            });

        let active = self.active_tab;
        let kernel_cfg = self.config.kernel.clone();
        let mut request_delete_confirm = false;

        // Execute menu actions
        for a in menu_actions {
            match a {
                "new_tab" => {
                    if let Err(err) = self.new_tab() {
                        last_error = Some(format!("new tab failed: {err:#}"));
                    }
                }
                "close_tab" => self.request_close_active_tab(),
                "save" => {
                    if let Err(err) = self.tabs[active].save_notebook() {
                        last_error = Some(format!("save failed: {err:#}"));
                    }
                }
                "save_all" => {
                    for t in &mut self.tabs {
                        if t.dirty {
                            if let Err(err) = t.save_notebook() {
                                last_error = Some(format!("save-all failed: {err:#}"));
                                break;
                            }
                        }
                    }
                }
                "new_cell" => {
                    let id = self.next_group_id;
                    self.next_group_id += 1;
                    let tab = &mut self.tabs[active];
                    tab.groups.push(CellGroup {
                        id,
                        input: String::new(),
                        output: None,
                        collapsed: false,
                        bookmarked: false,
                        tags: Vec::new(),
                        status: CellStatus::Idle,
                        last_duration: None,
                    });
                    tab.set_selected(tab.groups.len().saturating_sub(1));
                    tab.selection.clear();
                    tab.selection.insert(id);
                    tab.selection_anchor = Some(tab.selected);
                    tab.dirty = true;
                }
                "delete_selected" => self.confirm_delete = true,
                "duplicate" => self.tabs[active].duplicate_selected(&mut self.next_group_id),
                "copy_json" => {
                    if let Some(s) = self.tabs[active].selection_as_json() {
                        self.tabs[active].set_internal_clipboard(s.clone());
                        ui.ctx().copy_text(s);
                    }
                }
                "copy_text" => {
                    let s = self.tabs[active].selection_as_text();
                    self.tabs[active].set_internal_clipboard(s.clone());
                    ui.ctx().copy_text(s);
                }
                "paste" => {
                    self.tabs[active].paste_groups_from_internal_clipboard(&mut self.next_group_id)
                }
                "eval_selection" => self.tabs[active].enqueue_eval_selected(),
                "eval_visible" => self.tabs[active].enqueue_eval_all_visible(),
                "abort" => {
                    if let Err(err) = self.tabs[active].kernel.abort() {
                        last_error = Some(format!("abort failed: {err:#}"));
                    }
                }
                "restart_kernel" => {
                    if let Err(err) = self.tabs[active].kernel.restart(&kernel_cfg) {
                        last_error = Some(format!("kernel restart failed: {err:#}"));
                    }
                }
                "palette" => self.tabs[active].show_palette = true,
                "run_all" => self.tabs[active].enqueue_eval_all_groups(),
                _ => {}
            }
        }

        // Left navigator
        if self.show_navigator {
            egui::Panel::left("navigator")
                .resizable(true)
                .default_size(self.config.ui.nav_width)
                .min_size(240.0)
                .frame(
                    egui::Frame::NONE
                        .fill(palette.panel_alt)
                        .stroke(palette.subtle_stroke),
                )
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);

                    egui::CollapsingHeader::new("File Navigator")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::CollapsingHeader::new("Notebooks")
                                .default_open(true)
                                .show(ui, |ui| {
                                    for i in 0..self.tabs.len() {
                                        let mut label = self.tabs[i].title.clone();
                                        if self.tabs[i].dirty {
                                            label.push('*');
                                        }
                                        if ui
                                            .selectable_label(i == self.active_tab, label)
                                            .clicked()
                                        {
                                            self.active_tab = i;
                                        }
                                    }
                                });

                            egui::CollapsingHeader::new("Files")
                                .default_open(true)
                                .show(ui, |ui| {
                                    let notebooks_dir = PathBuf::from("notebooks");
                                    if notebooks_dir.is_dir() {
                                        let mut entries: Vec<_> = match fs::read_dir(&notebooks_dir)
                                        {
                                            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
                                            Err(_) => Vec::new(),
                                        };
                                        entries.sort_by_key(|e| e.path());
                                        for e in entries {
                                            let p = e.path();
                                            if p.extension().and_then(|s| s.to_str())
                                                != Some("json")
                                            {
                                                continue;
                                            }
                                            let label = p
                                                .file_stem()
                                                .and_then(|s| s.to_str())
                                                .unwrap_or("notebook");
                                            if ui.button(label).clicked() {
                                                open_notebook = Some(p);
                                            }
                                        }
                                    } else {
                                        ui.label("No `notebooks/` directory yet.");
                                    }
                                });
                        });

                    ui.separator();

                    egui::CollapsingHeader::new("Notebook Organization")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label("Reserved for future notebook indexing/folders.");
                        });

                    egui::CollapsingHeader::new("Tags")
                        .default_open(false)
                        .show(ui, |ui| {
                            let tab = &mut self.tabs[active];
                            let mut uniq: BTreeSet<String> = BTreeSet::new();
                            for g in &tab.groups {
                                for t in &g.tags {
                                    uniq.insert(t.clone());
                                }
                            }
                            if uniq.is_empty() {
                                ui.label("No tags yet.");
                            } else {
                                for t in uniq {
                                    if ui.button(&t).clicked() {
                                        tab.filter = t;
                                    }
                                }
                            }
                        });

                    egui::CollapsingHeader::new("Kernels")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let dot = if dark {
                                    egui::Color32::from_rgb(90, 210, 120)
                                } else {
                                    egui::Color32::from_rgb(40, 170, 80)
                                };
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(rect.center(), 6.0, dot);
                                ui.label("Kernel status");
                            });
                            if ui.button("Restart kernel").clicked() {
                                if let Err(err) = self.tabs[active].kernel.restart(&kernel_cfg) {
                                    last_error = Some(format!("kernel restart failed: {err:#}"));
                                }
                            }
                        });
                });
        }

        // Right inspector
        if self.show_inspector {
            egui::Panel::right("inspector")
                .resizable(true)
                .default_size(self.config.ui.inspector_width)
                .min_size(260.0)
                .size_range(self.config.ui.inspector_width..=self.config.ui.inspector_max_width)
                .frame(
                    egui::Frame::NONE
                        .fill(palette.panel_alt)
                        .stroke(palette.subtle_stroke),
                )
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(
                                self.inspector_tab == InspectorTab::Variables,
                                "Variables",
                            )
                            .clicked()
                        {
                            self.inspector_tab = InspectorTab::Variables;
                        }
                        if ui
                            .selectable_label(
                                self.inspector_tab == InspectorTab::Documentation,
                                "Documentation",
                            )
                            .clicked()
                        {
                            self.inspector_tab = InspectorTab::Documentation;
                        }
                    });

                    ui.separator();

                    match self.inspector_tab {
                        InspectorTab::Variables => {
                            egui::CollapsingHeader::new("Symbol")
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.label("Reserved: active selection inspection.");
                                });
                            egui::CollapsingHeader::new("Details")
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.label("Reserved: kernel variable metadata.");
                                });
                            egui::CollapsingHeader::new("Properties")
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.label("Reserved: plot/expr properties.");
                                });
                        }
                        InspectorTab::Documentation => {
                            let tab = &mut self.tabs[active];
                            ui.label("Documentation");
                            ui.add_space(6.0);

                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 72.0).max(160.0);
                                let resp = ui.add_sized(
                                    [w, 0.0],
                                    egui::TextEdit::singleline(&mut tab.doc_query)
                                        .hint_text("Search symbols (e.g. Plot, Integrate)…")
                                        .desired_width(w),
                                );
                                if resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    tab.enqueue_doc_search();
                                }
                                if ui.button("Search").clicked() {
                                    tab.enqueue_doc_search();
                                }
                            });

                            if let Some(err) = tab.doc_error.as_deref() {
                                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
                            }

                            ui.separator();

                            ui.label("Results");
                            egui::ScrollArea::vertical()
                                .max_height(180.0)
                                .show(ui, |ui| {
                                    for sym in tab.doc_results.clone() {
                                        let selected =
                                            tab.doc_selected.as_deref() == Some(sym.as_str());
                                        if ui.selectable_label(selected, &sym).clicked() {
                                            tab.enqueue_doc_fetch(sym);
                                        }
                                    }
                                });

                            ui.separator();
                            ui.label("Page");
                            if tab.doc_content.trim().is_empty() {
                                ui.label("Search to view symbol usage and context.");
                            } else {
                                ui.add(
                                    egui::TextEdit::multiline(&mut tab.doc_content)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_rows(18)
                                        .interactive(false),
                                );
                            }
                        }
                    }
                });
        }

        // Central notebook
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.background))
            .show_inside(ui, |ui| {
                if let Some(err) = last_error.as_deref() {
                    ui.add_space(6.0);
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
                    ui.separator();
                }

                let (tabs, next_group_id) = (&mut self.tabs, &mut self.next_group_id);
                tabs[active].ensure_one_group(next_group_id);
                let tab = &mut tabs[active];

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut tab.filter)
                            .hint_text("Search…")
                            .desired_width(240.0),
                    );
                    ui.add_space(6.0);
                    ui.checkbox(&mut tab.filter_errors_only, "Errors");
                    ui.checkbox(&mut tab.filter_messages_only, "Messages");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("Selected: {}", tab.selection.len()));
                    });
                });

                ui.add_space(10.0);

                let visible = tab.visible_indices();
                let full_w = ui.available_width();
                let content_w = full_w.min(980.0);
                let side = ((full_w - content_w) / 2.0).max(0.0);

                ui.horizontal(|ui| {
                    ui.add_space(side);

                    ui.allocate_ui_with_layout(
                        egui::vec2(content_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(content_w);
                            ui.set_max_width(content_w);

                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_min_width(content_w);
                                    ui.set_max_width(content_w);

                                    for idx in visible {
                                        if idx >= tab.groups.len() {
                                            continue;
                                        }

                                        ui.add_space(10.0);

                                        let group_id = tab.groups[idx].id;
                                        let accent = group_accent_color(&tab.groups[idx]);
                                        let is_selected = idx == tab.selected;
                                        let is_checked = tab.selection.contains(&group_id);
                                        let input_blue = egui::Color32::from_rgb(80, 160, 255);
                                        let output_green = egui::Color32::from_rgb(90, 210, 120);

                                        let mut request_eval = false;
                                        let mut request_delete = false;

                                        let frame = egui::Frame::NONE
                                            .fill(palette.card)
                                            .stroke(if is_selected {
                                                egui::Stroke::new(1.5, accent)
                                            } else {
                                                palette.subtle_stroke
                                            })
                                            .corner_radius(egui::CornerRadius::same(10))
                                            .inner_margin(egui::Margin::symmetric(14, 12));

                                        let inner = frame.show(ui, |ui| {
                                            ui.set_min_width(content_w);
                                            ui.set_max_width(content_w);

                                            ui.horizontal(|ui| {
                                                ui.vertical(|ui| {
                                                    let mut checked = is_checked;
                                                    let resp = ui.checkbox(&mut checked, "");
                                                    if resp.clicked() {
                                                        let modifiers = ui.input(|i| i.modifiers);
                                                        tab.set_selected(idx);
                                                        if modifiers.shift {
                                                            let anchor = tab
                                                                .selection_anchor
                                                                .unwrap_or(tab.selected);
                                                            tab.select_range(anchor, idx);
                                                        } else if modifiers.ctrl {
                                                            tab.toggle_selection_for(idx);
                                                        } else {
                                                            tab.selection.clear();
                                                            tab.toggle_selection_for(idx);
                                                        }
                                                        tab.selection_anchor = Some(idx);
                                                    }

                                                    let run =
                                                        ui.button("Run").on_hover_text("Evaluate");
                                                    if run.clicked() {
                                                        tab.set_selected(idx);
                                                        request_eval = true;
                                                    }

                                                    let status = tab.groups[idx].status;
                                                    match status {
                                                        CellStatus::Running => {
                                                            ui.add(egui::Spinner::new().size(16.0));
                                                        }
                                                        CellStatus::Error => {
                                                            ui.colored_label(
                                                                egui::Color32::from_rgb(
                                                                    220, 80, 80,
                                                                ),
                                                                "ERR",
                                                            );
                                                        }
                                                        CellStatus::Idle => {
                                                            if tab.groups[idx].output.is_some() {
                                                                ui.colored_label(
                                                                    egui::Color32::from_rgb(
                                                                        90, 210, 120,
                                                                    ),
                                                                    "OK",
                                                                );
                                                            } else {
                                                                ui.label("");
                                                            }
                                                        }
                                                    }
                                                });

                                                ui.add_space(10.0);

                                                ui.vertical(|ui| {
                                                    let group = &mut tab.groups[idx];
                                                    let desired_rows =
                                                        group.input.lines().count().clamp(1, 12);
                                                    let input_id =
                                                        egui::Id::new(("cell_input", group.id));
                                                    if tab.focus_input_group_id == Some(group.id) {
                                                        ui.ctx().memory_mut(|mem| {
                                                            mem.request_focus(input_id);
                                                        });
                                                        tab.focus_input_group_id = None;
                                                    }
                                                    let resp = ui.add(
                                                        egui::TextEdit::multiline(&mut group.input)
                                                            .font(egui::TextStyle::Monospace)
                                                            .text_color(input_blue)
                                                            .desired_rows(desired_rows)
                                                            .desired_width(f32::INFINITY)
                                                            .frame(egui::Frame::NONE)
                                                            .id(input_id)
                                                            .hint_text("Wolfram Language input…"),
                                                    );
                                                    if resp.changed() {
                                                        tab.dirty = true;
                                                    }

                                                    if let Some(out) = group.output.as_ref() {
                                                        if !group.collapsed {
                                                            ui.add_space(10.0);
                                                            ui.separator();
                                                            ui.add_space(10.0);

                                                            let output_text = truncate_str(
                                                                &out.output_text,
                                                                self.config.plot.max_output_chars,
                                                            );
                                                            if !output_text.trim().is_empty() {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        output_text,
                                                                    )
                                                                    .size(20.0)
                                                                    .color(output_green),
                                                                );
                                                            }

                                                            let messages: Vec<&String> = out
                                                                .messages
                                                                .iter()
                                                                .take(self.config.plot.max_messages)
                                                                .collect();
                                                            if !messages.is_empty() {
                                                                ui.add_space(8.0);
                                                                for m in messages {
                                                                    ui.colored_label(
                                                                        palette.text_dim,
                                                                        m,
                                                                    );
                                                                }
                                                            }

                                                            if self.config.plot.placeholder_enabled
                                                                && is_plot_like(out)
                                                            {
                                                                ui.add_space(10.0);
                                                                Plot::new(format!(
                                                                    "plot_placeholder_{}",
                                                                    group.id
                                                                ))
                                                                .show(ui, |plot_ui| {
                                                                    let points: PlotPoints = (0
                                                                        ..100)
                                                                        .map(|i| {
                                                                            let x = i as f64 / 10.0;
                                                                            [x, (x).sin()]
                                                                        })
                                                                        .collect();
                                                                    plot_ui.line(Line::new(
                                                                        "sin(x)", points,
                                                                    ));
                                                                });
                                                            }
                                                        }
                                                    }
                                                });

                                                ui.with_layout(
                                                    egui::Layout::right_to_left(egui::Align::TOP),
                                                    |ui| {
                                                        let group = &mut tab.groups[idx];
                                                        let icon = if group.collapsed {
                                                            "Expand"
                                                        } else {
                                                            "Collapse"
                                                        };
                                                        if ui
                                                            .button(icon)
                                                            .on_hover_text("Collapse/expand output")
                                                            .clicked()
                                                        {
                                                            group.collapsed = !group.collapsed;
                                                            tab.dirty = true;
                                                        }
                                                        let bookmark = if group.bookmarked {
                                                            "Bookmarked"
                                                        } else {
                                                            "Bookmark"
                                                        };
                                                        if ui.button(bookmark).clicked() {
                                                            group.bookmarked = !group.bookmarked;
                                                            tab.dirty = true;
                                                        }
                                                    },
                                                );
                                            });
                                        });

                                        let rect = inner.response.rect;
                                        let stripe = egui::Rect::from_min_max(
                                            rect.min,
                                            egui::pos2(rect.min.x + 6.0, rect.max.y),
                                        );
                                        ui.painter().rect_filled(
                                            stripe,
                                            egui::CornerRadius::same(10),
                                            accent,
                                        );

                                        inner.response.context_menu(|ui| {
                                            if ui.button("Evaluate").clicked() {
                                                request_eval = true;
                                                ui.close_kind(egui::UiKind::Menu);
                                            }
                                            if ui.button("Delete selected…").clicked() {
                                                request_delete = true;
                                                ui.close_kind(egui::UiKind::Menu);
                                            }
                                        });

                                        if request_eval {
                                            tab.eval_queue.push_back(EvalJob {
                                                idx,
                                                spawn_next: true,
                                            });
                                        }
                                        if request_delete {
                                            request_delete_confirm = true;
                                        }
                                    }
                                });
                        },
                    );

                    ui.add_space(side);
                });
            });

        if request_delete_confirm {
            self.confirm_delete = true;
        }

        if let Some(p) = open_notebook.take() {
            if let Err(err) = self.open_notebook_in_new_tab(p) {
                error!(error = %err, "open notebook failed");
            }
        }

        // Confirmations
        if self.confirm_delete {
            egui::Window::new("Confirm delete")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Delete selected groups?");
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete = false;
                        }
                        if ui.button("Delete").clicked() {
                            self.active_tab_mut().delete_selected();
                            self.confirm_delete = false;
                        }
                    });
                });
        }

        if let Some(idx) = self.confirm_close_tab {
            egui::Window::new("Close tab")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    let dirty = self.tabs.get(idx).is_some_and(|t| t.dirty);
                    if dirty {
                        ui.label("Tab has unsaved changes. Save before closing?");
                    } else {
                        ui.label("Close tab?");
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirm_close_tab = None;
                        }
                        if dirty && ui.button("Save").clicked() {
                            if let Some(t) = self.tabs.get_mut(idx) {
                                let _ = t.save_notebook();
                            }
                            self.close_tab_index(idx);
                            self.confirm_close_tab = None;
                        }
                        if ui.button("Close").clicked() {
                            self.close_tab_index(idx);
                            self.confirm_close_tab = None;
                        }
                    });
                });
        }

        // Command palette (minimal)
        let mut palette_action: Option<&'static str> = None;
        if self.active_tab().show_palette {
            let actions = [
                ("Evaluate selection", "eval_selection"),
                ("Evaluate visible", "eval_visible"),
                ("Copy selection as JSON", "copy_json"),
                ("Copy selection as text", "copy_text"),
                ("Paste", "paste"),
                ("Collapse all outputs", "collapse_all"),
                ("Expand all outputs", "expand_all"),
            ];
            egui::Window::new("Command palette")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Press Esc to close");
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.active_tab_mut().show_palette = false;
                        return;
                    }

                    for (label, key) in actions {
                        if ui.button(label).clicked() {
                            palette_action = Some(key);
                            self.active_tab_mut().show_palette = false;
                        }
                    }
                });
        }

        if let Some(key) = palette_action {
            match key {
                "eval_selection" => self.tabs[active].enqueue_eval_selected(),
                "eval_visible" => self.tabs[active].enqueue_eval_all_visible(),
                "copy_json" => {
                    if let Some(s) = self.tabs[active].selection_as_json() {
                        self.tabs[active].set_internal_clipboard(s.clone());
                        ui.ctx().copy_text(s);
                    }
                }
                "copy_text" => {
                    let s = self.tabs[active].selection_as_text();
                    self.tabs[active].set_internal_clipboard(s.clone());
                    ui.ctx().copy_text(s);
                }
                "paste" => {
                    self.tabs[active].paste_groups_from_internal_clipboard(&mut self.next_group_id)
                }
                "collapse_all" => self.tabs[active].collapse_all_outputs(),
                "expand_all" => self.tabs[active].expand_all_outputs(),
                _ => {}
            }
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // Save all tabs best-effort.
        for t in &mut self.tabs {
            if t.dirty
                && let Err(err) = t.save_notebook()
            {
                tracing::warn!(tab_id = t.id, error = %err, "failed to save notebook");
            }
        }
    }
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    let mut visuals = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };

    // Push closer to the screenshot feel: softer panels and less contrasty strokes.
    let palette = UiPalette::for_theme(theme == Theme::Dark);
    visuals.panel_fill = palette.panel;
    visuals.faint_bg_color = palette.panel_alt;
    visuals.window_fill = palette.panel;
    visuals.extreme_bg_color = palette.background;
    visuals.widgets.noninteractive.bg_fill = palette.panel_alt;
    visuals.widgets.inactive.bg_fill = palette.panel_alt;
    visuals.widgets.hovered.bg_fill = palette.card_hover;
    visuals.widgets.active.bg_fill = palette.card_hover;
    visuals.widgets.noninteractive.bg_stroke = palette.subtle_stroke;
    visuals.widgets.inactive.bg_stroke = palette.subtle_stroke;
    visuals.widgets.hovered.bg_stroke = palette.stroke;
    visuals.widgets.active.bg_stroke = palette.stroke;

    // Softer corner radii, closer to the reference screenshots.
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);

    ctx.set_visuals(visuals);
}

fn autosave_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_string_lossy().to_string();
    s.push_str(".autosave");
    PathBuf::from(s)
}

fn group_accent_color(group: &CellGroup) -> egui::Color32 {
    match group.status {
        CellStatus::Error => egui::Color32::from_rgb(220, 80, 80),
        CellStatus::Running => egui::Color32::from_rgb(80, 160, 255),
        CellStatus::Idle => {
            if group.output.is_some() {
                egui::Color32::from_rgb(90, 210, 120) // output
            } else {
                egui::Color32::from_rgb(80, 160, 255) // input
            }
        }
    }
}

fn is_plot_like(output: &EvalResult) -> bool {
    let s = output.output_text.trim_start();
    let raw = output.raw_expr.as_deref().unwrap_or("");

    let needles = [
        "Graphics[",
        "Graphics3D[",
        "ListPlot[",
        "Plot[",
        "DateListPlot[",
        "Histogram[",
        "DensityPlot[",
        "ContourPlot[",
    ];

    needles.iter().any(|n| s.starts_with(n) || raw.contains(n))
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

fn wl_escape_string(s: &str) -> String {
    // Minimal escaping for embedding into a Wolfram Language string literal.
    s.replace('\\', "\\\\")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn unquote_json_string(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('"') && t.ends_with('"') {
        if let Ok(decoded) = serde_json::from_str::<String>(t) {
            return decoded;
        }
    }
    t.to_string()
}
