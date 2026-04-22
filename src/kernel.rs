use std::{
    path::PathBuf,
    process,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::{debug, info, instrument};
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
    pub fn restart_with_current_config(&mut self) -> anyhow::Result<()> {
        let cfg = self.config.clone();
        self.restart(&cfg)
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

        // IMPORTANT:
        // The kernel can emit relative symbols (without contexts) inside packets like MESSAGEPKT.
        // The `wstp` crate rejects those when parsing into `Expr`, which can poison the link.
        //
        // Strategy:
        // - Ask the kernel to return a single JSON *string* for every evaluation.
        // - Use Quiet + $MessageList to avoid MESSAGEPKT and capture messages ourselves.
        // - Prefer reading RETURNPKT as a raw string (no symbol parsing).
        let wl = format!(
            "Block[{{$MessageList={{}}}}, \
             Module[{{res}}, \
               res = Quiet[Check[ToExpression[\"{input}\"], $Failed], All]; \
               ExportString[<|\
                 \"output\" -> ToString[res, InputForm], \
                 \"raw\" -> ToString[res, FullForm], \
                 \"messages\" -> (ToString[#, InputForm] & /@ $MessageList)\
               |>, \"JSON\"]\
             ]]",
            input = wl_escape_string(wl_input),
        );
        let expr = Expr::normal(Symbol::new("System`ToExpression"), vec![Expr::string(wl)]);

        self.link
            .put_eval_packet(&expr)
            .map_err(|e| anyhow!("{e:?}"))?;
        self.link.flush().map_err(|e| anyhow!("{e:?}"))?;

        let mut messages = Vec::new();

        loop {
            wait_until(&mut self.link, deadline).context("wait for kernel activity")?;
            let pkt = match self.link.raw_next_packet() {
                Ok(pkt) => pkt,
                Err(e) => {
                    let _ = self.link.new_packet();
                    return Err(anyhow!("{e:?}"));
                }
            };

            match pkt {
                sys::RETURNPKT => {
                    // The wrapper always returns a single JSON string. If we can't read a string
                    // token here, something has gone badly wrong; fail fast (but still try to
                    // advance packets so the session doesn't get stuck).
                    let json = match self.link.get_string() {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = self.link.new_packet();
                            return Err(anyhow!("{e:?}"));
                        }
                    };

                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;

                    let decoded: JsonValue = serde_json::from_str(&json).with_context(|| {
                        format!(
                            "kernel returned non-JSON string: {}",
                            truncate_for_log(&json)
                        )
                    })?;

                    let output_text = decoded
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let raw_expr = decoded
                        .get("raw")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if let Some(arr) = decoded.get("messages").and_then(|v| v.as_array()) {
                        for m in arr.iter().filter_map(|v| v.as_str()) {
                            messages.push(m.to_string());
                        }
                    }

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
                    }
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
                sys::MESSAGEPKT => {
                    // Avoid parsing MESSAGEPKT (it may contain relative symbols).
                    // We rely on Quiet + $MessageList in the RETURNPKT payload instead.
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

fn wl_escape_string(s: &str) -> String {
    // Minimal escaping for embedding into a Wolfram Language string literal.
    s.replace('\\', "\\\\")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn truncate_for_log(s: &str) -> String {
    const MAX: usize = 400;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}…", &s[..MAX])
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
