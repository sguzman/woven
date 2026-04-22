use std::{
    path::PathBuf,
    process,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument, warn};
use wolfram_app_discovery::WolframApp;
use wolfram_expr::{Expr, Symbol};
use wstp::{Link, Protocol, sys};

use crate::config::KernelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub output_text: String,
    pub messages: Vec<String>,
    pub raw_expr: Option<String>,
}

#[derive(Debug)]
pub struct KernelSession {
    process: process::Child,
    link: Link,
    config: KernelConfig,
}

impl KernelSession {
    #[instrument(skip_all, fields(kernel_path = %kernel_path_string(config)))]
    pub fn new(config: &KernelConfig) -> anyhow::Result<Self> {
        let exe = discover_kernel_executable(config).context("discover kernel executable")?;

        info!(path = %exe.display(), "launching wolfram kernel via WSTP");
        let (process, link) = launch_kernel_with_timeout(&exe, config.start_timeout_ms)
            .context("launch kernel with timeout")?;
        info!("kernel connected");

        Ok(Self {
            process,
            link,
            config: config.clone(),
        })
    }

    #[instrument(skip_all)]
    pub fn restart(&mut self, config: &KernelConfig) -> anyhow::Result<()> {
        // Best-effort: terminate the old process and establish a new one.
        let _ = self.process.kill();
        let _ = self.process.wait();

        let exe = discover_kernel_executable(config).context("discover kernel executable")?;
        info!(path = %exe.display(), "restarting wolfram kernel via WSTP");
        let (process, link) = launch_kernel_with_timeout(&exe, config.start_timeout_ms)
            .context("launch kernel with timeout")?;

        self.process = process;
        self.link = link;
        self.config = config.clone();
        Ok(())
    }

    #[instrument(skip_all)]
    pub fn abort(&mut self) -> anyhow::Result<()> {
        use wstp::UrgentMessage;
        self.link
            .put_message(UrgentMessage::ABORT)
            .map_err(|e| anyhow!("{e:?}"))?;
        Ok(())
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

        let deadline = Instant::now()
            .checked_add(Duration::from_millis(self.config.eval_timeout_ms))
            .expect("deadline overflow");

        let expr = Expr::normal(
            Symbol::new("System`ToExpression"),
            vec![Expr::string(wl_input)],
        );

        self.link
            .put_eval_packet(&expr)
            .map_err(|e| anyhow!("{e:?}"))?;
        self.link.flush().map_err(|e| anyhow!("{e:?}"))?;

        let mut messages = Vec::new();

        loop {
            wait_until(&mut self.link, deadline).context("wait for kernel activity")?;
            let pkt = self.link.raw_next_packet().map_err(|e| anyhow!("{e:?}"))?;

            match pkt {
                sys::RETURNPKT => {
                    let result = self.link.get_expr().map_err(|e| anyhow!("{e:?}"))?;
                    let raw_expr = Some(format!("{result:?}"));

                    let output_text = expr_to_input_form_string(&mut self.link, deadline, &result)
                        .unwrap_or_else(|err| {
                            warn!(error = %err, "failed to stringify result");
                            format!("{result:?}")
                        });

                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;

                    info!(
                        eval_id,
                        output_len = output_text.len(),
                        message_count = messages.len(),
                        "evaluation complete"
                    );

                    return Ok(EvalResult {
                        output_text,
                        messages,
                        raw_expr,
                    });
                }
                sys::TEXTPKT | sys::RETURNTEXTPKT => {
                    if let Ok(s) = self.link.get_string() {
                        messages.push(s);
                    } else {
                        messages.push("<text packet decode error>".to_string());
                    }
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
                sys::MESSAGEPKT => {
                    // MESSAGEPKT structure: MessagePacket[symbol, messageName]
                    // We'll read it as a generic Expr and debug-print it.
                    let msg = self.link.get_expr().map_err(|e| anyhow!("{e:?}"))?;
                    messages.push(format!("{msg:?}"));
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
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
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
                other => {
                    debug!(packet = other, "unhandled packet type; skipping");
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
            }
        }

        // Unreachable: the kernel is expected to eventually send RETURNPKT for an evaluation.
        // If it doesn't, `wait_until` will time out and return an error.
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
        return Err(anyhow!(
            "kernel.path does not exist or is not a file: {}",
            p.display()
        ));
    }

    let app = WolframApp::try_default()
        .context("wolfram-app-discovery could not find a Wolfram installation")?;
    app.kernel_executable_path()
        .context("failed to get kernel executable path from wolfram-app-discovery")
}

fn expr_to_input_form_string(
    link: &mut Link,
    deadline: Instant,
    result: &Expr,
) -> anyhow::Result<String> {
    let to_string_expr = Expr::normal(
        Symbol::new("System`ToString"),
        vec![
            result.clone(),
            Expr::symbol(Symbol::new("System`InputForm")),
        ],
    );

    link.put_eval_packet(&to_string_expr)
        .map_err(|e| anyhow!("{e:?}"))?;
    link.flush().map_err(|e| anyhow!("{e:?}"))?;

    loop {
        wait_until(link, deadline).context("wait for kernel activity (ToString)")?;
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

fn launch_kernel_with_timeout(
    kernel_exe: &PathBuf,
    start_timeout_ms: u64,
) -> anyhow::Result<(process::Child, Link)> {
    let mut link = Link::listen(Protocol::SharedMemory, "").map_err(|e| anyhow!("{e:?}"))?;
    let name = link.link_name();
    if name.is_empty() {
        return Err(anyhow!("WSTP link name was empty"));
    }

    let child = process::Command::new(kernel_exe)
        .arg("-wstp")
        .arg("-linkprotocol")
        .arg("SharedMemory")
        .arg("-linkconnect")
        .arg("-linkname")
        .arg(&name)
        .spawn()
        .context("spawn WolframKernel")?;

    let deadline = Instant::now()
        .checked_add(Duration::from_millis(start_timeout_ms))
        .ok_or_else(|| anyhow!("start timeout overflow"))?;

    // Wait for activity, then activate. If the kernel never connects (e.g. licensing
    // prompt), we time out instead of hanging forever.
    wait_until(&mut link, deadline).context("wait for kernel connect")?;
    link.activate().map_err(|e| anyhow!("{e:?}"))?;

    Ok((child, link))
}

fn wait_until(link: &mut Link, deadline: Instant) -> anyhow::Result<()> {
    use std::ops::ControlFlow;

    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!("timeout waiting for kernel/WSTP activity"));
        }

        let got_data = link
            .wait_with_callback(|_link| {
                if Instant::now() < deadline {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            })
            .map_err(|e| anyhow!("{e:?}"))?;

        if got_data {
            return Ok(());
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
