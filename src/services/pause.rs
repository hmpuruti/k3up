use anyhow::{Result, anyhow};

/// Stops and starts workload services. A trait so the bookkeeping in `while_stopped` can be
/// tested without the service manager.
pub trait Control {
    fn stop(&mut self, name: &str) -> Result<()>;
    fn start(&mut self, name: &str) -> Result<()>;
}

/// Stops each of `running` in turn, runs `work`, then starts every workload it stopped again,
/// even when a stop or `work` failed. The error names every failure, starting with the first.
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
            Ok(()) => {
                lines.push(format!("Stopped {name}"));
                stopped.push(name);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    #[derive(Default)]
    struct Fake {
        stop_fails: Vec<&'static str>,
        start_fails: Vec<&'static str>,
        calls: Vec<String>,
    }

    impl Control for Fake {
        fn stop(&mut self, name: &str) -> Result<()> {
            self.calls.push(format!("stop {name}"));
            if self.stop_fails.contains(&name) {
                bail!("timed out");
            }
            Ok(())
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
