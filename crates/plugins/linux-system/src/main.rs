//! `it-matters-plugin-linux-system`: reports whether the computer is online and a user is signed in.

use std::process::ExitCode;

use it_matters_plugin_linux_system::logind::Logind;
use it_matters_plugin_linux_system::{Parsed, parse_args, run};
use it_matters_plugin_sdk::{Connector, Target, sanitize_id};

const USAGE: &str = "\
it-matters-plugin-linux-system: reports whether this computer is online and whether a user is signed in

USAGE:
    it-matters-plugin-linux-system [--device <id>] [--name <name>] [--poll-ms <ms>]
                                   [--connect <host:port> --plugin-id <id> [--token-file <file>]]

OPTIONS:
    --device <id>     device id (default: the host name, made into a valid id)
    --name <name>     name shown in controllers (default: the host name)
    --poll-ms <ms>    how often to re-read the sessions, at least 100 (default 5000)
    --connect <addr>  run as a remote agent: connect to the bridge's TCP listener for this agent
    --plugin-id <id>  with --connect: the plugin id the bridge configured for this agent
    --token-file <f>  with --connect: file holding the agent's token (see the bridge's token_file)
    -h, --help        print this text
    -V, --version     print the version

Meant to be started by the it-matters daemon (a `run` plugin), which sets IT_MATTERS_SOCKET and
IT_MATTERS_PLUGIN_ID; or, with --connect, run on another machine as a remote agent.";

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

    let mut logind = match Logind::connect().await {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let target = match args.remote.into_target(|k| std::env::var(k).ok()) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let remote = target.reconnects();
    if let Target::Remote { addr, plugin_id, .. } = &target {
        println!("running as a remote agent for plugin {plugin_id:?} of the bridge at {addr}");
    }
    let mut connector = Connector::new(target, env!("CARGO_PKG_VERSION"));
    loop {
        match connector.connect().await {
            Ok((client, mut events)) => {
                println!("connected as plugin {:?}; device {device:?} ({name})", connector.target().plugin_id());
                connector.reset();
                let reason = run(&client, &mut events, &mut logind, &device, &name, args.poll).await;
                eprintln!("it-matters-plugin-linux-system: disconnected: {reason}");
            }
            Err(e) => eprintln!("it-matters-plugin-linux-system: {e}"),
        }
        // A plugin the daemon started just exits: the daemon restarts it. A remote agent has nobody to do
        // that, so it reconnects (with a growing delay) forever.
        if !remote {
            return ExitCode::from(1);
        }
        connector.wait().await;
    }
}
