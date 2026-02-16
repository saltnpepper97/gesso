// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::path::Path;

/// Initialize eventline once.
/// We keep this local so daemon stays the only place that knows how runtime is bootstrapped.
pub fn init_eventline(log_path: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for eventline init")?;

    rt.block_on(async {
        eventline::runtime::init().await;
    });

    // Daemon policy:
    // - no console output
    // - full file logging (live + structured)
    eventline::runtime::enable_console_output(false);
    eventline::runtime::enable_console_color(false);
    eventline::runtime::enable_console_timestamp(false);
    eventline::runtime::enable_console_duration(true);

    // Single canonical log file (owned by gesso)
    eventline::runtime::enable_file_output(log_path)
        .with_context(|| format!("enable eventline file output: {}", log_path.display()))?;

    // Default verbosity (adjustable later)
    eventline::runtime::set_log_level(eventline::runtime::LogLevel::Info);

    Ok(())
}
