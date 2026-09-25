//! Harmless local workloads for demonstrating supervision and scheduling.
use std::{io::Write, time::Duration};
fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "heartbeat".into());
    match mode.as_str() {
        "job" => println!("Demo job finished successfully. No files were changed."),
        "fail" => {
            eprintln!("Demo failure: deliberately exiting with code 7.");
            std::process::exit(7);
        }
        _ => {
            let mut count = 0;
            loop {
                count += 1;
                println!(
                    "{}  heartbeat {count}  ·  worker is available",
                    chrono::Utc::now().format("%H:%M:%S UTC")
                );
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}
