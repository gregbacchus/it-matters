//! The audio backend: reads and changes the speaker with `wpctl`, and finds out about microphone use
//! with `pw-dump`. Behind the [`Audio`] trait so the plugin loop is tested without PipeWire.

use crate::exec::Exec;
use crate::graph::{parse_pw_dump, parse_volume, percent_arg, to_percent};

/// One look at the audio system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// 0..=100 (the speaker's `wpctl` volume times 100, clamped).
    pub volume: u8,
    pub muted: bool,
    /// An application is capturing from a microphone-type source.
    pub mic_active: bool,
}

/// What the plugin needs from the audio system. The plugin loop calls these one at a time, never
/// concurrently, so a slow poll can never overtake a controller's write.
#[allow(async_fn_in_trait)]
pub trait Audio {
    async fn read(&mut self) -> Result<Reading, String>;
    async fn set_volume(&mut self, percent: u8) -> Result<(), String>;
    async fn set_mute(&mut self, muted: bool) -> Result<(), String>;
}

/// Which speaker to control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `@DEFAULT_AUDIO_SINK@`: whatever the desktop currently uses (follows the user's choice).
    DefaultSink,
    /// A PipeWire node id. Ids change when PipeWire restarts; prefer a name.
    Id(u32),
    /// A `node.name` such as `alsa_output.pci-0000_c4_00.6.analog-stereo`, looked up on every read.
    Name(String),
}

impl Target {
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("--sink must not be empty".into());
        }
        if t == "@DEFAULT_AUDIO_SINK@" {
            return Ok(Target::DefaultSink);
        }
        if t.starts_with('@') {
            return Err(format!("--sink {t:?}: only @DEFAULT_AUDIO_SINK@ is supported as a special name"));
        }
        if t.bytes().all(|b| b.is_ascii_digit()) {
            return t.parse().map(Target::Id).map_err(|_| format!("--sink {t:?} is not a valid node id"));
        }
        Ok(Target::Name(t.to_owned()))
    }
}

/// The real backend.
pub struct Wpctl<E: Exec> {
    exec: E,
    target: Target,
    resolved: Option<u32>,
}

impl<E: Exec> Wpctl<E> {
    pub fn new(exec: E, target: Target) -> Self {
        Self { exec, target, resolved: None }
    }

    async fn graph(&self) -> Result<crate::graph::Graph, String> {
        parse_pw_dump(&self.exec.run("pw-dump", &[]).await?)
    }

    fn name_error(name: &str) -> String {
        format!("no PipeWire node named {name:?} (see `wpctl status` and `pw-dump`)")
    }

    /// The argument that addresses the speaker in `wpctl` commands.
    async fn target_arg(&mut self) -> Result<String, String> {
        match &self.target {
            Target::DefaultSink => Ok("@DEFAULT_AUDIO_SINK@".to_owned()),
            Target::Id(id) => Ok(id.to_string()),
            Target::Name(name) => {
                if let Some(id) = self.resolved {
                    return Ok(id.to_string());
                }
                let id = self.graph().await?.node_id(name).ok_or_else(|| Self::name_error(name))?;
                self.resolved = Some(id);
                Ok(id.to_string())
            }
        }
    }
}

impl<E: Exec> Audio for Wpctl<E> {
    async fn read(&mut self) -> Result<Reading, String> {
        let graph = self.graph().await?;
        if let Target::Name(name) = &self.target {
            // Looked up again on every read, so a restart of PipeWire (new ids) is followed.
            self.resolved = Some(graph.node_id(name).ok_or_else(|| Self::name_error(name))?);
        }
        let arg = self.target_arg().await?;
        let (volume, muted) = parse_volume(&self.exec.run("wpctl", &["get-volume".to_owned(), arg]).await?)?;
        Ok(Reading { volume: to_percent(volume), muted, mic_active: graph.mic_active })
    }

