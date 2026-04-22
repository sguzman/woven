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
        .with_filter(env_filter.clone());

    let mut file_guard = None;

    if should_enable_file_logging(config) {
        let (writer, guard) = build_file_writer(config)?;
        file_guard = Some(guard);

        let file_layer = fmt::layer()
            .json()
            .with_target(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(writer)
            .with_filter(env_filter);

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

fn build_file_writer(config: &AppConfig) -> anyhow::Result<(NonBlocking, WorkerGuard)> {
    let path = PathBuf::from(&config.logging.file.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create logs dir")?;
    }

    let rolling = config.logging.file.rolling.as_str();
    let (writer, guard) = match rolling {
        "daily" => {
            let dir = path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("woven.log");
            let appender = tracing_appender::rolling::daily(dir, file_name);
            tracing_appender::non_blocking(appender)
        },
        "never" => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("open log file {}", path.display()))?;
            tracing_appender::non_blocking(file)
        },
        other => {
            anyhow::bail!(
                "unsupported logging.file.rolling value: {other} (expected 'daily' or 'never')"
            );
        },
    };
    Ok((writer, guard))
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
