pub const TEMPLATE: &str = r#"# K3 Up manifest. Check it with `k3up validate FILE`, then `k3up apply FILE`.
# Every field is shown. Fields marked "default" can be left out.
# Paths must be absolute. Programs run directly, never through a shell.

version = 1

# A service is kept running. Its restart policy decides what happens when it exits.
[[workloads]]
name = "web"                               # 1 to 64 letters, digits, hyphens or underscores
description = "Web server"                 # default ""
executable = "/usr/local/bin/node"         # absolute path
args = ["server.js", "--port", "8080"]     # default []
working_directory = "/srv/web"             # absolute path
kind = "service"                           # service | job; default service
start_at_boot = true                       # default false
depends_on = ["migrate"]                   # workloads that must be ready first; default []
restart = "on_failure"                     # never | on_failure | always; default on_failure
max_restarts = 5                           # 0 to 100; default 5
restart_delay_secs = 2                     # 1 to 300, doubles after each restart; default 2
stop_timeout_secs = 5                      # 1 to 30; default 5
# run_timeout_secs = 3600                  # optional; the run ends with exit code 124 after it
readiness_tcp = "127.0.0.1:8080"           # optional, services only; IP:port that must accept connections
startup_timeout_secs = 30                  # 1 to 300, time allowed for readiness_tcp; default 30

[workloads.environment]                    # optional; any number of KEY = "value" lines
NODE_ENV = "production"

[workloads.schedule]                       # optional; use either cron or every_secs
cron = "0 0 4 * * *"                       # 6 or 7 fields starting with seconds; optional 7th is the year
timezone = "UTC"                           # IANA name; default UTC
action = "restart"                         # start | restart; default start; jobs allow start only
missed = "skip"                            # skip | run_once; default skip

# A job runs to completion each time it is started or scheduled. It is never restarted.
[[workloads]]
name = "migrate"
description = "Database migration"
executable = "/usr/local/bin/node"
args = ["migrate.js"]
working_directory = "/srv/web"
kind = "job"
run_timeout_secs = 600

[workloads.schedule]
every_secs = 86400                         # 1 to 31536000
timezone = "UTC"
action = "start"
missed = "run_once"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use k3up::model::{Kind, Manifest, PathStyle, Restart, ScheduleAction};

    #[test]
    fn template_is_a_valid_manifest_showing_every_field() {
        let manifest: Manifest = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(
            manifest.validate_for(PathStyle::Unix).unwrap(),
            ["migrate", "web"]
        );
        let web = &manifest.workloads[0];
        assert_eq!(web.kind, Kind::Service);
        assert_eq!(web.restart, Restart::OnFailure);
        assert_eq!(web.environment["NODE_ENV"], "production");
        assert_eq!(
            web.schedule.as_ref().unwrap().action,
            ScheduleAction::Restart
        );
        assert_eq!(manifest.workloads[1].run_timeout_secs, Some(600));
        for field in [
            "name",
            "description",
            "executable",
            "args",
            "working_directory",
            "kind",
            "start_at_boot",
            "depends_on",
            "restart",
            "max_restarts",
            "restart_delay_secs",
            "stop_timeout_secs",
            "run_timeout_secs",
            "readiness_tcp",
            "startup_timeout_secs",
            "environment",
            "schedule",
            "every_secs",
            "cron",
            "timezone",
            "action",
            "missed",
        ] {
            assert!(
                TEMPLATE.contains(&format!("{field} = "))
                    || TEMPLATE.contains(&format!("{field}]")),
                "{field}"
            );
        }
    }
}
