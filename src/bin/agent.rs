use clap::Parser;
use k3up::{platform, server};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "K3 Up background agent")]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Entry point used by Windows Service Control Manager.
    #[arg(long)]
    windows_service: bool,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let data = args.data_dir.unwrap_or_else(platform::default_data_dir);
    if args.windows_service {
        #[cfg(windows)]
        {
            return k3up::windows_host::run(data);
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("Windows service mode requires Windows");
        }
    }
    // One thread is plenty: requests are handled one at a time and the sampler has its own.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(server::run(data, server::signal()))
}
