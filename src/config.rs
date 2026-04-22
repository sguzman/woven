use std::{env, path::PathBuf};

use anyhow::{Context, anyhow};
use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dev,
    Prod,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dev => "dev",
            Mode::Prod => "prod",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub mode: Mode,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub kernel: KernelConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub plot: PlotConfig,
}

impl AppConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.kernel.start_timeout_ms == 0 {
            return Err(anyhow!("kernel.start_timeout_ms must be > 0"));
        }
        if self.kernel.eval_timeout_ms == 0 {
            return Err(anyhow!("kernel.eval_timeout_ms must be > 0"));
        }
        if self.ui.font_scale <= 0.0 {
            return Err(anyhow!("ui.font_scale must be > 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub file: LoggingFileConfig,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: LoggingFileConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingFileConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default = "default_log_path")]
    pub path: String,
    #[serde(default = "default_log_rolling")]
    pub rolling: String,
}

fn default_log_path() -> String {
    "logs/woven.log".to_string()
}

fn default_log_rolling() -> String {
    "daily".to_string()
}

impl Default for LoggingFileConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            path: default_log_path(),
            rolling: default_log_rolling(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KernelConfig {
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_start_timeout_ms")]
    pub start_timeout_ms: u64,
    #[serde(default = "default_eval_timeout_ms")]
    pub eval_timeout_ms: u64,
}

fn default_start_timeout_ms() -> u64 {
    20_000
}

fn default_eval_timeout_ms() -> u64 {
    60_000
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            start_timeout_ms: default_start_timeout_ms(),
            eval_timeout_ms: default_eval_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    #[serde(default = "default_notebook_path")]
    pub notebook_path: String,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default = "default_autosave_enabled")]
    pub autosave_enabled: bool,
    #[serde(default = "default_autosave_interval_ms")]
    pub autosave_interval_ms: u64,
    #[serde(default = "default_nav_width")]
    pub nav_width: f32,
    #[serde(default = "default_inspector_width")]
    pub inspector_width: f32,
}

fn default_font_scale() -> f32 {
    1.0
}

fn default_notebook_path() -> String {
    "notebooks/default.json".to_string()
}

fn default_autosave_enabled() -> bool {
    true
}

fn default_autosave_interval_ms() -> u64 {
    10_000
}

fn default_nav_width() -> f32 {
    280.0
}

fn default_inspector_width() -> f32 {
    360.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Dark
    }
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font_scale: default_font_scale(),
            notebook_path: default_notebook_path(),
            theme: Theme::default(),
            autosave_enabled: default_autosave_enabled(),
            autosave_interval_ms: default_autosave_interval_ms(),
            nav_width: default_nav_width(),
            inspector_width: default_inspector_width(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlotConfig {
    #[serde(default = "default_plot_placeholder_enabled")]
    pub placeholder_enabled: bool,
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
}

fn default_plot_placeholder_enabled() -> bool {
    true
}

fn default_max_output_chars() -> usize {
    200_000
}

fn default_max_messages() -> usize {
    1_000
}

impl Default for PlotConfig {
    fn default() -> Self {
        Self {
            placeholder_enabled: default_plot_placeholder_enabled(),
            max_output_chars: default_max_output_chars(),
            max_messages: default_max_messages(),
        }
    }
}

pub fn repo_root() -> anyhow::Result<PathBuf> {
    // Keep it simple: assume CWD is repo root for now.
    // (If that changes, we can anchor off `CARGO_MANIFEST_DIR` at compile-time.)
    env::current_dir().context("get current dir")
}

pub fn load() -> anyhow::Result<AppConfig> {
    let root = repo_root()?;
    let base_path = root.join("config/woven.toml");
    let local_path = root.join("config/woven.local.toml");

    if !base_path.is_file() {
        return Err(anyhow!(
            "missing required config file: {}",
            base_path.display()
        ));
    }

    let mut builder = config::Config::builder()
        .add_source(config::File::from(base_path))
        .add_source(config::File::from(local_path).required(false))
        .add_source(
            config::Environment::with_prefix("WOVEN")
                .separator("__")
                .try_parsing(true),
        );

    // Helpful in tests / debugging: allow overrides via a single env var pointing
    // to an additional TOML file.
    if let Ok(extra_path) = env::var("WOVEN_CONFIG") {
        builder = builder.add_source(config::File::from(PathBuf::from(extra_path)));
    }

    let cfg = builder
        .build()
        .context("build layered config")?
        .try_deserialize::<AppConfig>()
        .context("deserialize config")?;

    cfg.validate().context("validate config")?;
    debug!(mode = %cfg.mode.as_str(), "config loaded");
    Ok(cfg)
}

pub fn persist_local_theme(theme: Theme) -> anyhow::Result<()> {
    let root = repo_root()?;
    let local_path = root.join("config/woven.local.toml");

    let mut doc: toml::Value = if local_path.is_file() {
        let s = std::fs::read_to_string(&local_path)
            .with_context(|| format!("read {}", local_path.display()))?;
        toml::from_str(&s).with_context(|| format!("parse {}", local_path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow!("config/woven.local.toml must be a TOML table at the root"))?;

    let ui = table
        .entry("ui")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let ui_table = ui
        .as_table_mut()
        .ok_or_else(|| anyhow!("[ui] must be a table"))?;
    ui_table.insert(
        "theme".to_string(),
        toml::Value::String(theme.as_str().to_string()),
    );

    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let s = toml::to_string_pretty(&doc).context("serialize woven.local.toml")?;
    std::fs::write(&local_path, s).with_context(|| format!("write {}", local_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn validates_font_scale() {
        let cfg = AppConfig {
            mode: Mode::Dev,
            logging: LoggingConfig::default(),
            kernel: KernelConfig::default(),
            ui: UiConfig {
                font_scale: 0.0,
                notebook_path: default_notebook_path(),
                autosave_enabled: default_autosave_enabled(),
                autosave_interval_ms: default_autosave_interval_ms(),
                theme: Theme::default(),
                nav_width: default_nav_width(),
                inspector_width: default_inspector_width(),
            },
            plot: PlotConfig::default(),
        };

        assert!(cfg.validate().is_err());
    }

    #[test]
    fn precedence_local_over_base() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("config")).unwrap();

        fs::write(
            root.join("config/woven.toml"),
            r#"
mode = "dev"
[ui]
font_scale = 1.0
"#,
        )
        .unwrap();
        fs::write(
            root.join("config/woven.local.toml"),
            r#"
[ui]
font_scale = 2.0
"#,
        )
        .unwrap();

        let cfg = config::Config::builder()
            .add_source(config::File::from(root.join("config/woven.toml")))
            .add_source(config::File::from(root.join("config/woven.local.toml")).required(false))
            .build()
            .unwrap()
            .try_deserialize::<AppConfig>()
            .unwrap();

        assert_eq!(cfg.ui.font_scale, 2.0);
    }

    #[test]
    fn persist_local_theme_writes_file_and_preserves_existing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("config")).unwrap();

        // Seed with an existing override.
        fs::write(
            root.join("config/woven.local.toml"),
            r#"
[ui]
font_scale = 1.5
"#,
        )
        .unwrap();

        let old = env::current_dir().unwrap();
        env::set_current_dir(root).unwrap();

        let res = persist_local_theme(Theme::Light);

        env::set_current_dir(old).unwrap();

        res.unwrap();

        let s = fs::read_to_string(root.join("config/woven.local.toml")).unwrap();
        let v: toml::Value = toml::from_str(&s).unwrap();

        let ui = v.get("ui").and_then(|v| v.as_table()).unwrap();
        assert_eq!(ui.get("theme").and_then(|v| v.as_str()).unwrap(), "light");
        assert_eq!(
            ui.get("font_scale").and_then(|v| v.as_float()).unwrap(),
            1.5
        );
    }
}
