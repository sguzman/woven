use std::{fs, path::PathBuf};

use anyhow::Context;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{
    fmt,
    layer::SubscriberExt as _,
    util::SubscriberInitExt as _,
    EnvFilter,
    Layer as _,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::config::{AppConfig, Mode};

#[derive(Debug)]
pub struct LoggingGuards {
    _file_guard: Option<WorkerGuard>,
}

pub fn init(config: &AppConfig) -> anyhow::Result<LoggingGuards> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.logging.level))
        .context("build EnvFilter")?;

    let stdout_layer = fmt::layer()
        .pretty()
        .with_target(true)
        .with_line_number(true)
        .with_file(true)
        .with_thread_names(true)
        .with_timer(fmt::time::UtcTime::rfc_3339())
        .with_filter(env_filter.clone());

    let mut file_guard = None;

    if should_enable_file_logging(config) {
        let (writer, guard, file_path) = build_file_writer(config)?;
        file_guard = Some(guard);

        let file_layer = fmt::layer()
            .json()
            .with_target(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_timer(fmt::time::UtcTime::rfc_3339())
            .with_writer(writer)
            .with_filter(env_filter);

        tracing::info!(path = %file_path.display(), "file logging enabled");

        tracing_subscriber::registry()
            .with(stdout_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry().with(stdout_layer).init();
    }

    Ok(LoggingGuards {
        _file_guard: file_guard,
    })
}

fn should_enable_file_logging(config: &AppConfig) -> bool {
    match config.logging.file.enabled {
        Some(enabled) => enabled,
        None => config.mode == Mode::Dev,
    }
}

fn build_file_writer(config: &AppConfig) -> anyhow::Result<(NonBlocking, WorkerGuard, PathBuf)> {
    let path = PathBuf::from(&config.logging.file.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create logs dir")?;
    }

    let rolling = config.logging.file.rolling.as_str();
    if rolling != "run" {
        anyhow::bail!("unsupported logging.file.rolling value: {rolling} (expected 'run')");
    }

    let dir = path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs"));

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("woven")
        .to_string();

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("log")
        .to_string();

    let ts = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("format timestamp")?
        .replace(':', "")
        .replace('.', "")
        .replace('Z', "Z");
    let pid = std::process::id();

    let file_path = dir.join(format!("{stem}-{ts}-{pid}.{ext}"));

    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("create log file {}", file_path.display()))?;

    let (writer, guard) = tracing_appender::non_blocking(file);
    Ok((writer, guard, file_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, LoggingConfig, Mode};

    #[test]
    fn file_logging_enabled_by_default_in_dev() {
        let cfg = AppConfig {
            mode: Mode::Dev,
            logging: LoggingConfig::default(),
            kernel: Default::default(),
            ui: Default::default(),
            plot: Default::default(),
        };

        assert!(should_enable_file_logging(&cfg));
    }

    #[test]
    fn file_logging_disabled_by_default_in_prod() {
        let cfg = AppConfig {
            mode: Mode::Prod,
            logging: LoggingConfig::default(),
            kernel: Default::default(),
            ui: Default::default(),
            plot: Default::default(),
        };

        assert!(!should_enable_file_logging(&cfg));
    }
}