    async fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        let arg = self.target_arg().await?;
        self.exec.run("wpctl", &["set-volume".to_owned(), arg, percent_arg(percent)]).await.map(|_| ())
    }

    async fn set_mute(&mut self, muted: bool) -> Result<(), String> {
        let arg = self.target_arg().await?;
        self.exec
            .run("wpctl", &["set-mute".to_owned(), arg, if muted { "1" } else { "0" }.to_owned()])
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers `pw-dump` and `wpctl` from scripts and records every call.
    struct Fake {
        calls: Mutex<Vec<String>>,
        dump: Mutex<String>,
        volume: Mutex<Result<String, String>>,
    }

    impl Fake {
        fn new(dump: &str, volume: &str) -> Self {
            Self { calls: Mutex::default(), dump: Mutex::new(dump.into()), volume: Mutex::new(Ok(volume.into())) }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Exec for &Fake {
        async fn run(&self, program: &str, args: &[String]) -> Result<String, String> {
            self.calls.lock().unwrap().push(format!("{program} {}", args.join(" ")).trim().to_owned());
            match (program, args.first().map(String::as_str)) {
                ("pw-dump", _) => Ok(self.dump.lock().unwrap().clone()),
                ("wpctl", Some("get-volume")) => self.volume.lock().unwrap().clone(),
                ("wpctl", Some("set-volume" | "set-mute")) => Ok(String::new()),
                _ => Err("unexpected command".into()),
            }
        }
    }

    const DUMP: &str = r#"[
      {"id":60,"type":"PipeWire:Interface:Node","info":{"state":"suspended","props":{"media.class":"Audio/Sink","node.name":"speakers"}}},
      {"id":61,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Audio/Source","node.name":"mic"}}}
    ]"#;

    #[test]
    fn targets_parse() {
        assert_eq!(Target::parse("@DEFAULT_AUDIO_SINK@"), Ok(Target::DefaultSink));
        assert_eq!(Target::parse("60"), Ok(Target::Id(60)));
        assert_eq!(
            Target::parse("alsa_output.pci-0000_c4_00.6.analog-stereo"),
            Ok(Target::Name("alsa_output.pci-0000_c4_00.6.analog-stereo".into()))
        );
        assert!(Target::parse("").is_err() && Target::parse("@DEFAULT_AUDIO_SOURCE@").is_err());
        assert!(Target::parse("99999999999999999999").is_err());
    }

    #[tokio::test]
    async fn the_default_sink_is_addressed_by_its_alias() {
        let f = Fake::new(DUMP, "Volume: 0.30 [MUTED]\n");
        let mut a = Wpctl::new(&f, Target::DefaultSink);
        assert_eq!(a.read().await, Ok(Reading { volume: 30, muted: true, mic_active: false }));
        assert_eq!(f.calls(), ["pw-dump", "wpctl get-volume @DEFAULT_AUDIO_SINK@"]);
    }

    #[tokio::test]
    async fn writes_use_the_documented_wpctl_commands() {
        let f = Fake::new(DUMP, "Volume: 0.50\n");
        let mut a = Wpctl::new(&f, Target::Id(60));
        a.set_volume(30).await.unwrap();
        a.set_mute(true).await.unwrap();
        a.set_mute(false).await.unwrap();
        assert_eq!(f.calls(), ["wpctl set-volume 60 0.30", "wpctl set-mute 60 1", "wpctl set-mute 60 0"]);
    }

    #[tokio::test]
    async fn a_node_name_is_resolved_to_the_current_id_on_every_read() {
        let f = Fake::new(DUMP, "Volume: 0.50\n");
        let mut a = Wpctl::new(&f, Target::Name("speakers".into()));
        a.read().await.unwrap();
        assert_eq!(f.calls()[1], "wpctl get-volume 60");
        // PipeWire restarted: the same node now has another id.
        *f.dump.lock().unwrap() = DUMP.replace("\"id\":60", "\"id\":160");
        a.read().await.unwrap();
        assert_eq!(f.calls().last().unwrap(), "wpctl get-volume 160");
        a.set_mute(true).await.unwrap();
        assert_eq!(f.calls().last().unwrap(), "wpctl set-mute 160 1", "a write uses the id from the last read");
    }

    #[tokio::test]
    async fn an_unknown_node_name_is_a_clear_error() {
        let f = Fake::new(DUMP, "Volume: 0.50\n");
        let mut a = Wpctl::new(&f, Target::Name("nope".into()));
        assert!(a.read().await.unwrap_err().contains("no PipeWire node named \"nope\""));
        assert!(a.set_mute(true).await.is_err());
    }

    #[tokio::test]
    async fn tool_failures_are_reported() {
        let f = Fake::new(DUMP, "");
        *f.volume.lock().unwrap() = Err("`wpctl` failed".into());
        assert!(Wpctl::new(&f, Target::DefaultSink).read().await.unwrap_err().contains("wpctl"));
        *f.volume.lock().unwrap() = Ok("garbage".into());
        assert!(Wpctl::new(&f, Target::DefaultSink).read().await.unwrap_err().contains("unexpected"));
    }
}
