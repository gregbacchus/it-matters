//! The logic of the Linux system plugin: which devices it registers and when "signed in" is true.
//! The D-Bus access lives in [`logind`]; everything else is testable without a system bus.

use std::time::Duration;

use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role, Value};
use it_matters_plugin_sdk::{Client, Event, Events, RemoteOptions};
use tokio::time::sleep;

pub mod logind;

/// The command line flags of the plugin binary. `docs/plugins/linux-system.md` must document each
/// (a test checks it).
pub const CLI_ITEMS: &[&str] =
    &["--device", "--name", "--poll-ms", "--connect", "--token-file", "--plugin-id", "--help", "--version"];

pub const CAP_ONLINE: &str = "online";
pub const CAP_SIGNED_IN: &str = "signed-in";

/// What the plugin needs from logind: the sessions, and a signal when they change.
#[allow(async_fn_in_trait)]
pub trait SessionSource {
    async fn sessions(&mut self) -> Result<Vec<SessionInfo>, String>;
    /// Resolves when a session was added or removed. May resolve spuriously, or never.
    async fn changed(&mut self);
}

/// The logind properties that decide whether someone is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// `Class`: `user`, `greeter`, `lock-screen`, `manager`, ...
    pub class: String,
    /// `Type`: `x11`, `wayland`, `mir`, `tty`, `unspecified`.
    pub kind: String,
    /// `State`: `online`, `active`, `closing`.
    pub state: String,
    /// `Remote`: true for ssh and similar.
    pub remote: bool,
}

/// Someone is signed in at the machine: a **local, graphical user session** that is not closing. Text
/// consoles (`tty`), remote (ssh) sessions, the login screen and the per-user `manager` session do not
/// count. A locked screen still counts as signed in.
pub fn signed_in(sessions: &[SessionInfo]) -> bool {
    sessions.iter().any(|s| {
        s.class == "user"
            && !s.remote
            && matches!(s.kind.as_str(), "x11" | "wayland" | "mir")
            && matches!(s.state.as_str(), "online" | "active")
    })
}

/// The device this plugin registers: two read-only sensors.
pub fn device_spec(device: &str, name: &str) -> DeviceSpec {
    let sensor = |id: &str| CapabilitySpec {
        id: id.into(),
        kind: CapabilityKind::Boolean,
        writable: false,
        role: Some(Role::Contact),
    };
    DeviceSpec { id: device.into(), name: name.into(), capabilities: vec![sensor(CAP_ONLINE), sensor(CAP_SIGNED_IN)] }
}

/// Command line options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Device id (`--device`); `None` means derive it from the host name.
    pub device: Option<String>,
    /// Display name (`--name`); `None` means the host name.
    pub name: Option<String>,
    pub poll: Duration,
    /// `--connect`, `--token-file`, `--plugin-id`: run as a remote agent instead of a daemon-started plugin.
    pub remote: RemoteOptions,
}

pub enum Parsed {
    Run(Args),
    Help,
    Version,
}

pub const DEFAULT_POLL: Duration = Duration::from_secs(5);

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut out = Args { device: None, name: None, poll: DEFAULT_POLL, remote: RemoteOptions::default() };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--device" => out.device = Some(value("--device")?),
            "--name" => out.name = Some(value("--name")?),
            "--poll-ms" => {
                let ms: u64 = value("--poll-ms")?.parse().map_err(|_| "--poll-ms needs a whole number".to_owned())?;
                if ms < 100 {
                    return Err("--poll-ms must be at least 100".into());
                }
                out.poll = Duration::from_millis(ms);
            }
            flag @ ("--connect" | "--token-file" | "--plugin-id") => out.remote.set(flag, value(flag)?)?,
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

