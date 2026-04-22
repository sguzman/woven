use anyhow::Context;
use tracing::{info, instrument};

#[instrument]
fn main() -> anyhow::Result<()> {
    let config = woven::config::load().context("load config")?;
    let _guards = woven::logging::init(&config).context("init logging")?;

    info!(mode = %config.mode.as_str(), "woven starting");

    // WSTP is obligatory: fail fast at startup if the kernel cannot be discovered
    // and connected.
    let kernel = woven::kernel::KernelSession::new(&config.kernel)
        .context("connect to Wolfram Kernel via WSTP")?;

    let native_options = eframe::NativeOptions::default();

    eframe::run_native(
        "Woven",
        native_options,
        Box::new(move |cc| Ok(Box::new(woven::app::WovenApp::new(cc, config.clone(), kernel)))),
    )
    .context("run eframe app")?;

    Ok(())
}
