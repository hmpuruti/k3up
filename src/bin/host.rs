use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Runs one K3 Up workload as a Windows service. The service manager starts it; use `k3up services enable` and `k3up create` instead of running it yourself"
)]
struct Args {
    #[arg(long, value_name = "DIR")]
    data_dir: PathBuf,
    #[arg(long, value_name = "NAME")]
    workload: String,
}

fn main() {
    let args = Args::parse();
    #[cfg(windows)]
    if let Err(error) = k3up::services::host::run(args.data_dir, args.workload) {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        let _ = (args.data_dir, args.workload);
        eprintln!("k3up-host runs only on Windows");
        std::process::exit(2);
    }
}
