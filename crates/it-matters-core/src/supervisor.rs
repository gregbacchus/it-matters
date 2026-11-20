//! Starts and supervises the plugin processes listed with `run` in the configuration.
//!
//! * Each plugin runs in **its own process group**, with stdin closed and stdout/stderr piped to the
//!   log (always drained, with a cap per line, so a chatty plugin can neither block nor grow memory).
//! * A plugin that exits **for any reason, including a clean exit**, is restarted after a delay that
//!   doubles from [`Settings::initial_backoff`] up to [`Settings::max_backoff`] and resets after a run
//!   of at least [`Settings::stable_after`]. A command that cannot be started is retried the same way.
//! * After every exit the whole group is SIGKILLed, so helpers a crashed plugin left behind cannot
//!   pile up next to its replacement.
//! * On [`Supervisor::shutdown`] each group gets SIGTERM, then SIGKILL after [`Settings::grace`].
//! * If the daemon itself dies without shutting down (a crash, `kill -9`), the kernel sends SIGTERM to
//!   each plugin (`PR_SET_PDEATHSIG`). That reaches only the plugin's main process, not helpers it
//!   started, and it relies on the daemon running its supervisor on the main thread of a
//!   current-thread runtime: the signal fires when the *thread that started the child* exits.

use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

/// The longest output line forwarded; the rest of a longer line is dropped.
pub const MAX_LOG_LINE: usize = 8 * 1024;

/// A plugin process to supervise.
#[derive(Debug, Clone)]
pub struct PluginProcess {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    /// Added to the daemon's own environment.
    pub env: Vec<(String, String)>,
}

/// Timing of the restart policy and of shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// A run at least this long counts as healthy and resets the backoff.
    pub stable_after: Duration,
    /// How long a plugin gets between SIGTERM and SIGKILL at shutdown.
    pub grace: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            stable_after: Duration::from_secs(30),
            grace: Duration::from_secs(5),
        }
    }
}

/// The restart delay sequence.
#[derive(Debug, Clone)]
pub struct Backoff {
    settings: Settings,
    next: Duration,
}

impl Backoff {
    pub fn new(settings: Settings) -> Self {
        Self { next: settings.initial_backoff, settings }
    }

