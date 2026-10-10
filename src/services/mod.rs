//! Services mode: on Windows, each workload is its own Windows service, run by k3up-host and
//! managed through the service manager, with no central agent.
pub mod files;
pub mod status;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod scm;
#[cfg(windows)]
pub mod setup;

#[cfg(windows)]
pub use backend::{Backend, DEFAULT_PREFIX, Settings};

use std::path::Path;

/// Whether `data` holds the services mode marker. Always false outside Windows.
pub fn active(data: &Path) -> bool {
    cfg!(windows) && data.join(files::MARKER).is_file()
}
