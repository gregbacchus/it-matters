//! it-matters: a lightweight Matter bridge that hosts plugins.

mod config;

use std::path::PathBuf;
use std::process::ExitCode;

use config::Config;

const USAGE: &str = "\
it-matters: a lightweight Matter bridge that hosts plugins

USAGE:
    it-matters [--config <file>] [run]     run the bridge (default)
    it-matters [--config <file>] check     validate the configuration and exit
    it-matters --help | --version

The config file is, in order: --config, $XDG_CONFIG_HOME/it-matters/config.yaml (or
~/.config/it-matters/config.yaml), /etc/it-matters/config.yaml. With no file, defaults apply.";

/// Every command and flag the CLI accepts. `docs/usage.md` must document each one (a test checks it),
/// and `parse_args` must accept each one.
#[cfg(test)]
const CLI_ITEMS: &[&str] = &["run", "check", "--config", "-c", "--help", "-h", "--version", "-V"];

#[derive(Debug, PartialEq)]
enum Command {
    Run,
    Check,
    Help,
    Version,
}

#[derive(Debug, PartialEq)]
struct Cli {
    command: Command,
    config: Option<PathBuf>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli { command: Command::Run, config: None };
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "run" => cli.command = Command::Run,
            "check" => cli.command = Command::Check,
            "-h" | "--help" => cli.command = Command::Help,
            "-V" | "--version" => cli.command = Command::Version,
            "-c" | "--config" => {
                cli.config = Some(PathBuf::from(args.next().ok_or("--config needs a file name")?));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(cli)
}

/// Loads the configuration: an explicit file must exist; otherwise the first existing search path
/// wins, and no file at all means the defaults. Returns the config and where it came from.
fn load(explicit: Option<PathBuf>) -> Result<(Config, Option<PathBuf>), String> {
    let explicit_given = explicit.is_some();
    let candidates = match explicit {
        Some(p) => vec![p],
        None => config::config_search_paths(
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        ),
    };
    for path in candidates {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let cfg = Config::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok((cfg, Some(path)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit_given => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok((Config::default(), None))
}

fn main() -> ExitCode {
    let cli = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("it-matters: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match cli.command {
        Command::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("it-matters {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Run | Command::Check => {}
    }
    let (cfg, source) = match load(cli.config) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("it-matters: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(problems) = cfg.validate() {
        eprintln!("it-matters: invalid configuration:");
        for p in problems {
            eprintln!("  - {p}");
        }
        return ExitCode::from(1);
    }
    let from = source.map_or("built-in defaults".to_owned(), |p| p.display().to_string());
    match cli.command {
        Command::Check => {
            println!("configuration OK ({from}): {} plugin(s)", cfg.plugins.len());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("it-matters: the Matter bridge is not implemented yet (configuration OK, {from})");
            ExitCode::from(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Cli, String> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_means_run() {
        assert_eq!(args(&[]).unwrap(), Cli { command: Command::Run, config: None });
    }

    #[test]
    fn parses_commands_and_config() {
        assert_eq!(args(&["check"]).unwrap().command, Command::Check);
        assert_eq!(args(&["--config", "/x.yaml", "check"]).unwrap().config, Some(PathBuf::from("/x.yaml")));
        assert_eq!(args(&["-V"]).unwrap().command, Command::Version);
    }

    #[test]
    fn rejects_unknown_arguments_and_a_dangling_config_flag() {
        assert!(args(&["frobnicate"]).is_err());
        assert!(args(&["--config"]).is_err());
    }

    #[test]
    fn every_cli_item_is_accepted_and_in_the_usage_text() {
        for item in CLI_ITEMS {
            let mut a = vec![item.to_string()];
            if matches!(*item, "--config" | "-c") {
                a.push("x.yaml".into());
            }
            assert!(parse_args(a).is_ok(), "{item} is not accepted by parse_args");
            if item.starts_with("--") || matches!(*item, "run" | "check") {
                assert!(USAGE.contains(item), "{item} is missing from the usage text");
            }
        }
    }

    #[test]
    fn usage_guide_covers_every_cli_item() {
        let guide = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/usage.md")).unwrap();
        for item in CLI_ITEMS {
            assert!(
                guide
                    .lines()
                    .any(|l| l.starts_with("| `")
                        && l.split('|').nth(1).is_some_and(|c| c.contains(&format!("`{item}")))),
                "docs/usage.md has no table row for `{item}`: document every command and flag"
            );
        }
    }
}
