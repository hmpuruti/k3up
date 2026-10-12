use anyhow::{Result, anyhow, bail};

/// Stops and starts workload services. A trait so the bookkeeping in `while_stopped` can be
/// tested without the service manager.
pub trait Control {
    /// True when this call stopped a running workload, false when it had already stopped.
    fn stop(&mut self, name: &str) -> Result<bool>;
    fn start(&mut self, name: &str) -> Result<()>;
}

/// Stopping a job would cut its work short, and starting it again would repeat what it did.
pub fn refuse_running_jobs(jobs: &[String]) -> Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    bail!(
        "These jobs are running: {}. Wait for them to finish, or stop them, then run `k3up services enable` again",
        jobs.join(", ")
    )
}

/// Stops each of `running` in turn, runs `work`, then starts again every workload this call
/// stopped, even when a stop or `work` failed. One that had already stopped stays stopped.
/// The error names every failure, starting with the first.
pub fn while_stopped(
    control: &mut impl Control,
    running: &[String],
    work: impl FnOnce() -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let mut lines = vec![];
    let mut stopped = vec![];
    let mut problems = vec![];
    for name in running {
        match control.stop(name) {
            Ok(true) => {
                lines.push(format!("Stopped {name}"));
                stopped.push(name);
            }
            Ok(false) => {}
            Err(error) => {
                problems.push(format!("{name} did not stop: {error:#}"));
                break;
            }
        }
    }
    if problems.is_empty() {
        match work() {
            Ok(done) => lines.extend(done),
            Err(error) => problems.push(format!("{error:#}")),
        }
    }
    for name in stopped {
        match control.start(name) {
            Ok(()) => lines.push(format!("Started {name}")),
            Err(error) => problems.push(format!("{name} did not start again: {error:#}")),
        }
    }
    if problems.is_empty() {
        Ok(lines)
    } else {
        Err(anyhow!(problems.join("\n")))
    }
}

