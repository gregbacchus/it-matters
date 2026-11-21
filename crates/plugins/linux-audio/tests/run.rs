//! The plugin loop against the real host (Unix socket) with a scripted audio backend.
//! `real_pipewire_*` need a PipeWire session and are `#[ignore]`d: `cargo test -- --ignored`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use it_matters_core::allocator::{Allocator, MemoryStore};
use it_matters_core::frontend::{self, AttrValue, FakeFrontend, FrontendEvent, Write};
use it_matters_core::host::{HostSettings, PluginSettings, bind_unix, serve_unix, spawn_host};
use it_matters_core::registry::Registry;
use it_matters_plugin_linux_audio::backend::{Audio, Reading};
use it_matters_plugin_linux_audio::run;
use it_matters_plugin_sdk::connect;
use tokio::time::{sleep, timeout};

/// A pretend audio system. Volumes are quantised to steps of 5 to show that the plugin reports what
/// the system really did, not what was asked.
#[derive(Clone)]
struct Fake {
    state: Arc<Mutex<Reading>>,
    calls: Arc<Mutex<Vec<String>>>,
    fail_reads: Arc<Mutex<bool>>,
    fail_writes: Arc<Mutex<Option<String>>>,
    read_delay: Duration,
}

impl Fake {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Reading { volume: 30, muted: false, mic_active: false })),
            calls: Arc::default(),
            fail_reads: Arc::default(),
            fail_writes: Arc::default(),
            read_delay: Duration::ZERO,
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Audio for Fake {
    async fn read(&mut self) -> Result<Reading, String> {
        // The snapshot is taken first and returned after the delay: a slow read is a stale read.
        let snapshot = *self.state.lock().unwrap();
        sleep(self.read_delay).await;
        if *self.fail_reads.lock().unwrap() {
            return Err("PipeWire is restarting".into());
        }
        Ok(snapshot)
    }
    async fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("volume {percent}"));
        if let Some(e) = self.fail_writes.lock().unwrap().clone() {
            return Err(e);
        }
        self.state.lock().unwrap().volume = (percent / 5) * 5;
        Ok(())
    }
    async fn set_mute(&mut self, muted: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("mute {muted}"));
        if let Some(e) = self.fail_writes.lock().unwrap().clone() {
            return Err(e);
        }
        self.state.lock().unwrap().muted = muted;
        Ok(())
    }
}

struct Setup {
    fake: Fake,
    frontend: FakeFrontend,
    dir: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str, fake: Fake, poll: Duration) -> Setup {
    start_with(name, fake, poll, false).await
}

async fn start_with(name: &str, fake: Fake, poll: Duration, read_only: bool) -> Setup {
    let dir = std::env::temp_dir().join(format!("it-matters-audio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let (host_end, frontend_end) = frontend::channel();
    let registry = Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new());
    let settings = HostSettings::new(BTreeMap::from([(
        "linux-audio".to_owned(),
        PluginSettings { set_timeout: Duration::from_secs(2) },
    )]));
    let (host, _actor) = spawn_host(settings, registry, host_end);
    tokio::spawn(serve_unix(bind_unix(&socket).unwrap(), host));

    let mut backend = fake.clone();
    tokio::spawn(async move {
        let (client, mut events) = connect(&socket, "linux-audio", "test").await.unwrap();
        run(&client, &mut events, &mut backend, "pc-audio", "PC Audio", poll, read_only).await;
    });
    Setup { fake, frontend: FakeFrontend::new(frontend_end), dir }
}

async fn event(s: &Setup) -> FrontendEvent {
    timeout(Duration::from_secs(3), s.frontend.next_event()).await.expect("no event").unwrap()
}

fn level(endpoint: u16, percent: u8) -> FrontendEvent {
    // 0..=100 scaled to 0..=254, rounding to nearest.
    FrontendEvent::SetAttr { endpoint, value: AttrValue::CurrentLevel(((u32::from(percent) * 254 + 50) / 100) as u8) }
}

/// Consumes the start-up events and returns once the initial state (volume 30, not muted, mic idle) is in.
async fn settle(s: &Setup) {
    for _ in 0..2 {
        assert!(matches!(event(s).await, FrontendEvent::AddEndpoint(_)));
    }
    assert_eq!(event(s).await, level(2, 30));
    assert_eq!(event(s).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::OnOff(true) });
    assert_eq!(event(s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::Occupancy(false) });
}

