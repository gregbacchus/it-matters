//! Running the PipeWire command line tools, with a hard timeout.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

/// How long a tool may run. It must stay **below the plugin's `timeout_ms`** (2000 ms by default):
/// otherwise a controller is told "failure" before the plugin gives up on a hung `wpctl`.
pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);

/// Runs a program and returns its standard output.
#[allow(async_fn_in_trait)]
pub trait Exec {
    async fn run(&self, program: &str, args: &[String]) -> Result<String, String>;
}

/// Runs real programs: no standard input, `LC_ALL=C` (so numbers are `0.58`, never `0,58`), killed if
/// it exceeds the timeout.
pub struct RealExec {
    pub timeout: Duration,
}

impl Default for RealExec {
    fn default() -> Self {
        Self { timeout: COMMAND_TIMEOUT }
    }
}

impl Exec for RealExec {
    async fn run(&self, program: &str, args: &[String]) -> Result<String, String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| format!("`{program}` did not finish within {} ms", self.timeout.as_millis()))?
            .map_err(|e| format!("cannot run `{program}`: {e} (is it installed?)"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("`{program}` failed ({}): {}", out.status, err.trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn runs_with_the_c_locale_and_returns_stdout() {
        let out = RealExec::default().run("sh", &args(&["-c", "echo \"$LC_ALL\""])).await.unwrap();
        assert_eq!(out.trim(), "C");
    }

    #[tokio::test]
    async fn a_failing_program_reports_its_error_output() {
        let e = RealExec::default().run("sh", &args(&["-c", "echo boom >&2; exit 3"])).await.unwrap_err();
        assert!(e.contains("boom") && e.contains("failed"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_program_is_a_clear_error() {
        let e = RealExec::default().run("definitely-not-installed", &[]).await.unwrap_err();
        assert!(e.contains("definitely-not-installed") && e.contains("installed"), "{e}");
    }

    #[tokio::test]
    async fn a_hung_program_is_killed_at_the_timeout() {
        let exec = RealExec { timeout: Duration::from_millis(100) };
        let t = std::time::Instant::now();
        let e = exec.run("sleep", &args(&["30"])).await.unwrap_err();
        assert!(e.contains("did not finish"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_timeout_is_below_the_default_set_timeout() {
        assert!(COMMAND_TIMEOUT < Duration::from_millis(2000));
    }
}
