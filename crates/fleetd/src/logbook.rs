//! The logbook — fleetd's log: one `tracing` line per event (local time, level, module, message),
//! always to stderr and, for the daemon, to `.fleet/fleetd.log` too (what the dashboard tails).

use anyhow::Result;
use std::path::Path;
use tracing_subscriber::fmt::{self, time::OffsetTime};
use tracing_subscriber::prelude::*;

/// Install the subscriber. `file`: the daemon's log — the previous run's is rotated to `<file>.1`,
/// and a panic on any thread is logged before it unwinds. Call before spawning threads (the local
/// UTC offset can only be read soundly while single-threaded).
pub fn init(file: Option<&Path>) -> Result<()> {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let clock = || OffsetTime::new(offset, time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"));
    let to_file = match file {
        Some(path) => {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
            let f = std::fs::File::create(path)?;
            Some(fmt::layer().with_timer(clock()).with_ansi(false).with_writer(std::sync::Mutex::new(f)))
        }
        None => None,
    };
    tracing_subscriber::registry()
        .with(fmt::layer().with_timer(clock()).with_writer(std::io::stderr))
        .with(to_file)
        .init();
    if file.is_some() {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!("panic: {info}");
            default(info);
        }));
    }
    Ok(())
}