#[tokio::test]
async fn it_registers_a_speaker_and_a_microphone_sensor_and_reports_the_initial_state() {
    let s = start("reg", Fake::new(), Duration::from_secs(60)).await;
    let FrontendEvent::AddEndpoint(speaker) = event(&s).await else { panic!() };
    let FrontendEvent::AddEndpoint(mic) = event(&s).await else { panic!() };
    assert_eq!((speaker.endpoint, speaker.unique_id.as_str()), (2, "pc-audio/volume"));
    assert_eq!((mic.endpoint, mic.unique_id.as_str()), (3, "pc-audio/mic-active"));
    assert_eq!(event(&s).await, level(2, 30));
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::OnOff(true) });
}

#[tokio::test]
async fn changes_made_outside_matter_are_reported() {
    let s = start("outside", Fake::new(), Duration::from_millis(30)).await;
    settle(&s).await;
    s.fake.state.lock().unwrap().volume = 80;
    assert_eq!(event(&s).await, level(2, 80));
    s.fake.state.lock().unwrap().muted = true;
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::OnOff(false) });
    s.fake.state.lock().unwrap().mic_active = true;
    assert_eq!(event(&s).await, FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::Occupancy(true) });
}

#[tokio::test]
async fn a_controller_volume_write_reaches_the_audio_system() {
    let s = start("volume", Fake::new(), Duration::from_secs(60)).await;
    settle(&s).await;
    let answer = s.frontend.write(2, Write::CurrentLevel(127)).await; // 127 of 254 = 50 %
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    assert_eq!(s.fake.calls(), ["volume 50"]);
}

#[tokio::test]
async fn after_a_write_the_real_resulting_value_is_reported_at_once_not_at_the_next_poll() {
    // The poll is a minute away. The fake quantises 52 % to 50 %: the host first shows the requested
    // value, and the plugin's immediate re-read corrects it. If the plugin waited for the poll, this
    // would time out.
    let s = start("reread", Fake::new(), Duration::from_secs(60)).await;
    settle(&s).await;
    let answer = s.frontend.write(2, Write::CurrentLevel(132)).await; // 132/254 = 52 %
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    assert_eq!(s.fake.calls(), ["volume 52"]);
    let mut seen = Vec::new();
    while seen.last() != Some(&level(2, 50)) {
        seen.push(event(&s).await);
    }
    assert!(seen.contains(&level(2, 52)) || seen.len() == 1, "the host may show the requested value first: {seen:?}");
}

#[tokio::test]
async fn muting_from_a_controller_mutes() {
    let s = start("mute", Fake::new(), Duration::from_secs(60)).await;
    settle(&s).await;
    let answer = s.frontend.write(2, Write::OnOff(false)).await; // Off = muted
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    assert_eq!(s.fake.calls(), ["mute true"]);
    let answer = s.frontend.write(2, Write::OnOff(true)).await;
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    assert_eq!(s.fake.calls(), ["mute true", "mute false"]);
}

#[tokio::test]
async fn a_failed_write_tells_the_controller_why_and_changes_nothing() {
    let s = start("fail", Fake::new(), Duration::from_secs(60)).await;
    settle(&s).await;
    *s.fake.fail_writes.lock().unwrap() = Some("`wpctl` failed: no such node".into());
    let answer = s.frontend.write(2, Write::CurrentLevel(200)).await;
    let reason = answer.recv().await.unwrap().unwrap_err();
    assert!(reason.contains("no such node"), "{reason}");
    sleep(Duration::from_millis(100)).await;
    assert_eq!(s.frontend.drain(), vec![], "a refused write must not change anything");
}

#[tokio::test]
async fn a_failed_read_keeps_the_last_values_and_recovery_reports_the_truth() {
    let s = start("readfail", Fake::new(), Duration::from_millis(20)).await;
    settle(&s).await;
    *s.fake.fail_reads.lock().unwrap() = true;
    s.fake.state.lock().unwrap().volume = 90;
    sleep(Duration::from_millis(200)).await;
    assert_eq!(s.frontend.drain(), vec![], "nothing is reported while the audio system cannot be read");
    *s.fake.fail_reads.lock().unwrap() = false;
    assert_eq!(event(&s).await, level(2, 90));
}

#[tokio::test]
async fn a_slow_stale_read_never_overtakes_a_write() {
    // Every read takes 60 ms and returns the state it saw at its start. If reads could overlap a write,
    // an old snapshot could be published after the write and flip the controller's change back.
    let mut fake = Fake::new();
    fake.read_delay = Duration::from_millis(60);
    let s = start("race", fake, Duration::from_millis(10)).await;
    settle(&s).await;
    let answer = s.frontend.write(2, Write::CurrentLevel(127)).await; // 50 %
    assert_eq!(answer.recv().await.unwrap(), Ok(()));
    sleep(Duration::from_millis(500)).await;
    let events = s.frontend.drain();
    let last_level = events
        .iter()
        .rev()
        .find(|e| matches!(e, FrontendEvent::SetAttr { value: AttrValue::CurrentLevel(_), .. }))
        .cloned();
    assert_eq!(last_level, Some(level(2, 50)), "the final reported volume must be the written one: {events:?}");
    let after_new = events.iter().skip_while(|e| **e != level(2, 50)).filter(|e| **e == level(2, 30)).count();
    assert_eq!(after_new, 0, "the old value reappeared after the write: {events:?}");
}

