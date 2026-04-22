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
    config::AppConfig,
    kernel::{EvalResult, KernelSession},
};

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

    // Execution flow
    eval_queue: VecDeque<usize>,
    last_rerun: Option<usize>,

    // Autosave
    last_autosave_at: Instant,

    // Internal clipboard (cross-platform; we still write to system clipboard for convenience).
    internal_clipboard: Option<String>,
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

    fn select_all(&mut self) {
        self.selection = self.groups.iter().map(|g| g.id).collect();
    }

    fn select_none(&mut self) {
        self.selection.clear();
    }

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
            self.eval_queue.push_back(idx);
        }
    }

    fn enqueue_eval_all_visible(&mut self) {
        for idx in self.visible_indices() {
            self.eval_queue.push_back(idx);
        }
    }

    fn tick_eval_queue(&mut self, max_per_frame: usize) {
        for _ in 0..max_per_frame {
            let Some(idx) = self.eval_queue.pop_front() else {
                return;
            };
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
            let result = self
                .kernel
                .evaluate(self.id * 1_000_000 + idx as u64, &input);
            let duration = started.elapsed();

            match result {
                Ok(out) => {
                    if let Some(group) = self.groups.get_mut(idx) {
                        group.output = Some(out);
                        group.status = CellStatus::Idle;
                        group.last_duration = Some(duration);
                    }
                    self.last_rerun = Some(idx);
                }
                Err(err) => {
                    error!(tab_id = self.id, group_id = %self.groups[idx].id, error = %err, "eval failed");
                    if let Some(group) = self.groups.get_mut(idx) {
                        group.status = CellStatus::Error;
                        group.last_duration = Some(duration);
                    }
                    self.last_rerun = Some(idx);
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
}

impl WovenApp {
    #[instrument(skip_all)]
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig, kernel: KernelSession) -> Self {
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.text_styles.iter_mut().for_each(|(_, font_id)| {
            font_id.size *= config.ui.font_scale;
        });
        cc.egui_ctx.set_global_style(style);

        let notebook_path = PathBuf::from(&config.ui.notebook_path);
        let mut tab = Tab {
            id: 1,
            title: "Tab 1".to_string(),
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
            eval_queue: VecDeque::new(),
            last_rerun: None,
            last_autosave_at: Instant::now(),
            internal_clipboard: None,
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
            title: format!("Tab {id}"),
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
            eval_queue: VecDeque::new(),
            last_rerun: None,
            last_autosave_at: Instant::now(),
            internal_clipboard: None,
        };
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
        self.active_tab_mut().tick_eval_queue(1);

        egui::Panel::top("tabs").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let mut label = t.title.clone();
                    if t.dirty {
                        label.push('*');
                    }
                    if ui.selectable_label(i == self.active_tab, label).clicked() {
                        self.active_tab = i;
                    }
                }
                ui.separator();
                ui.label(format!("Selected: {}", self.active_tab().selection.len()));
            });
        });

        let mut last_error: Option<String> = None;
        let active = self.active_tab;
        let kernel_cfg = self.config.kernel.clone();
        let mut toolbar_actions: Vec<&'static str> = Vec::new();
        egui::Panel::top("toolbar").show_inside(ui, |ui| {
            let tab_id = self.tabs[active].id;

            ui.horizontal(|ui| {
                ui.label(format!("Tab {tab_id}"));
                if ui.button("Save").clicked() {
                    toolbar_actions.push("save");
                }
                if ui.button("New group").clicked() {
                    toolbar_actions.push("new_group");
                }

                ui.separator();

                if ui.button("Eval selection").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl)
                {
                    toolbar_actions.push("eval_selection");
                }
                if ui.button("Eval visible").clicked()
                    || ui.input(|i| {
                        i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl && i.modifiers.shift
                    })
                {
                    toolbar_actions.push("eval_visible");
                }
                if ui.button("Abort").clicked() {
                    toolbar_actions.push("abort");
                }
                if ui.button("Rerun last").clicked() {
                    toolbar_actions.push("rerun_last");
                }

                ui.separator();

                if ui.button("Restart kernel").clicked() {
                    toolbar_actions.push("restart_kernel");
                }

                ui.separator();

                let tab = &mut self.tabs[active];
                ui.checkbox(&mut tab.filter_errors_only, "Errors");
                ui.checkbox(&mut tab.filter_messages_only, "Messages");
                ui.add(egui::TextEdit::singleline(&mut tab.filter).hint_text("Search…"));
            });

            ui.horizontal(|ui| {
                if ui.button("Select all").clicked() {
                    toolbar_actions.push("select_all");
                }
                if ui.button("Select none").clicked() {
                    toolbar_actions.push("select_none");
                }
                if ui.button("Invert").clicked() {
                    toolbar_actions.push("invert");
                }
                ui.separator();
                if ui.button("Copy JSON").clicked() {
                    toolbar_actions.push("copy_json");
                }
                if ui.button("Copy text").clicked() {
                    toolbar_actions.push("copy_text");
                }
                if ui.button("Paste").clicked() {
                    toolbar_actions.push("paste");
                }
            });

            ui.horizontal(|ui| {
                if ui.button("Duplicate").clicked() {
                    toolbar_actions.push("duplicate");
                }
                if ui.button("Move up").clicked() {
                    toolbar_actions.push("move_up");
                }
                if ui.button("Move down").clicked() {
                    toolbar_actions.push("move_down");
                }
                if ui.button("Clear outputs").clicked() {
                    toolbar_actions.push("clear_outputs");
                }
                if ui.button("Collapse all").clicked() {
                    toolbar_actions.push("collapse_all");
                }
                if ui.button("Expand all").clicked() {
                    toolbar_actions.push("expand_all");
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    toolbar_actions.push("delete_confirm");
                }
            });
        });

        for a in toolbar_actions {
            match a {
                "save" => {
                    if let Err(err) = self.tabs[active].save_notebook() {
                        last_error = Some(format!("save failed: {err:#}"));
                    }
                }
                "new_group" => {
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
                    tab.dirty = true;
                }
                "eval_selection" => self.tabs[active].enqueue_eval_selected(),
                "eval_visible" => self.tabs[active].enqueue_eval_all_visible(),
                "abort" => {
                    if let Err(err) = self.tabs[active].kernel.abort() {
                        last_error = Some(format!("abort failed: {err:#}"));
                    }
                }
                "rerun_last" => {
                    if let Some(idx) = self.tabs[active].last_rerun {
                        self.tabs[active].eval_queue.push_back(idx);
                    }
                }
                "restart_kernel" => {
                    if let Err(err) = self.tabs[active].kernel.restart(&kernel_cfg) {
                        last_error = Some(format!("kernel restart failed: {err:#}"));
                    }
                }
                "select_all" => self.tabs[active].select_all(),
                "select_none" => self.tabs[active].select_none(),
                "invert" => self.tabs[active].invert_selection(),
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
                "duplicate" => self.tabs[active].duplicate_selected(&mut self.next_group_id),
                "move_up" => self.tabs[active].move_selected_block(-1),
                "move_down" => self.tabs[active].move_selected_block(1),
                "clear_outputs" => self.tabs[active].clear_outputs_selected(),
                "collapse_all" => self.tabs[active].collapse_all_outputs(),
                "expand_all" => self.tabs[active].expand_all_outputs(),
                "delete_confirm" => self.confirm_delete = true,
                _ => {}
            }
        }

        egui::Panel::left("groups")
            .resizable(true)
            .min_size(220.0)
            .show_inside(ui, |ui| {
                let tab = self.active_tab_mut();
                ui.heading("Groups");
                ui.separator();

                let visible = tab.visible_indices();
                for idx in visible {
                    let group_id = tab.groups[idx].id;
                    let selected = idx == tab.selected;

                    ui.horizontal(|ui| {
                        let mut checked = tab.selection.contains(&group_id);
                        let response = ui.checkbox(&mut checked, "");
                        if response.clicked() {
                            let modifiers = ui.input(|i| i.modifiers);
                            if modifiers.shift {
                                let anchor = tab.selection_anchor.unwrap_or(tab.selected);
                                tab.select_range(anchor, idx);
                            } else if modifiers.ctrl {
                                tab.toggle_selection_for(idx);
                            } else {
                                tab.selection.clear();
                                tab.toggle_selection_for(idx);
                            }
                            tab.selection_anchor = Some(idx);
                        }

                        let label =
                            format!("#{:03} {}", idx + 1, status_icon(tab.groups[idx].status));
                        if ui.selectable_label(selected, label).clicked() {
                            let modifiers = ui.input(|i| i.modifiers);
                            tab.set_selected(idx);
                            if modifiers.shift {
                                let anchor = tab.selection_anchor.unwrap_or(tab.selected);
                                tab.select_range(anchor, idx);
                            } else if modifiers.ctrl {
                                tab.toggle_selection_for(idx);
                            } else {
                                tab.selection.clear();
                                tab.toggle_selection_for(idx);
                            }
                            tab.selection_anchor = Some(idx);
                        }

                        if ui.button("★").on_hover_text("Bookmark").clicked() {
                            tab.groups[idx].bookmarked = !tab.groups[idx].bookmarked;
                            tab.dirty = true;
                        }
                    });
                }
            });

        egui::Panel::right("outline")
            .resizable(true)
            .min_size(200.0)
            .show_inside(ui, |ui| {
                let tab = self.active_tab_mut();
                ui.heading("Outline");
                ui.separator();
                ui.label("Bookmarks");
                let mut jump_to: Option<usize> = None;
                for (idx, g) in tab.groups.iter().enumerate() {
                    if !g.bookmarked {
                        continue;
                    }
                    let label = format!("#{:03} {}", idx + 1, preview_title(&g.input));
                    if ui.button(label).clicked() {
                        jump_to = Some(idx);
                    }
                }
                if let Some(idx) = jump_to {
                    tab.set_selected(idx);
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(err) = last_error {
                ui.colored_label(egui::Color32::RED, err);
                ui.separator();
            }

            let (tabs, next_group_id) = (&mut self.tabs, &mut self.next_group_id);
            tabs[active].ensure_one_group(next_group_id);

            let tab = &mut tabs[active];
            let selected_idx = tab.selected;
            let placeholder_enabled = tab.kernel_plot_placeholder_enabled();

            if selected_idx >= tab.groups.len() {
                return;
            }

            // Edit phase (mutable borrow)
            let input_changed = {
                let group = &mut tab.groups[selected_idx];
                ui.horizontal(|ui| {
                    ui.heading(format!("Group {}", group.id));
                    if let Some(d) = group.last_duration {
                        ui.label(format!("Last eval: {} ms", d.as_millis()));
                    }
                    if ui.checkbox(&mut group.collapsed, "Collapsed").changed() {
                        tab.dirty = true;
                    }
                    if ui
                        .button("Format")
                        .on_hover_text("Trim trailing whitespace")
                        .clicked()
                    {
                        group.input = format_input(&group.input);
                        tab.dirty = true;
                    }
                    if ui.button("Snippet: Plot").clicked() {
                        group.input.push_str("\nPlot[Sin[x], {x, 0, 10}]");
                        tab.dirty = true;
                    }
                    if ui.button("Snippet: ListPlot").clicked() {
                        group
                            .input
                            .push_str("\nListPlot[Table[Sin[x], {x, 0, 10, 0.1}]]");
                        tab.dirty = true;
                    }
                });
                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("Tags:");
                    let mut tags = group.tags.join(", ");
                    if ui
                        .add(egui::TextEdit::singleline(&mut tags).hint_text("comma,separated"))
                        .changed()
                    {
                        group.tags = tags
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect();
                        tab.dirty = true;
                    }
                    if ui.checkbox(&mut group.bookmarked, "Bookmarked").changed() {
                        tab.dirty = true;
                    }
                });

                let response = ui.add(
                    egui::TextEdit::multiline(&mut group.input)
                        .code_editor()
                        .desired_rows(8)
                        .hint_text("Enter Wolfram Language input…"),
                );
                response.changed()
            };
            if input_changed {
                tab.dirty = true;
            }

            // Render phase (immutable borrows)
            ui.separator();
            ui.heading("Output");

            let group = &tab.groups[selected_idx];
            if let Some(out) = &group.output {
                if group.collapsed {
                    ui.label("(collapsed)");
                } else {
                    let output_text =
                        truncate_str(&out.output_text, self.config.plot.max_output_chars);
                    if output_text.is_empty() {
                        ui.label("(no output)");
                    } else {
                        ui.add(
                            egui::TextEdit::multiline(&mut output_text.to_string())
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(6)
                                .interactive(false),
                        );
                    }

                    let messages: Vec<&String> = out
                        .messages
                        .iter()
                        .take(self.config.plot.max_messages)
                        .collect();
                    if !messages.is_empty() {
                        ui.separator();
                        ui.heading("Messages");
                        for m in messages {
                            ui.label(m);
                        }
                    }

                    if placeholder_enabled && is_plot_like(out) {
                        ui.separator();
                        ui.heading("Plot (placeholder)");
                        Plot::new("plot_placeholder").show(ui, |plot_ui| {
                            let points: PlotPoints = (0..100)
                                .map(|i| {
                                    let x = i as f64 / 10.0;
                                    [x, (x).sin()]
                                })
                                .collect();
                            plot_ui.line(Line::new("sin(x)", points));
                        });
                    }
                }
            } else {
                ui.label("Evaluate to see output.");
            }
        });

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

impl Tab {
    fn kernel_plot_placeholder_enabled(&self) -> bool {
        // Keep the plot placeholder behind config in the future; for now it is always on.
        true
    }
}

fn autosave_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_string_lossy().to_string();
    s.push_str(".autosave");
    PathBuf::from(s)
}

fn status_icon(status: CellStatus) -> &'static str {
    match status {
        CellStatus::Idle => " ",
        CellStatus::Running => "⏳",
        CellStatus::Error => "⚠",
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

fn preview_title(input: &str) -> String {
    let s = input.lines().next().unwrap_or("").trim();
    if s.is_empty() {
        "(empty)".to_string()
    } else {
        s.chars().take(32).collect()
    }
}

fn format_input(input: &str) -> String {
    input
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
