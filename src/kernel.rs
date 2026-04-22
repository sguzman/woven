use std::path::PathBuf;

use anyhow::{anyhow, Context};
use tracing::{debug, info, instrument, warn};
use wolfram_app_discovery::WolframApp;
use wolfram_expr::{Expr, Symbol};
use wstp::{kernel::WolframKernelProcess, sys, Link};

use crate::config::KernelConfig;

#[derive(Debug, Clone)]
pub struct EvalResult {
    pub output_text: String,
    pub messages: Vec<String>,
    pub raw_expr: Option<String>,
}

#[derive(Debug)]
pub struct KernelSession {
    kernel: WolframKernelProcess,
}

impl KernelSession {
    #[instrument(skip_all, fields(kernel_path = %kernel_path_string(config)))]
    pub fn new(config: &KernelConfig) -> anyhow::Result<Self> {
        let exe = discover_kernel_executable(config).context("discover kernel executable")?;

        // NOTE: wstp::kernel::WolframKernelProcess::launch currently blocks forever
        // waiting for link activation. We record timeouts in config, but don't yet
        // enforce them. (Implementing timeouts requires non-blocking activation and
        // threading; Link is not guaranteed Send.)
        if config.start_timeout_ms > 0 {
            debug!(start_timeout_ms = config.start_timeout_ms, "kernel start timeout configured (not yet enforced)");
        }

        info!(path = %exe.display(), "launching wolfram kernel via WSTP");
        let kernel = WolframKernelProcess::launch(&exe).map_err(|e| anyhow!("{e:?}"))?;
        info!("kernel connected");

        Ok(Self { kernel })
    }

    #[instrument(skip_all, fields(eval_id))]
    pub fn evaluate(&mut self, eval_id: u64, wl_input: &str) -> anyhow::Result<EvalResult> {
        if wl_input.trim().is_empty() {
            return Ok(EvalResult {
                output_text: String::new(),
                messages: Vec::new(),
                raw_expr: None,
            });
        }

        // NOTE: Similar to start timeout, eval timeout is not enforced yet.
        debug!(
            eval_timeout_ms = %"<not enforced>",
            "evaluation requested"
        );

        let expr = Expr::normal(
            Symbol::new("System`ToExpression"),
            vec![Expr::string(wl_input)],
        );

        let link = self.kernel.link();
        link.put_eval_packet(&expr).map_err(|e| anyhow!("{e:?}"))?;
        link.flush().map_err(|e| anyhow!("{e:?}"))?;

        let mut messages = Vec::new();
        let mut output_text: Option<String> = None;
        let mut raw_expr: Option<String> = None;

        loop {
            let pkt = link.raw_next_packet().map_err(|e| anyhow!("{e:?}"))?;

            match pkt {
                sys::RETURNPKT => {
                    let result = link.get_expr().map_err(|e| anyhow!("{e:?}"))?;
                    raw_expr = Some(format!("{result:?}"));

                    output_text = Some(
                        expr_to_input_form_string(link, &result)
                        .unwrap_or_else(|err| {
                            warn!(error = %err, "failed to stringify result");
                            format!("{result:?}")
                        }),
                    );

                    link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                    break;
                },
                sys::TEXTPKT | sys::RETURNTEXTPKT => {
                    if let Ok(s) = link.get_string() {
                        messages.push(s);
                    } else {
                        messages.push("<text packet decode error>".to_string());
                    }
                    link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                },
                sys::MESSAGEPKT => {
                    // MESSAGEPKT structure: MessagePacket[symbol, messageName]
                    // We'll read it as a generic Expr and debug-print it.
                    let msg = link.get_expr().map_err(|e| anyhow!("{e:?}"))?;
                    messages.push(format!("{msg:?}"));
                    link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                },
                sys::INPUTNAMEPKT
                | sys::OUTPUTNAMEPKT
                | sys::SYNTAXPKT
                | sys::DISPLAYPKT
                | sys::SUSPENDPKT
                | sys::RESUMEPKT
                | sys::BEGINDLGPKT
                | sys::ENDDLGPKT
                | sys::FIRSTUSERPKT
                | sys::LASTUSERPKT
                | sys::ILLEGALPKT => {
                    // Consume/skip.
                    link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                },
                other => {
                    debug!(packet = other, "unhandled packet type; skipping");
                    link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                },
            }
        }

        info!(
            eval_id,
            output_len = output_text.as_deref().map(|s| s.len()).unwrap_or(0),
            message_count = messages.len(),
            "evaluation complete"
        );

        Ok(EvalResult {
            output_text: output_text.unwrap_or_default(),
            messages,
            raw_expr,
        })
    }
}

fn kernel_path_string(config: &KernelConfig) -> String {
    if config.path.trim().is_empty() {
        "<auto>".to_string()
    } else {
        config.path.clone()
    }
}

fn discover_kernel_executable(config: &KernelConfig) -> anyhow::Result<PathBuf> {
    if !config.path.trim().is_empty() {
        let p = PathBuf::from(config.path.trim());
        if p.is_file() {
            return Ok(p);
        }
        return Err(anyhow!("kernel.path does not exist or is not a file: {}", p.display()));
    }

    let app = WolframApp::try_default().context("wolfram-app-discovery could not find a Wolfram installation")?;
    app.kernel_executable_path()
        .context("failed to get kernel executable path from wolfram-app-discovery")
}

fn expr_to_input_form_string(link: &mut Link, result: &Expr) -> anyhow::Result<String> {
    let to_string_expr = Expr::normal(
        Symbol::new("System`ToString"),
        vec![result.clone(), Expr::symbol(Symbol::new("System`InputForm"))],
    );

    link.put_eval_packet(&to_string_expr)
        .map_err(|e| anyhow!("{e:?}"))?;
    link.flush().map_err(|e| anyhow!("{e:?}"))?;

    loop {
        let pkt = link.raw_next_packet().map_err(|e| anyhow!("{e:?}"))?;
        if pkt == sys::RETURNPKT {
            let s_expr = link.get_expr().map_err(|e| anyhow!("{e:?}"))?;
            link.new_packet().map_err(|e| anyhow!("{e:?}"))?;

            // Best-effort: if it's a string, show the string; else debug.
            if let wolfram_expr::ExprKind::String(s) = s_expr.kind() {
                return Ok(s.as_str().to_string());
            }
            return Ok(format!("{s_expr:?}"));
        } else {
            link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_path_string_auto_when_empty() {
        let cfg = KernelConfig {
            path: "".to_string(),
            start_timeout_ms: 1,
            eval_timeout_ms: 1,
        };
        assert_eq!(kernel_path_string(&cfg), "<auto>");
    }
}
