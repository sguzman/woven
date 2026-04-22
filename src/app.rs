use std::sync::Arc;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use tracing::{debug, error, info, instrument};

use crate::{
    config::AppConfig,
    kernel::{EvalResult, KernelSession},
};

#[derive(Debug, Clone)]
struct Cell {
    id: u64,
    input: String,
    output: Option<EvalResult>,
    status: CellStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellStatus {
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
    kernel: Option<KernelSession>,
    last_error: Option<String>,
}

impl WovenApp {
    #[instrument(skip_all)]
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let config = Arc::new(config);

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
            kernel: None,
            last_error: None,
        };

        app.ensure_one_cell();
        app
    }

    fn ensure_one_cell(&mut self) {
        if self.cells.is_empty() {
            self.cells.push(Cell {
                id: self.next_cell_id,
                input: "1+1".to_string(),
                output: None,
                status: CellStatus::Idle,
            });
            self.next_cell_id += 1;
            self.selected = 0;
        }
    }

    fn selected_cell_mut(&mut self) -> Option<&mut Cell> {
        self.cells.get_mut(self.selected)
    }

    fn get_or_start_kernel(&mut self) -> anyhow::Result<&mut KernelSession> {
        if self.kernel.is_none() {
            let kernel = KernelSession::new(&self.config.kernel)?;
            self.kernel = Some(kernel);
        }
        Ok(self.kernel.as_mut().expect("just set"))
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
        }

        info!("evaluating cell");
        let result = (|| -> anyhow::Result<EvalResult> {
            let kernel = self.get_or_start_kernel()?;
            kernel.evaluate(eval_id, &input)
        })();

        match result {
            Ok(out) => {
                debug!("eval ok");
                if let Some(cell) = self.cells.get_mut(self.selected) {
                    cell.output = Some(out);
                    cell.status = CellStatus::Idle;
                }
            },
            Err(err) => {
                error!(error = %err, "eval failed");
                self.last_error = Some(format!("{err:#}"));
                if let Some(cell) = self.cells.get_mut(self.selected) {
                    cell.status = CellStatus::Error;
                }
            },
        }
    }
}

impl eframe::App for WovenApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let last_error = self.last_error.clone();

        egui::Panel::top("top").show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Woven");
                if ui.button("New cell").clicked() {
                    let id = self.next_cell_id;
                    self.next_cell_id += 1;
                    self.cells.push(Cell {
                        id,
                        input: String::new(),
                        output: None,
                        status: CellStatus::Idle,
                    });
                    self.selected = self.cells.len().saturating_sub(1);
                }

                if ui.button("Evaluate").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl)
                {
                    self.evaluate_selected();
                }
            });
        });

        egui::Panel::left("cells")
            .resizable(true)
            .min_size(140.0)
            .show(&ctx, |ui| {
                ui.heading("Cells");
                ui.separator();
                for (idx, cell) in self.cells.iter().enumerate() {
                    let label = format!("#{}  {}", idx + 1, status_icon(cell.status));
                    if ui
                        .selectable_label(idx == self.selected, label)
                        .clicked()
                    {
                        self.selected = idx;
                    }
                }
            });

        egui::CentralPanel::default().show(&ctx, |ui| {
            let Some(cell) = self.selected_cell_mut() else {
                self.ensure_one_cell();
                return;
            };

            ui.horizontal(|ui| {
                ui.label(format!("Cell {}", cell.id));
                ui.label("Ctrl+Enter to evaluate");
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

                if self.config.plot.placeholder_enabled {
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
}

fn status_icon(status: CellStatus) -> &'static str {
    match status {
        CellStatus::Idle => " ",
        CellStatus::Running => "⏳",
        CellStatus::Error => "⚠",
    }
}
