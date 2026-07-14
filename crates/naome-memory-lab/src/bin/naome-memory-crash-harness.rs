#![forbid(unsafe_code)]

use std::error::Error;

use naome_memory_lab::{crash_child_requested, run_crash_campaign, run_crash_child};

fn main() -> Result<(), Box<dyn Error>> {
    if crash_child_requested() {
        run_crash_child()?;
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let evidence = run_crash_campaign(executable)?;
    serde_json::to_writer(std::io::stdout().lock(), &evidence)?;
    Ok(())
}