/// Turns services mode on: stops the running services, installs the programs, repairs the
/// data folders, and only then starts what it stopped, since a host started before the
/// repair would refuse the folders. A failed install skips the repair. Every stopped workload
/// is started again whatever failed.
pub fn install_then_repair(
    control: &mut impl Control,
    running: &[String],
    install: impl FnOnce() -> Result<Vec<String>>,
    repair: impl FnOnce() -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    while_stopped(control, running, || {
        let mut lines = install()?;
        lines.extend(repair()?);
        Ok(lines)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    #[derive(Default)]
    struct Fake {
        stop_fails: Vec<&'static str>,
        start_fails: Vec<&'static str>,
        already_stopped: Vec<&'static str>,
        calls: Vec<String>,
    }

    impl Control for Fake {
        fn stop(&mut self, name: &str) -> Result<bool> {
            self.calls.push(format!("stop {name}"));
            if self.stop_fails.contains(&name) {
                bail!("timed out");
            }
            Ok(!self.already_stopped.contains(&name))
        }

        fn start(&mut self, name: &str) -> Result<()> {
            self.calls.push(format!("start {name}"));
            if self.start_fails.contains(&name) {
                bail!("access denied");
            }
            Ok(())
        }
    }

    fn running() -> Vec<String> {
        ["a", "b", "c"].map(String::from).to_vec()
    }

    #[test]
    fn everything_stopped_is_started_after_the_work() {
        let mut fake = Fake::default();
        let lines = while_stopped(&mut fake, &running(), || Ok(vec!["Copied".into()])).unwrap();
        assert_eq!(
            lines,
            [
                "Stopped a",
                "Stopped b",
                "Stopped c",
                "Copied",
                "Started a",
                "Started b",
                "Started c"
            ]
        );
    }

    #[test]
    fn a_workload_that_had_already_stopped_is_not_started() {
        let mut fake = Fake {
            already_stopped: vec!["b"],
            ..Default::default()
        };
        let lines = while_stopped(&mut fake, &running(), || Ok(vec!["Copied".into()])).unwrap();
        assert_eq!(
            fake.calls,
            ["stop a", "stop b", "stop c", "start a", "start c"]
        );
        assert_eq!(
            lines,
            ["Stopped a", "Stopped c", "Copied", "Started a", "Started c"]
        );
    }

    #[test]
    fn running_jobs_are_named() {
        refuse_running_jobs(&[]).unwrap();
        assert_eq!(
            refuse_running_jobs(&["backup".into(), "report".into()])
                .unwrap_err()
                .to_string(),
            "These jobs are running: backup, report. Wait for them to finish, or stop them, then run `k3up services enable` again"
        );
    }

    #[test]
    fn a_failed_stop_skips_the_work_and_starts_what_was_stopped() {
        let mut fake = Fake {
            stop_fails: vec!["b"],
            ..Default::default()
        };
        let mut worked = false;
        let error = while_stopped(&mut fake, &running(), || {
            worked = true;
            Ok(vec![])
        })
        .unwrap_err();
        assert!(!worked);
        assert_eq!(fake.calls, ["stop a", "stop b", "start a"]);
        assert_eq!(error.to_string(), "b did not stop: timed out");
    }

    #[test]
    fn a_failed_work_still_starts_everything() {
        let mut fake = Fake::default();
        let error = while_stopped(&mut fake, &running(), || bail!("Copy failed")).unwrap_err();
        assert_eq!(
            fake.calls,
            [
                "stop a", "stop b", "stop c", "start a", "start b", "start c"
            ]
        );
        assert_eq!(error.to_string(), "Copy failed");
    }

    #[test]
    fn every_start_is_attempted_and_each_failure_is_named() {
        let mut fake = Fake {
            start_fails: vec!["a", "c"],
            ..Default::default()
        };
        let error = while_stopped(&mut fake, &running(), || Ok(vec![])).unwrap_err();
        assert_eq!(
            fake.calls,
            [
                "stop a", "stop b", "stop c", "start a", "start b", "start c"
            ]
        );
        assert_eq!(
            error.to_string(),
            "a did not start again: access denied\nc did not start again: access denied"
        );
    }

    /// Records stops and starts in a log it shares with the steps between them.
    struct Recorder<'a>(&'a std::cell::RefCell<Vec<String>>);

    impl Control for Recorder<'_> {
        fn stop(&mut self, name: &str) -> Result<bool> {
            self.0.borrow_mut().push(format!("stop {name}"));
            Ok(true)
        }

        fn start(&mut self, name: &str) -> Result<()> {
            self.0.borrow_mut().push(format!("start {name}"));
            Ok(())
        }
    }

    #[test]
    fn workloads_start_again_only_after_the_repair() {
        for repair_fails in [false, true] {
            let log = std::cell::RefCell::new(vec![]);
            let result = install_then_repair(
                &mut Recorder(&log),
                &running()[..2],
                || {
                    log.borrow_mut().push("install".into());
                    Ok(vec![])
                },
                || {
                    log.borrow_mut().push("repair".into());
                    if repair_fails {
                        bail!("repair failed");
                    }
                    Ok(vec![])
                },
            );
            assert_eq!(
                *log.borrow(),
                [
                    "stop a", "stop b", "install", "repair", "start a", "start b"
                ]
            );
            assert_eq!(
                result.err().map(|error| error.to_string()),
                repair_fails.then(|| "repair failed".to_string())
            );
        }
    }

    #[test]
    fn a_failed_install_skips_the_repair_and_still_starts_everything() {
        let log = std::cell::RefCell::new(vec![]);
        let error = install_then_repair(
            &mut Recorder(&log),
            &running()[..1],
            || bail!("copy failed"),
            || {
                log.borrow_mut().push("repair".into());
                Ok(vec![])
            },
        )
        .unwrap_err();
        assert_eq!(*log.borrow(), ["stop a", "start a"]);
        assert_eq!(error.to_string(), "copy failed");
    }

    #[test]
    fn the_original_error_comes_before_failed_starts() {
        let mut fake = Fake {
            start_fails: vec!["a"],
            ..Default::default()
        };
        let error = while_stopped(&mut fake, &running(), || bail!("Copy failed")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Copy failed\na did not start again: access denied"
        );
    }
}
