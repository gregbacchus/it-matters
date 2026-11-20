//! `it-matters-plugin-linux-audio`: the speaker's volume and mute, and whether a microphone is in use.

use std::process::ExitCode;

use it_matters_plugin_linux_audio::backend::Wpctl;
use it_matters_plugin_linux_audio::exec::RealExec;
use it_matters_plugin_linux_audio::{Parsed, parse_args, run};
use it_matters_plugin_sdk::{Env, connect, sanitize_id};

const USAGE: &str = "\
it-matters-plugin-linux-audio: the speaker's volume and mute, and whether a microphone is in use

USAGE:
    it-matters-plugin-linux-audio [--device <id>] [--name <name>] [--sink <target>] [--poll-ms <ms>]

OPTIONS:
    --device <id>     device id (default: <host name>-audio)
    --name <name>     name shown in controllers (default: \"<host name> Audio\")
    --sink <target>   the speaker to control: @DEFAULT_AUDIO_SINK@ (default), a PipeWire node id, or a
                      node name such as alsa_output.pci-0000_c4_00.6.analog-stereo
    --poll-ms <ms>    how often to read the audio state, at least 100 (default 1000)
    -h, --help        print this text
    -V, --version     print the version

Needs PipeWire with WirePlumber (`wpctl`, `pw-dump`) and the desktop user's XDG_RUNTIME_DIR, so run it
as that user. Meant to be started by the it-matters daemon (a `run` plugin), which sets
IT_MATTERS_SOCKET and IT_MATTERS_PLUGIN_ID.";

fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("it-matters-plugin-linux-audio: {message}");
    ExitCode::from(1)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("it-matters-plugin-linux-audio {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("it-matters-plugin-linux-audio: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let host = host_name();
    let device = args.device.unwrap_or_else(|| format!("{}-audio", sanitize_id(&host, "computer")));
    let name =
        args.name.unwrap_or_else(|| if host.is_empty() { "Computer Audio".into() } else { format!("{host} Audio") });

    let env = match Env::from_env() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    let mut audio = Wpctl::new(RealExec::default(), args.sink);
    let (client, mut events) = match connect(&env.socket, &env.plugin_id, env!("CARGO_PKG_VERSION")).await {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    println!("connected as plugin {:?}; device {device:?} ({name})", env.plugin_id);
    fail(run(&client, &mut events, &mut audio, &device, &name, args.poll).await)
}