    /// The delay before the next start, given how long the last run lasted.
    pub fn next_delay(&mut self, ran: Duration) -> Duration {
        if ran >= self.settings.stable_after {
            self.next = self.settings.initial_backoff;
        }
        let delay = self.next;
        self.next = (self.next * 2).min(self.settings.max_backoff);
        delay
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What happened to a plugin process; delivered to the [`Sink`].
#[derive(Debug, Clone)]
pub enum Event {
    Started { id: String, pid: u32 },
    Output { id: String, stream: Stream, line: String },
    Exited { id: String, status: String, ran: Duration },
    SpawnFailed { id: String, error: String },
    Restarting { id: String, delay: Duration },
    Stopped { id: String },
}

/// Receives events. Called from several tasks, so it must not block.
pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// A sink that writes to the `log` crate (target `plugin`).
pub fn log_sink() -> Sink {
    Arc::new(|event| match event {
        Event::Started { id, pid } => log::info!(target: "plugin", "[{id}] started (pid {pid})"),
        Event::Output { id, stream: Stream::Stdout, line } => log::info!(target: "plugin", "[{id}] {line}"),
        Event::Output { id, stream: Stream::Stderr, line } => log::warn!(target: "plugin", "[{id}] {line}"),
        Event::Exited { id, status, ran } => {
            log::warn!(target: "plugin", "[{id}] exited ({status}) after {:.1}s", ran.as_secs_f64())
        }
        Event::SpawnFailed { id, error } => log::error!(target: "plugin", "[{id}] cannot start: {error}"),
        Event::Restarting { id, delay } => {
            log::info!(target: "plugin", "[{id}] restarting in {:.1}s", delay.as_secs_f64())
        }
        Event::Stopped { id } => log::info!(target: "plugin", "[{id}] stopped"),
    })
}

/// The running supervisor. Dropping it without calling [`Supervisor::shutdown`] leaves the plugins
/// running until the process exits (then the kernel signals them, see the module docs).
pub struct Supervisor {
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

/// Starts supervising `plugins`. Must be called on the runtime that will keep running them.
pub fn spawn(plugins: Vec<PluginProcess>, settings: Settings, sink: Sink) -> Supervisor {
    let (stop, stopped) = watch::channel(false);
    let tasks =
        plugins.into_iter().map(|p| tokio::spawn(run_plugin(p, settings, sink.clone(), stopped.clone()))).collect();
    Supervisor { stop, tasks }
}

impl Supervisor {
    /// Stops every plugin (SIGTERM, then SIGKILL after the grace period) and waits until all are gone.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

struct Running {
    child: tokio::process::Child,
    /// The process group id: the child's pid, recorded at spawn (it is unavailable after reaping).
    pgid: i32,
    readers: Vec<JoinHandle<()>>,
}

fn kill_group(pgid: i32, signal: i32) {
    // A negative pid addresses the whole process group. ESRCH (nothing left) is fine.
    // SAFETY: plain syscall with integer arguments.
    unsafe {
        libc::kill(-pgid, signal);
    }
}

fn start(spec: &PluginProcess, sink: &Sink) -> io::Result<Running> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: the closure only calls async-signal-safe functions (prctl, getppid).
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            // If the daemon died between fork and prctl, the signal will never come: give up.
            if libc::getppid() != parent {
                return Err(io::Error::other("the daemon exited while the plugin was starting"));
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pid = child.id().ok_or_else(|| io::Error::other("the plugin exited before it could be tracked"))?;
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(tokio::spawn(forward(out, Stream::Stdout, spec.id.clone(), sink.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(forward(err, Stream::Stderr, spec.id.clone(), sink.clone())));
    }
    sink(Event::Started { id: spec.id.clone(), pid });
    Ok(Running { child, pgid: pid as i32, readers })
}

/// Reads one line, keeping at most `cap` bytes of it. Returns `None` at end of input, else whether the
/// line was cut. The excess is discarded as it arrives, so memory use is bounded.
async fn read_capped_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
    cap: usize,
    out: &mut Vec<u8>,
) -> io::Result<Option<bool>> {
    out.clear();
    let mut cut = false;
    loop {
        let (used, done) = {
            let chunk = r.fill_buf().await?;
            if chunk.is_empty() {
                return Ok(if out.is_empty() && !cut { None } else { Some(cut) });
            }
            let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (i, true),
                None => (chunk.len(), false),
            };
            let room = cap.saturating_sub(out.len());
            out.extend_from_slice(&chunk[..take.min(room)]);
            cut |= take > room;
            (take + usize::from(done), done)
        };
        r.consume(used);
        if done {
            return Ok(Some(cut));
        }
    }
}

async fn forward<R: tokio::io::AsyncRead + Unpin>(pipe: R, stream: Stream, id: String, sink: Sink) {
    let mut pipe = BufReader::new(pipe);
    let mut buf = Vec::new();
    while let Ok(Some(cut)) = read_capped_line(&mut pipe, MAX_LOG_LINE, &mut buf).await {
        let mut line = String::from_utf8_lossy(&buf).into_owned();
        if cut {
            line.push_str(" [line cut]");
        }
        sink(Event::Output { id: id.clone(), stream, line });
    }
}

/// Waits (briefly) for the output readers: they end once every holder of the pipe is gone.
async fn drain(readers: Vec<JoinHandle<()>>) {
    for r in readers {
        // If something outside the group still holds the pipe open, stop waiting for it after a second.
        let _ = timeout(Duration::from_secs(1), r).await;
    }
}

async fn stop(mut run: Running, grace: Duration) {
    kill_group(run.pgid, libc::SIGTERM);
    if timeout(grace, run.child.wait()).await.is_err() {
        kill_group(run.pgid, libc::SIGKILL);
        let _ = run.child.wait().await;
    }
    // The main process is gone; make sure nothing it started survives.
    kill_group(run.pgid, libc::SIGKILL);
    drain(run.readers).await;
}

/// Resolves when shutdown was requested. (A helper so the watch guard is not held across an await.)
async fn shutdown_requested(stopped: &mut watch::Receiver<bool>) {
    let _ = stopped.wait_for(|s| *s).await;
}

async fn run_plugin(spec: PluginProcess, settings: Settings, sink: Sink, mut stopped: watch::Receiver<bool>) {
    let mut backoff = Backoff::new(settings);
    loop {
        if *stopped.borrow() {
            return;
        }
        let started = Instant::now();
        match start(&spec, &sink) {
            Err(e) => sink(Event::SpawnFailed { id: spec.id.clone(), error: e.to_string() }),
            Ok(mut run) => {
                tokio::select! {
                    status = run.child.wait() => {
                        let status = status.map_or_else(|e| format!("wait failed: {e}"), |s| s.to_string());
                        sink(Event::Exited { id: spec.id.clone(), status, ran: started.elapsed() });
                        kill_group(run.pgid, libc::SIGKILL);
                        drain(run.readers).await;
                    }
                    () = shutdown_requested(&mut stopped) => {
                        stop(run, settings.grace).await;
                        sink(Event::Stopped { id: spec.id.clone() });
                        return;
                    }
                }
            }
        }
        let delay = backoff.next_delay(started.elapsed());
        sink(Event::Restarting { id: spec.id.clone(), delay });
        tokio::select! {
            () = sleep(delay) => {}
            () = shutdown_requested(&mut stopped) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;

    /// Real processes and real (millisecond) time: virtual time would let backoffs and grace periods
    /// "elapse" while a child is still running.
    fn fast() -> Settings {
        Settings {
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(80),
            stable_after: Duration::from_secs(60),
            grace: Duration::from_millis(300),
        }
    }

    fn sh(id: &str, script: &str) -> PluginProcess {
        PluginProcess {
            id: id.into(),
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("IT_MATTERS_PLUGIN_ID".into(), id.into()), ("IT_MATTERS_SOCKET".into(), "/x/sock".into())],
        }
    }

    #[derive(Clone, Default)]
    struct Events(Arc<Mutex<Vec<Event>>>);

    impl Events {
        fn sink(&self) -> Sink {
            let me = self.clone();
            Arc::new(move |e| me.0.lock().unwrap().push(e))
        }
        fn all(&self) -> Vec<Event> {
            self.0.lock().unwrap().clone()
        }
        /// Waits until `pick` has returned at least `n` items.
        async fn wait_for<T>(&self, n: usize, pick: impl Fn(&Event) -> Option<T>) -> Vec<T> {
            for _ in 0..500 {
                let got: Vec<T> = self.all().iter().filter_map(&pick).collect();
                if got.len() >= n {
                    return got;
                }
                sleep(Duration::from_millis(10)).await;
            }
            panic!("timed out waiting for {n} events; got {:#?}", self.all());
        }
        async fn lines(&self, n: usize) -> Vec<(Stream, String)> {
            self.wait_for(n, |e| match e {
                Event::Output { stream, line, .. } => Some((*stream, line.clone())),
                _ => None,
            })
            .await
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("it-matters-sup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn read_pid(file: &Path) -> i32 {
        for _ in 0..500 {
            if let Some(n) = std::fs::read_to_string(file).ok().and_then(|t| t.trim().parse().ok()) {
                return n;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!("{} never appeared", file.display());
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn dead_within(pid: i32, ms: u64) -> bool {
        for _ in 0..(ms / 10) {
            if !alive(pid) {
                return true;
            }
            sleep(Duration::from_millis(10)).await;
        }
        !alive(pid)
    }

    async fn shut_down(s: Supervisor) {
        timeout(Duration::from_secs(5), s.shutdown()).await.expect("shutdown did not finish in time");
    }

    // ---- pure backoff ----

    #[test]
    fn the_backoff_doubles_up_to_the_cap() {
        let mut b = Backoff::new(Settings::default());
        let seq: Vec<u64> = (0..8).map(|_| b.next_delay(Duration::ZERO).as_secs()).collect();
        assert_eq!(seq, vec![1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn a_stable_run_resets_the_backoff() {
        let mut b = Backoff::new(Settings::default());
        for _ in 0..5 {
            b.next_delay(Duration::from_secs(1));
        }
        assert_eq!(b.next_delay(Duration::from_secs(29)), Duration::from_secs(32), "not stable yet");
        assert_eq!(b.next_delay(Duration::from_secs(30)), Duration::from_secs(1), "stable: back to the start");
        assert_eq!(b.next_delay(Duration::from_secs(1)), Duration::from_secs(2));
    }

    // ---- processes ----

    #[tokio::test]
    async fn the_plugin_gets_its_environment_and_its_output_is_captured() {
        let ev = Events::default();
        let s = spawn(
            vec![sh("demo", "echo \"$IT_MATTERS_PLUGIN_ID:$IT_MATTERS_SOCKET\"; echo oops >&2; sleep 30")],
            fast(),
            ev.sink(),
        );
        let lines = ev.lines(2).await;
        assert!(lines.contains(&(Stream::Stdout, "demo:/x/sock".into())), "{lines:?}");
        assert!(lines.contains(&(Stream::Stderr, "oops".into())), "{lines:?}");
        shut_down(s).await;
    }

    #[tokio::test]
    async fn stdin_is_closed() {
        let ev = Events::default();
        let s = spawn(vec![sh("demo", "if read x; then echo got; else echo eof; fi; sleep 30")], fast(), ev.sink());
        assert_eq!(ev.lines(1).await[0].1, "eof");
        shut_down(s).await;
    }

    #[tokio::test]
    async fn a_plugin_that_exits_is_restarted_with_growing_delays_even_after_a_clean_exit() {
        let ev = Events::default();
        let s = spawn(vec![sh("flaky", "exit 3"), sh("clean", "exit 0")], fast(), ev.sink());
        let delays = |id: &'static str| {
            let ev = ev.clone();
            async move {
                ev.wait_for(4, |e| match e {
                    Event::Restarting { id: i, delay } if i == id => Some(delay.as_millis()),
                    _ => None,
                })
                .await
            }
        };
        assert_eq!(delays("flaky").await[..4], [20, 40, 80, 80]);
        assert_eq!(delays("clean").await[..4], [20, 40, 80, 80], "exit code 0 restarts too");
        let exits = ev.all().into_iter().filter_map(|e| match e {
            Event::Exited { id, status, .. } if id == "flaky" => Some(status),
            _ => None,
        });
        assert!(exits.into_iter().all(|s| s.contains('3')), "the exit status is reported");
        shut_down(s).await;
    }

    #[tokio::test]
    async fn a_command_that_cannot_start_is_retried_and_never_fatal() {
        let ev = Events::default();
        let missing =
            PluginProcess { id: "ghost".into(), command: "/no/such/plugin".into(), args: vec![], env: vec![] };
        let s = spawn(vec![missing], fast(), ev.sink());
        let errors = ev
            .wait_for(3, |e| match e {
                Event::SpawnFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .await;
        assert!(errors[0].to_lowercase().contains("no such file"), "{errors:?}");
        shut_down(s).await;
    }

    #[tokio::test]
    async fn shutdown_kills_helpers_the_plugin_started() {
        let dir = scratch("group");
        let pidfile = dir.join("helper.pid");
        let ev = Events::default();
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.display());
        let s = spawn(vec![sh("demo", &script)], fast(), ev.sink());
        let helper = read_pid(&pidfile).await;
        assert!(alive(helper));
        shut_down(s).await;
        assert!(dead_within(helper, 1000).await, "the helper process survived shutdown");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_plugin_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let ev = Events::default();
        let s =
            spawn(vec![sh("stubborn", "trap '' TERM; echo ready; while true; do sleep 0.05; done")], fast(), ev.sink());
        ev.lines(1).await;
        let pid =
            ev.wait_for(1, |e| if let Event::Started { pid, .. } = e { Some(*pid as i32) } else { None }).await[0];
        let t = std::time::Instant::now();
        shut_down(s).await;
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(250), "it should have been given the grace period first");
        assert!(took < Duration::from_millis(1500), "SIGKILL must follow promptly after the grace period ({took:?})");
        assert!(dead_within(pid, 1000).await, "the plugin survived SIGKILL");
        assert!(ev.all().iter().any(|e| matches!(e, Event::Stopped { .. })));
    }

    #[tokio::test]
    async fn a_plugin_that_obeys_sigterm_stops_at_once() {
        let ev = Events::default();
        let s = spawn(vec![sh("polite", "echo ready; sleep 300")], fast(), ev.sink());
        ev.lines(1).await;
        let t = std::time::Instant::now();
        shut_down(s).await;
        assert!(t.elapsed() < Duration::from_millis(250), "no need to wait out the grace period");
    }

    #[tokio::test]
    async fn helpers_left_by_a_crashed_plugin_are_killed_before_the_restart() {
        let dir = scratch("straggler");
        let pidfile = dir.join("helper.pid");
        let ev = Events::default();
        let script = format!("sleep 300 & echo $! > {}; exit 1", pidfile.display());
        let s = spawn(
            vec![sh("crashy", &script)],
            Settings { initial_backoff: Duration::from_secs(30), ..fast() },
            ev.sink(),
        );
        let helper = read_pid(&pidfile).await;
        ev.wait_for(1, |e| matches!(e, Event::Exited { .. }).then_some(())).await;
        assert!(dead_within(helper, 1000).await, "the leftover helper must not outlive the plugin");
        shut_down(s).await;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn shutdown_does_not_wait_out_a_restart_delay() {
        let ev = Events::default();
        let s = spawn(
            vec![sh("crashy", "exit 1")],
            Settings { initial_backoff: Duration::from_secs(300), ..fast() },
            ev.sink(),
        );
        ev.wait_for(1, |e| matches!(e, Event::Restarting { .. }).then_some(())).await;
        let t = std::time::Instant::now();
        shut_down(s).await;
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn very_long_lines_are_cut_and_floods_do_not_block_the_plugin() {
        let ev = Events::default();
        // One 200 KB line without a newline until the end, then 20000 short lines, then a marker.
        let script = "head -c 200000 /dev/zero | tr '\\0' x; echo; yes line | head -n 20000; echo done; sleep 30";
        let s = spawn(vec![sh("loud", script)], fast(), ev.sink());
        let all = ev.lines(20_002).await;
        let long = &all[0].1;
        assert!(long.ends_with(" [line cut]") && long.len() <= MAX_LOG_LINE + " [line cut]".len(), "{}", long.len());
        assert_eq!(all.last().unwrap().1, "done", "the plugin was not blocked by its own output");
        shut_down(s).await;
    }

    #[tokio::test]
    async fn plugins_are_supervised_independently() {
        let ev = Events::default();
        let s = spawn(vec![sh("a", "echo a; sleep 300"), sh("b", "exit 1")], fast(), ev.sink());
        ev.lines(1).await;
        ev.wait_for(2, |e| matches!(e, Event::Restarting { id, .. } if id == "b").then_some(())).await;
        let starts_of_a = ev.all().iter().filter(|e| matches!(e, Event::Started { id, .. } if id == "a")).count();
        assert_eq!(starts_of_a, 1, "b crashing does not disturb a");
        shut_down(s).await;
    }
}
