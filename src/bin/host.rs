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
    // Workload names may start with a hyphen.
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    workload: String,
    /// The definition's instance, recorded in the workload's state.
    #[arg(long, value_name = "ID")]
    instance: Option<String>,
}

fn main() {
    let args = Args::parse();
    #[cfg(windows)]
    if let Err(error) = k3up::services::host::run(args.data_dir, args.workload, args.instance) {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        let _ = (args.data_dir, args.workload, args.instance);
        eprintln!("k3up-host runs only on Windows");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workload_names_may_start_with_a_hyphen() {
        let args = Args::try_parse_from(["k3up-host", "--data-dir", "/d", "--workload", "-worker"])
            .unwrap();
        assert_eq!(args.workload, "-worker");
    }
}
