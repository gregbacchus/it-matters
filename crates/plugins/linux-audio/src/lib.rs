//! The Linux audio plugin: the speaker's volume and mute, and whether a microphone is in use.
//!
//! One loop owns the audio backend, so a poll and a controller's write never overlap: after a write the
//! loop re-reads at once, and the next poll cannot report a stale value.

use std::time::Duration;

use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role, Value};
use it_matters_plugin_sdk::{Client, Event, Events, SetRequest};
use tokio::time::sleep;

pub mod backend;
pub mod exec;
pub mod graph;

use backend::{Audio, Reading, Target};

/// Command line flags; `docs/plugins/linux-audio.md` must document each (a test checks it).
pub const CLI_ITEMS: &[&str] = &["--device", "--name", "--sink", "--poll-ms", "--help", "--version"];

pub const CAP_VOLUME: &str = "volume";
pub const CAP_MUTE: &str = "mute";
pub const CAP_MIC: &str = "mic-active";

/// The device: a speaker (volume 0-100 and mute) and a microphone-in-use sensor.
pub fn device_spec(device: &str, name: &str) -> DeviceSpec {
    DeviceSpec {
        id: device.into(),
        name: name.into(),
        capabilities: vec![
            CapabilitySpec {
                id: CAP_VOLUME.into(),
                kind: CapabilityKind::Level { min: 0, max: 100 },
                writable: true,
                role: None,
            },
            CapabilitySpec {
                id: CAP_MUTE.into(),
                kind: CapabilityKind::Boolean,
                writable: true,
                role: Some(Role::Mute),
            },
            CapabilitySpec {
                id: CAP_MIC.into(),
                kind: CapabilityKind::Boolean,
                writable: false,
                role: Some(Role::Occupancy),
            },
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub device: Option<String>,
    pub name: Option<String>,
    pub sink: Target,
    pub poll: Duration,
}

pub enum Parsed {
    Run(Args),
    Help,
    Version,
}

pub const DEFAULT_POLL: Duration = Duration::from_secs(1);

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut out = Args { device: None, name: None, sink: Target::DefaultSink, poll: DEFAULT_POLL };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--device" => out.device = Some(value("--device")?),
            "--name" => out.name = Some(value("--name")?),
            "--sink" => out.sink = Target::parse(&value("--sink")?)?,
            "--poll-ms" => {
                let ms: u64 = value("--poll-ms")?.parse().map_err(|_| "--poll-ms needs a whole number".to_owned())?;
                if ms < 100 {
                    return Err("--poll-ms must be at least 100".into());
                }
                out.poll = Duration::from_millis(ms);
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if let Some(d) = &out.device
        && !it_matters_model::valid_id(d)
    {
        return Err(format!("--device {d:?} must be 1-64 characters of a-z, 0-9, '-' or '_'"));
    }
    Ok(Parsed::Run(out))
}

/// Sends whatever differs from what was last sent.
async fn publish(client: &Client, device: &str, last: &mut Option<Reading>, now: Reading) -> Result<(), String> {
    let was = *last;
    let send = |cap: &'static str, value: Value| async move {
        client.update(device, cap, value).await.map_err(|e| e.to_string())
    };
    if was.map(|r| r.volume) != Some(now.volume) {
        send(CAP_VOLUME, Value::Int(i64::from(now.volume))).await?;
    }
    if was.map(|r| r.muted) != Some(now.muted) {
        send(CAP_MUTE, Value::Bool(now.muted)).await?;
    }
    if was.map(|r| r.mic_active) != Some(now.mic_active) {
        send(CAP_MIC, Value::Bool(now.mic_active)).await?;
        println!("microphone in use: {}", now.mic_active);
    }
    *last = Some(now);
    Ok(())
}

/// Applies a controller's write to the audio system.
async fn apply<A: Audio>(audio: &mut A, request: &SetRequest) -> Result<(), String> {
    match (request.capability.as_str(), request.value) {
        (CAP_VOLUME, Value::Int(v)) if (0..=100).contains(&v) => audio.set_volume(v as u8).await,
        (CAP_MUTE, Value::Bool(m)) => audio.set_mute(m).await,
        (cap, value) => Err(format!("cannot set {cap} to {value:?}")),
    }
}

/// Registers the device and keeps it current until the connection ends. Returns why it ended.
///
/// A failed read keeps the last values and is retried at the next poll (the error is printed when it
/// changes, not on every poll). A failed write is reported to the controller.
pub async fn run<A: Audio>(
    client: &Client,
    events: &mut Events,
    audio: &mut A,
    device: &str,
    name: &str,
    poll: Duration,
) -> String {
    if let Err(e) = client.register(device_spec(device, name)).await {
        return e.to_string();
    }
    let mut last: Option<Reading> = None;
    let mut last_error = String::new();
    loop {
        match audio.read().await {
            Ok(now) => {
                last_error.clear();
                if let Err(e) = publish(client, device, &mut last, now).await {
                    return e;
                }
            }
            Err(e) => {
                if e != last_error {
                    eprintln!("cannot read the audio state (keeping the last values, will retry): {e}");
                    last_error = e;
                }
            }
        }
        tokio::select! {
            () = sleep(poll) => {}
            event = events.next() => match event {
                Some(Event::Set(request)) => {
                    match apply(audio, &request).await {
                        Ok(()) => request.accept().await,
                        Err(e) => {
                            eprintln!("controller write failed: {e}");
                            request.refuse(e).await;
                        }
                    }
                    // The loop continues straight to a fresh read, so the new state is reported at once.
                }
                Some(Event::HostError(message)) => eprintln!("the host rejected something: {message}"),
                Some(Event::Closed(reason)) => return reason,
                None => return "the connection ended".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_is_a_speaker_plus_an_occupancy_sensor() {
        let d = device_spec("pc-audio", "PC Audio");
        assert!(d.validate().is_ok());
        let ids: Vec<_> = d.capabilities.iter().map(|c| (c.id.as_str(), c.writable)).collect();
        assert_eq!(ids, [(CAP_VOLUME, true), (CAP_MUTE, true), (CAP_MIC, false)]);
    }

    fn args(a: &[&str]) -> Result<Parsed, String> {
        parse_args(a.iter().map(|x| x.to_string()))
    }

    #[test]
    fn arguments_parse() {
        let Ok(Parsed::Run(a)) = args(&["--device", "pc-audio", "--name", "PC", "--sink", "42", "--poll-ms", "500"])
        else {
            panic!()
        };
        assert_eq!(
            (a.device.as_deref(), a.name.as_deref(), a.sink, a.poll),
            (Some("pc-audio"), Some("PC"), Target::Id(42), Duration::from_millis(500))
        );
        let Ok(Parsed::Run(a)) = args(&[]) else { panic!() };
        assert_eq!((a.device, a.sink, a.poll), (None, Target::DefaultSink, DEFAULT_POLL));
    }

    #[test]
    fn bad_arguments_are_rejected() {
        for bad in [&["--sink"][..], &["--sink", "@NOPE@"], &["--device", "Bad Id"], &["--poll-ms", "50"], &["--zzz"]] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_cli_item_is_accepted() {
        for item in CLI_ITEMS {
            let a: Vec<&str> = match *item {
                "--device" | "--name" | "--sink" => vec![item, "x1"],
                "--poll-ms" => vec![item, "1000"],
                _ => vec![item],
            };
            assert!(args(&a).is_ok(), "{item} is not accepted");
        }
    }

    /// Every flag must have a row in the plugin's documentation page (see AGENTS.md).
    #[test]
    fn the_documentation_covers_every_flag() {
        let page = include_str!("../../../../docs/plugins/linux-audio.md");
        for item in CLI_ITEMS {
            assert!(
                page.lines()
                    .any(|l| l.starts_with("| `")
                        && l.split('|').nth(1).is_some_and(|c| c.contains(&format!("`{item}")))),
                "docs/plugins/linux-audio.md has no table row for `{item}`"
            );
        }
    }
}