#[tokio::test]
async fn read_only_mode_still_reports_everything_but_refuses_every_controller_write() {
    let s = start_with("readonly", Fake::new(), Duration::from_millis(30), true).await;
    let FrontendEvent::AddEndpoint(speaker) = event(&s).await else { panic!() };
    assert_eq!(speaker.kind, it_matters_core::mapping::UnitKind::Speaker, "a read-only speaker is still a Speaker");
    assert!(matches!(event(&s).await, FrontendEvent::AddEndpoint(_)));
    assert_eq!(event(&s).await, level(2, 30));
    // Changes made elsewhere (the keyboard, the desktop's slider) are still reported.
    s.fake.state.lock().unwrap().volume = 80;
    loop {
        if event(&s).await == level(2, 80) {
            break;
        }
    }
    // A controller cannot change anything: the host refuses without asking the plugin.
    for write in [Write::CurrentLevel(127), Write::OnOff(false), Write::OnOff(true)] {
        let answer = s.frontend.write(2, write).await;
        let reason = answer.recv().await.unwrap().unwrap_err();
        assert!(reason.contains("read-only"), "{reason}");
    }
    sleep(Duration::from_millis(100)).await;
    assert_eq!(s.fake.calls(), Vec::<String>::new(), "nothing may reach the audio system");
}