/// Registers the device and keeps `signed-in` current until the connection ends. Returns why it ended.
///
/// Sensors are updated only when a value changes. A failed read keeps the last value and is retried
/// at the next poll. The loop re-reads whenever logind reports a session change and at least every
/// `poll` (state changes of an existing session send no signal).
pub async fn run<S: SessionSource>(
    client: &Client,
    events: &mut Events,
    source: &mut S,
    device: &str,
    name: &str,
    poll: Duration,
) -> String {
    macro_rules! tryq {
        ($e:expr) => {
            if let Err(e) = $e {
                return e.to_string();
            }
        };
    }
    tryq!(client.register(device_spec(device, name)).await);
    tryq!(client.update(device, CAP_ONLINE, Value::Bool(true)).await);
    let mut last: Option<bool> = None;
    let mut last_error = String::new();
    loop {
        match source.sessions().await {
            Ok(sessions) => {
                last_error.clear();
                let now = signed_in(&sessions);
                if last != Some(now) {
                    tryq!(client.update(device, CAP_SIGNED_IN, Value::Bool(now)).await);
                    println!("signed in: {now}");
                    last = Some(now);
                }
            }
            Err(e) => {
                if e != last_error {
                    eprintln!("cannot read the sessions from logind (keeping the last value, will retry): {e}");
                    last_error = e;
                }
            }
        }
        tokio::select! {
            () = sleep(poll) => {}
            () = source.changed() => {}
            event = events.next() => match event {
                Some(Event::Set(request)) => request.refuse("this device is read-only").await,
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

    fn s(class: &str, kind: &str, state: &str, remote: bool) -> SessionInfo {
        SessionInfo { class: class.into(), kind: kind.into(), state: state.into(), remote }
    }

    #[test]
    fn a_local_graphical_user_session_counts() {
        assert!(signed_in(&[s("user", "wayland", "active", false)]));
        assert!(signed_in(&[s("user", "x11", "online", false)]), "online (not in front) counts");
        assert!(signed_in(&[s("user", "mir", "active", false)]));
    }

    #[test]
    fn everything_else_does_not() {
        assert!(!signed_in(&[]));
        assert!(!signed_in(&[s("user", "tty", "active", false)]), "a text console");
        assert!(!signed_in(&[s("user", "wayland", "active", true)]), "remote");
        assert!(!signed_in(&[s("greeter", "wayland", "active", false)]), "the login screen");
        assert!(!signed_in(&[s("manager", "unspecified", "active", false)]), "the per-user manager");
        assert!(!signed_in(&[s("user", "wayland", "closing", false)]), "being closed");
        assert!(!signed_in(&[s("lock-screen", "wayland", "active", false)]));
    }

    #[test]
    fn one_qualifying_session_among_others_is_enough() {
        assert!(signed_in(&[s("manager", "unspecified", "active", false), s("user", "wayland", "active", false)]));
    }

    #[test]
    fn the_device_is_two_read_only_sensors_and_maps_to_matter() {
        let d = device_spec("office-pc", "Office PC");
        assert!(d.validate().is_ok());
        assert!(d.capabilities.iter().all(|c| !c.writable && c.kind == CapabilityKind::Boolean));
        assert_eq!(d.capabilities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), [CAP_ONLINE, CAP_SIGNED_IN]);
    }

    fn args(a: &[&str]) -> Result<Parsed, String> {
        parse_args(a.iter().map(|x| x.to_string()))
    }

    #[test]
    fn arguments_parse() {
        let Ok(Parsed::Run(a)) = args(&["--device", "pc", "--name", "My PC", "--poll-ms", "250"]) else { panic!() };
        assert_eq!(
            (a.device.as_deref(), a.name.as_deref(), a.poll),
            (Some("pc"), Some("My PC"), Duration::from_millis(250))
        );
        let Ok(Parsed::Run(a)) = args(&[]) else { panic!() };
        assert_eq!((a.device, a.name, a.poll), (None, None, DEFAULT_POLL));
        assert_eq!(a.remote, RemoteOptions::default());
        assert!(matches!(args(&["--help"]), Ok(Parsed::Help)) && matches!(args(&["-V"]), Ok(Parsed::Version)));
    }

    #[test]
    fn the_remote_agent_flags_are_parsed_and_validated() {
        let Ok(Parsed::Run(a)) =
            args(&["--connect", "192.168.1.5:7550", "--token-file", "/t", "--plugin-id", "desktop"])
        else {
            panic!()
        };
        assert_eq!(a.remote.connect.as_deref(), Some("192.168.1.5:7550"));
        assert_eq!(a.remote.token_file, Some(std::path::PathBuf::from("/t")));
        assert_eq!(a.remote.plugin_id.as_deref(), Some("desktop"));
        assert!(args(&["--connect", "nohost"]).is_err());
        assert!(args(&["--plugin-id", "Bad Id"]).is_err());
        assert!(args(&["--connect"]).is_err());
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(args(&["--device"]).is_err());
        assert!(args(&["--device", "Not Valid"]).is_err());
        assert!(args(&["--poll-ms", "x"]).is_err());
        assert!(args(&["--poll-ms", "5"]).is_err());
        assert!(args(&["--frobnicate"]).is_err());
    }

    #[test]
    fn every_cli_item_is_accepted() {
        for item in CLI_ITEMS {
            let a: Vec<&str> = if matches!(
                *item,
                "--device" | "--name" | "--poll-ms" | "--connect" | "--token-file" | "--plugin-id"
            ) {
                vec![
                    item,
                    match *item {
                        "--poll-ms" => "1000",
                        "--connect" => "h:1",
                        "--plugin-id" => "p",
                        _ => "x",
                    },
                ]
            } else {
                vec![item]
            };
            assert!(args(&a).is_ok(), "{item} is not accepted");
        }
    }

    /// Every flag must have a row in the plugin's documentation page (see AGENTS.md).
    #[test]
    fn the_documentation_covers_every_flag() {
        let page = include_str!("../../../../docs/plugins/linux-system.md");
        for item in CLI_ITEMS {
            assert!(
                page.lines()
                    .any(|l| l.starts_with("| `")
                        && l.split('|').nth(1).is_some_and(|c| c.contains(&format!("`{item}")))),
                "docs/plugins/linux-system.md has no table row for `{item}`"
            );
        }
    }
}
