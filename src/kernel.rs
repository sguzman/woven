use std::{
    path::PathBuf,
    process,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};
use wolfram_app_discovery::WolframApp;
use wolfram_expr::{Expr, ExprKind, Symbol};
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

        // IMPORTANT:
        // The kernel can emit relative symbols (without contexts) inside packets like MESSAGEPKT.
        // The `wstp` crate rejects those when parsing into `Expr`, which can poison the link.
        //
        // Strategy:
        // - Ask the kernel to return a simple `{out, raw, msgs}` list for every evaluation.
        //   (We intentionally do NOT use `ExportString[..., "JSON"]` because JSON export can be
        //   unavailable in some installations, yielding `$Failed`.)
        // - Use Quiet + $MessageList to avoid MESSAGEPKT and capture messages ourselves.
        // - Parse RETURNPKT expression with a permissive symbol resolver.
        let wl = format!(
            "Block[{{$MessageList={{}}}}, \
             Module[{{res, out, raw, msgs}}, \
               res = Quiet[Check[ToExpression[\"{input}\"], $Failed], All]; \
               out = ToString[res, InputForm]; \
               raw = ToString[res, InputForm]; \
               msgs = (ToString[#, InputForm] & /@ $MessageList); \
               {{out, raw, msgs}}\
             ]]",
            input = wl_escape_string(wl_input),
        );
        let (expr, mut messages) = self.eval_return_expr(eval_id, &wl)?;

        let (output_text, raw, msgs_from_kernel) =
            parse_eval_tuple3_strings(&expr).context("parse eval result tuple")?;
        messages.extend(msgs_from_kernel);

        info!(
            eval_id,
            output_len = output_text.len(),
            message_count = messages.len(),
            "evaluation complete"
        );

        Ok(EvalResult {
            output_text,
            messages,
            raw_expr: Some(raw),
        })
    }

    #[instrument(skip_all, fields(eval_id))]
    pub fn evaluate_string_list(
        &mut self,
        eval_id: u64,
        wl_input: &str,
    ) -> anyhow::Result<Vec<String>> {
        let (expr, _messages) = self.eval_return_expr(eval_id, wl_input)?;
        parse_string_list(&expr).context("parse string list")
    }

    fn eval_return_expr(
        &mut self,
        eval_id: u64,
        wl_input: &str,
    ) -> anyhow::Result<(Expr, Vec<String>)> {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(self.config.eval_timeout_ms))
            .expect("deadline overflow");

        let expr = Expr::normal(
            Symbol::new("System`ToExpression"),
            vec![Expr::string(wl_input.to_string())],
        );

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
                    let ret = self
                        .link
                        .get_expr_with_resolver(&mut resolve_symbol_lossy)
                        .map_err(|e| anyhow!("{e:?}"))?;
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                    debug!(eval_id, kind = ?ret.kind(), "received RETURNPKT");
                    return Ok((ret, messages));
                }
                sys::TEXTPKT | sys::RETURNTEXTPKT => {
                    if let Ok(s) = self.link.get_string() {
                        messages.push(s);
                    }
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
                sys::MESSAGEPKT => {
                    // Avoid parsing MESSAGEPKT (it may contain relative symbols).
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
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
                other => {
                    debug!(packet = other, "unhandled packet type; skipping");
                    self.link.new_packet().map_err(|e| anyhow!("{e:?}"))?;
                }
            }
        }
    }
}

fn wl_escape_string(s: &str) -> String {
    // Minimal escaping for embedding into a Wolfram Language string literal.
    s.replace('\\', "\\\\")
        .replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn resolve_symbol_lossy(sym: &str) -> Option<Symbol> {
    if sym.contains('`') {
        return Symbol::try_new(sym);
    }
    if sym.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        Symbol::try_new(&format!("System`{sym}"))
    } else {
        Symbol::try_new(&format!("Global`{sym}"))
    }
}

fn parse_eval_tuple3_strings(expr: &Expr) -> anyhow::Result<(String, String, Vec<String>)> {
    let items = parse_list_items(expr).context("expected List[...]")?;
    if items.len() != 3 {
        return Err(anyhow!("expected 3-item list, got {}", items.len()));
    }
    let out = as_string(&items[0]).context("out string")?;
    let raw = as_string(&items[1]).context("raw string")?;
    let msgs_expr = &items[2];
    let msgs_items = parse_list_items(msgs_expr).unwrap_or_default();
    let mut msgs = Vec::new();
    for m in msgs_items {
        if let Ok(s) = as_string(&m) {
            msgs.push(s);
        }
    }
    Ok((out, raw, msgs))
}

fn parse_string_list(expr: &Expr) -> anyhow::Result<Vec<String>> {
    let items = parse_list_items(expr).context("expected List[...]")?;
    items
        .iter()
        .map(|e| as_string(e).context("list element"))
        .collect()
}

fn parse_list_items(expr: &Expr) -> anyhow::Result<Vec<Expr>> {
    match expr.kind() {
        ExprKind::Normal(n) => {
            let head = n.head();
            let is_list = matches!(
                head.kind(),
                ExprKind::Symbol(s) if s.as_str() == "System`List"
            );
            if !is_list {
                return Err(anyhow!("expected List head, got {head:?}"));
            }
            Ok(n.elements().to_vec())
        }
        _ => Err(anyhow!("expected list expr, got {:?}", expr.kind())),
    }
}

fn as_string(expr: &Expr) -> anyhow::Result<String> {
    match expr.kind() {
        ExprKind::String(s) => Ok(s.as_str().to_string()),
        _ => Err(anyhow!("expected string expr, got {:?}", expr.kind())),
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