/// Even if a (buggy or hostile) host sent a `set` anyway, a read-only plugin must not change the volume.
#[tokio::test]
async fn a_read_only_plugin_refuses_a_set_even_if_one_arrives() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let dir = std::env::temp_dir().join(format!("it-matters-audio-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let listener = bind_unix(&socket).unwrap();
    let reply = Arc::new(Mutex::new(None::<String>));
    let seen = reply.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd).lines();
        rd.next_line().await.unwrap();
        wr.write_all(b"{\"type\":\"welcome\",\"proto\":1}\n").await.unwrap();
        // After the plugin has registered, send a write the plugin must not obey.
        while let Ok(Some(line)) = rd.next_line().await {
            if line.contains("\"type\":\"register\"") {
                wr.write_all(
                    b"{\"type\":\"set\",\"req_id\":9,\"device\":\"pc-audio\",\"capability\":\"volume\",\"value\":50}\n",
                )
                .await
                .unwrap();
            }
            if line.contains("\"type\":\"set_result\"") {
                *seen.lock().unwrap() = Some(line);
            }
        }
    });
    let fake = Fake::new();
    let mut backend = fake.clone();
    tokio::spawn(async move {
        let (client, mut events) = connect(&socket, "linux-audio", "test").await.unwrap();
        run(&client, &mut events, &mut backend, "pc-audio", "PC Audio", Duration::from_secs(60), true).await;
    });
    for _ in 0..100 {
        if reply.lock().unwrap().is_some() {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let line = reply.lock().unwrap().clone().expect("the plugin never answered the set");
    assert!(line.contains("\"req_id\":9") && line.contains("\"ok\":false") && line.contains("--read-only"), "{line}");
    assert_eq!(fake.calls(), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn only_changes_are_sent_over_the_wire() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let dir = std::env::temp_dir().join(format!("it-matters-audio-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("p.sock");
    let listener = bind_unix(&socket).unwrap();
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd).lines();
        rd.next_line().await.unwrap();
        wr.write_all(b"{\"type\":\"welcome\",\"proto\":1}\n").await.unwrap();
        while let Ok(Some(l)) = rd.next_line().await {
            sink.lock().unwrap().push(l);
        }
    });
    let fake = Fake::new();
    let mut backend = fake.clone();
    tokio::spawn(async move {
        let (client, mut events) = connect(&socket, "linux-audio", "test").await.unwrap();
        run(&client, &mut events, &mut backend, "pc-audio", "PC Audio", Duration::from_millis(10), false).await;
    });
    let updates =
        |cap: &str| lines.lock().unwrap().iter().filter(|l| l.contains(&format!("\"capability\":\"{cap}\""))).count();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        (updates("volume"), updates("mute"), updates("mic-active")),
        (1, 1, 1),
        "dozens of polls, one report each"
    );
    fake.state.lock().unwrap().volume = 55;
    sleep(Duration::from_millis(200)).await;
    assert_eq!(
        (updates("volume"), updates("mute"), updates("mic-active")),
        (2, 1, 1),
        "only the changed value is sent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the real PipeWire (ignored by default) ---------------------------------------------------------

/// Creates a disposable null sink with the lowest priority (so it never becomes the default) and removes
/// it again when dropped: the node lives only as long as the `pw-cli` process that created it.
struct TestSink {
    pw_cli: std::process::Child,
}

impl TestSink {
    const NAME: &'static str = "it-matters-test-sink";

    fn create() -> Self {
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        let mut pw_cli = Command::new("pw-cli")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("pw-cli is needed for this test");
        writeln!(
            pw_cli.stdin.as_mut().unwrap(),
            "create-node adapter {{ factory.name=support.null-audio-sink node.name={} media.class=Audio/Sink \
             audio.position=[FL,FR] priority.session=1 priority.driver=1 object.linger=false }}",
            Self::NAME
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        Self { pw_cli }
    }
}

impl Drop for TestSink {
    fn drop(&mut self) {
        drop(self.pw_cli.stdin.take());
        let _ = self.pw_cli.kill();
        let _ = self.pw_cli.wait();
    }
}

fn default_sink_state() -> String {
    let out = std::process::Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SINK@"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Writes only to a disposable null sink; reads the real default only to prove it was not touched.
#[tokio::test]
#[ignore = "needs a PipeWire session with wpctl and pw-dump; creates a temporary null sink"]
async fn real_pipewire_reads_and_writes_a_disposable_sink_and_leaves_the_default_alone() {
    use it_matters_plugin_linux_audio::backend::{Target, Wpctl};
    use it_matters_plugin_linux_audio::exec::RealExec;

    let before = default_sink_state();
    {
        let _sink = TestSink::create();
        let mut audio = Wpctl::new(RealExec::default(), Target::Name(TestSink::NAME.into()));
        let first = audio.read().await.expect("read the test sink");
        println!("test sink starts at {first:?}");

        audio.set_volume(42).await.unwrap();
        assert_eq!(audio.read().await.unwrap().volume, 42);
        audio.set_volume(7).await.unwrap();
        assert_eq!(audio.read().await.unwrap().volume, 7);
        audio.set_mute(true).await.unwrap();
        assert!(audio.read().await.unwrap().muted);
        audio.set_mute(false).await.unwrap();
        assert!(!audio.read().await.unwrap().muted);
        assert!(!audio.read().await.unwrap().mic_active, "nothing is capturing");

        let mut missing = Wpctl::new(RealExec::default(), Target::Name("no-such-node".into()));
        assert!(missing.read().await.unwrap_err().contains("no PipeWire node named"));
    }
    assert_eq!(default_sink_state(), before, "the real default speaker must not have been touched");
}

/// Captures into /dev/null for a moment (the microphone indicator may light up).
#[tokio::test]
#[ignore = "needs a PipeWire session with a microphone-type source; records a few seconds to /dev/null"]
async fn real_pipewire_tells_microphone_capture_from_desktop_audio_capture() {
    use it_matters_plugin_linux_audio::backend::{Target, Wpctl};
    use it_matters_plugin_linux_audio::exec::RealExec;
    use std::process::{Command, Stdio};

    let mut audio = Wpctl::new(RealExec::default(), Target::DefaultSink);
    assert!(!audio.read().await.unwrap().mic_active, "start from an idle microphone");

    let capture = |args: &[&str]| {
        Command::new("pw-cat")
            .args(["--record"])
            .args(args)
            .arg("/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    // The default microphone.
    let mut mic = capture(&[]);
    sleep(Duration::from_millis(1500)).await;
    assert!(audio.read().await.unwrap().mic_active, "an app capturing the microphone");
    mic.kill().unwrap();
    mic.wait().unwrap();
    sleep(Duration::from_millis(500)).await;
    assert!(!audio.read().await.unwrap().mic_active);

    // The monitor of the default speaker (what a screen recorder captures).
    let sink = String::from_utf8_lossy(
        &Command::new("wpctl").args(["inspect", "@DEFAULT_AUDIO_SINK@"]).output().unwrap().stdout,
    )
    .lines()
    .next()
    .and_then(|l| l.strip_prefix("id "))
    .and_then(|l| l.split(',').next())
    .map(str::to_owned)
    .expect("the default sink's id");
    let mut desktop = capture(&["-P", "stream.capture.sink=true", "--target", &sink]);
    sleep(Duration::from_millis(1500)).await;
    assert!(!audio.read().await.unwrap().mic_active, "capturing desktop audio is not the microphone");
    desktop.kill().unwrap();
    desktop.wait().unwrap();
}
