// Author: Dustin Pilgrim
// License: MIT

use std::path::Path;

use eventline::runtime;
use eventline::LogLevel;

pub fn init(verbose: bool, log_path: &Path) -> std::io::Result<()> {
    futures::executor::block_on(eventline::runtime::init());

    runtime::enable_file_output(log_path).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("eventline file output: {e}"))
    })?;

    if verbose {
        runtime::set_log_level(LogLevel::Debug);
        runtime::enable_console_output(true);
    } else {
        runtime::set_log_level(LogLevel::Info);
        runtime::enable_console_output(false);
    }

    Ok(())
}
