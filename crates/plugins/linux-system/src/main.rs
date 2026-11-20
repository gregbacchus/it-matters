//! `it-matters-plugin-linux-system`: reports whether the computer is online and a user is signed in.

use std::process::ExitCode;

use it_matters_plugin_linux_system::logind::Logind;
use it_matters_plugin_linux_system::{Parsed, parse_args, run};
use it_matters_plugin_sdk::{Env, connect, sanitize_id};

const USAGE: &str = "\
it-matters-plugin-linux-system: reports whether this computer is online and whether a user is signed in

USAGE:
    it-matters-plugin-linux-system [--device <id>] [--name <name>] [--poll-ms <ms>]

OPTIONS:
    --device <id>     device id (default: the host name, made into a valid id)
    --name <name>     name shown in controllers (default: the host name)
    --poll-ms <ms>    how often to re-read the sessions, at least 100 (default 5000)
    -h, --help        print this text
    -V, --version     print the version

Meant to be started by the it-matters daemon (a `run` plugin), which sets IT_MATTERS_SOCKET and
IT_MATTERS_PLUGIN_ID.";

fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("it-matters-plugin-linux-system: {message}");
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
            println!("it-matters-plugin-linux-system {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("it-matters-plugin-linux-system: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let host = host_name();
    let device = args.device.unwrap_or_else(|| sanitize_id(&host, "computer"));
    let name = args.name.unwrap_or_else(|| if host.is_empty() { "Computer".into() } else { host });

    let env = match Env::from_env() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    let mut logind = match Logind::connect().await {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let (client, mut events) = match connect(&env.socket, &env.plugin_id, env!("CARGO_PKG_VERSION")).await {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    println!("connected as plugin {:?}; device {device:?} ({name})", env.plugin_id);
    fail(run(&client, &mut events, &mut logind, &device, &name, args.poll).await)
}
