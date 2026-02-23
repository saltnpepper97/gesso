// Author: Dustin Pilgrim
// License: MIT

use anyhow::Result;

fn main() -> Result<()> {
    gesso::daemon::run_daemon()
}
