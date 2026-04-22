use anyhow::Context;
use tracing::{info, instrument};

#[instrument]
fn main() -> anyhow::Result<()> {
    let config = woven::config::load().context("load config")?;
    let _guards = woven::logging::init(&config).context("init logging")?;

    info!(mode = %config.mode.as_str(), "woven starting");

    let native_options = eframe::NativeOptions::default();

    eframe::run_native(
        "Woven",
        native_options,
        Box::new(|cc| Ok(Box::new(woven::app::WovenApp::new(cc, config.clone())))),
    )
    .context("run eframe app")?;

    Ok(())
}
