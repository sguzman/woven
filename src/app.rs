use std::time::{Duration, Instant};
use std::{fs, path::PathBuf, sync::Arc};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument};

use crate::{
    config::AppConfig,
    kernel::{EvalResult, KernelSession},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cell {
    id: u64,
    input: String,
    output: Option<EvalResult>,
    #[serde(skip, default)]
    status: CellStatus,
    #[serde(skip, default)]
    last_duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum CellStatus {
    #[default]
    Idle,
    Running,
    Error,
}

pub struct WovenApp {
    config: Arc<AppConfig>,
    cells: Vec<Cell>,
    selected: usize,
    next_cell_id: u64,
    next_eval_id: u64,
    kernel: KernelSession,
    last_error: Option<String>,
    notebook_path: PathBuf,
}

impl WovenApp {
    #[instrument(skip_all)]
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig, kernel: KernelSession) -> Self {
        let config = Arc::new(config);
        let notebook_path = PathBuf::from(&config.ui.notebook_path);

        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.text_styles.iter_mut().for_each(|(_, font_id)| {
            font_id.size *= config.ui.font_scale;
        });
        cc.egui_ctx.set_global_style(style);

        let mut app = Self {
            config,
            cells: Vec::new(),
            selected: 0,
            next_cell_id: 1,
            next_eval_id: 1,
            kernel,
            last_error: None,
            notebook_path,
        };

        if let Err(err) = app.load_notebook() {
            debug!(error = %err, "failed to load notebook; starting new");
        }
        app.ensure_one_cell();
        app
    }

    fn load_notebook(&mut self) -> anyhow::Result<()> {
        let path = self.notebook_path.clone();
        if !path.is_file() {
            return Ok(());
        }

        let bytes = fs::read(&path)?;
        let mut cells: Vec<Cell> = serde_json::from_slice(&bytes)?;

        // Ensure runtime-only fields are initialized.
        for c in &mut cells {
            c.status = CellStatus::Idle;
            c.last_duration = None;
        }

        self.next_cell_id = cells.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.cells = cells;
        self.selected = self.selected.min(self.cells.len().saturating_sub(1));
        Ok(())
    }

    fn save_notebook(&self) -> anyhow::Result<()> {
        let path = self.notebook_path.clone();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let bytes = serde_json::to_vec_pretty(&self.cells)?;
        fs::write(&path, bytes)?;
        Ok(())
    }

    fn ensure_one_cell(&mut self) {
        if self.cells.is_empty() {
            self.cells.push(Cell {
                id: self.next_cell_id,
                input: "1+1".to_string(),
                output: None,
                status: CellStatus::Idle,
                last_duration: None,
            });
            self.next_cell_id += 1;
            self.selected = 0;
        }
    }

    fn selected_cell_mut(&mut self) -> Option<&mut Cell> {
        self.cells.get_mut(self.selected)
    }

    #[instrument(skip_all, fields(cell_id, eval_id))]
    fn evaluate_selected(&mut self) {
        let eval_id = self.next_eval_id;
        self.next_eval_id += 1;

        let Some(cell_id) = self.cells.get(self.selected).map(|c| c.id) else {
            return;
        };

        self.last_error = None;

        tracing::Span::current().record("cell_id", cell_id);
        tracing::Span::current().record("eval_id", eval_id);

        let input = self
            .cells
            .get(self.selected)
            .map(|c| c.input.clone())
            .unwrap_or_default();

        if let Some(cell) = self.cells.get_mut(self.selected) {
            cell.status = CellStatus::Running;
            cell.output = None;
            cell.last_duration = None;
        }

        info!("evaluating cell");
        let started = Instant::now();
        let result = self.kernel.evaluate(eval_id, &input);
        let duration = started.elapsed();

        match result {
            Ok(out) => {
                debug!("eval ok");
                if let Some(cell) = self.cells.get_mut(self.selected) {
                    cell.output = Some(out);
                    cell.status = CellStatus::Idle;
                    cell.last_duration = Some(duration);
                }
            }
            Err(err) => {
                error!(error = %err, "eval failed");
                self.last_error = Some(format!("{err:#}"));
                if let Some(cell) = self.cells.get_mut(self.selected) {
                    cell.status = CellStatus::Error;
                    cell.last_duration = Some(duration);
                }
            }
        }
    }

    #[instrument(skip_all)]
    fn evaluate_all(&mut self) {
        for idx in 0..self.cells.len() {
            self.selected = idx;
            self.evaluate_selected();
        }
    }

    fn move_selected(&mut self, delta: isize) {
        let len = self.cells.len();
        if len == 0 {
            return;
        }
        let from = self.selected;
        let to = (from as isize + delta).clamp(0, (len - 1) as isize) as usize;
        if from == to {
            return;
        }
        let cell = self.cells.remove(from);
        self.cells.insert(to, cell);
        self.selected = to;
    }
}

impl eframe::App for WovenApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let last_error = self.last_error.clone();

        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Woven");
                if ui.button("Save").clicked()
                    && let Err(err) = self.save_notebook()
                {
                    self.last_error = Some(format!("save failed: {err:#}"));
                }
                if ui.button("New cell").clicked() {
                    let id = self.next_cell_id;
                    self.next_cell_id += 1;
                    self.cells.push(Cell {
                        id,
                        input: String::new(),
                        output: None,
                        status: CellStatus::Idle,
                        last_duration: None,
                    });
                    self.selected = self.cells.len().saturating_sub(1);
                }

                if ui.button("Evaluate").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl)
                {
                    self.evaluate_selected();
                }

                if ui.button("Evaluate all").clicked()
                    || ui.input(|i| {
                        i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl && i.modifiers.shift
                    })
                {
                    self.evaluate_all();
                }

                ui.separator();

                if ui.button("Move up").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::ArrowUp) && i.modifiers.alt)
                {
                    self.move_selected(-1);
                }
                if ui.button("Move down").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::ArrowDown) && i.modifiers.alt)
                {
                    self.move_selected(1);
                }

                ui.separator();

                if ui.button("Restart kernel").clicked()
                    && let Err(err) = self.kernel.restart(&self.config.kernel)
                {
                    self.last_error = Some(format!("kernel restart failed: {err:#}"));
                }
            });
        });

        egui::Panel::left("cells")
            .resizable(true)
            .min_size(140.0)
            .show_inside(ui, |ui| {
                ui.heading("Cells");
                ui.separator();
                for (idx, cell) in self.cells.iter().enumerate() {
                    let label = format!("#{}  {}", idx + 1, status_icon(cell.status));
                    if ui.selectable_label(idx == self.selected, label).clicked() {
                        self.selected = idx;
                    }
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let placeholder_enabled = self.config.plot.placeholder_enabled;
            let Some(cell) = self.selected_cell_mut() else {
                self.ensure_one_cell();
                return;
            };

            ui.horizontal(|ui| {
                ui.label(format!("Cell {}", cell.id));
                ui.label("Ctrl+Enter to evaluate");
                if let Some(d) = cell.last_duration {
                    ui.label(format!("Last eval: {} ms", d.as_millis()));
                }
            });
            ui.separator();

            ui.add(
                egui::TextEdit::multiline(&mut cell.input)
                    .code_editor()
                    .desired_rows(8)
                    .hint_text("Enter Wolfram Language input…"),
            );

            ui.separator();

            if let Some(err) = &last_error {
                ui.colored_label(egui::Color32::RED, err);
                ui.separator();
            }

            ui.heading("Output");

            if let Some(output) = &cell.output {
                if !output.output_text.is_empty() {
                    ui.add(
                        egui::TextEdit::multiline(&mut output.output_text.clone())
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(6)
                            .interactive(false),
                    );
                } else {
                    ui.label("(no output)");
                }

                if !output.messages.is_empty() {
                    ui.separator();
                    ui.heading("Messages");
                    for msg in &output.messages {
                        ui.label(msg);
                    }
                }

                if placeholder_enabled && is_plot_like(output) {
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
            } else {
                ui.label("Evaluate to see output.");
            }
        });
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // eframe persistence is optional; we do our own file-based persistence.
        if let Err(err) = self.save_notebook() {
            tracing::warn!(error = %err, "failed to save notebook");
        }
    }
}

fn status_icon(status: CellStatus) -> &'static str {
    match status {
        CellStatus::Idle => " ",
        CellStatus::Running => "⏳",
        CellStatus::Error => "⚠",
    }
}

fn is_plot_like(output: &EvalResult) -> bool {
    let s = output.output_text.trim_start().to_string();
    let raw = output.raw_expr.as_deref().unwrap_or("");

    // Extremely early heuristic. We'll replace with WL-side normalization and a real
    // payload schema.
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
