//! The tokens of remote agents (`plugins[].connect.token_file`): reading them, and what to warn about.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use it_matters_model::auth::read_token_file;

use crate::config::Config;

/// What loading the token files found.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Plugin id -> token bytes, for the `connect` plugins that have a `token_file`.
    pub tokens: BTreeMap<String, Vec<u8>>,
    /// Things worth telling the user that do not stop the daemon.
    pub warnings: Vec<String>,
}

/// Reads every `connect` plugin's token file. All problems are returned together. Token contents are never
/// put in a message.
pub fn load(cfg: &Config) -> Result<Loaded, Vec<String>> {
    let mut out = Loaded::default();
    let mut errors = Vec::new();
    for (i, p) in cfg.plugins.iter().enumerate() {
        let Some(c) = &p.connect else { continue };
        let at = format!("plugins[{i}] ({:?})", p.id);
        let loopback = c.listen.parse::<SocketAddr>().is_ok_and(|a| a.ip().is_loopback());
        match &c.token_file {
            Some(path) => match read_token_file(path) {
                Ok(file) => {
                    if file.too_open {
                        out.warnings.push(format!(
                            "{at}: {} can be read by other users; make it readable by the daemon's user only (chmod 600)",
                            path.display()
                        ));
                    }
                    out.tokens.insert(p.id.clone(), file.token.into_bytes());
                }
                Err(e) => errors.push(format!("{at}: {e}")),
            },
            None if loopback => out.warnings.push(format!(
                "{at}: no token_file, so any program on this machine can connect as this plugin through {}; set one",
                c.listen
            )),
            None => {} // rejected by validate()
        }
        if !loopback {
            out.warnings.push(format!(
                "{at}: {} is reachable from the network and the connection is not encrypted; use a trusted LAN or a VPN/SSH tunnel",
                c.listen
            ));
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    fn token_file(name: &str, content: &str, mode: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("it-matters-tokens-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t");
        std::fs::write(&p, content).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn cfg(listen: &str, token: Option<&PathBuf>) -> Config {
        let t = token.map_or(String::new(), |p| format!(", token_file: {}", p.display()));
        Config::from_yaml(&format!("plugins: [{{ id: agent, connect: {{ listen: '{listen}'{t} }} }}]")).unwrap()
    }

    const GOOD: &str = "0123456789abcdef0123456789abcdef\n";

    #[test]
    fn a_good_token_is_loaded_and_trimmed() {
        let p = token_file("good", GOOD, 0o600);
        let l = load(&cfg("192.168.1.2:7550", Some(&p))).unwrap();
        assert_eq!(l.tokens["agent"], b"0123456789abcdef0123456789abcdef");
        assert!(l.warnings.iter().all(|w| w.contains("not encrypted")), "{:?}", l.warnings);
    }

    #[test]
    fn problems_are_reported_for_every_plugin_and_never_show_the_token() {
        let short = token_file("short", "hunter2", 0o600);
        let cfg = Config::from_yaml(&format!(
            "plugins:\n  - {{ id: a, connect: {{ listen: '127.0.0.1:1', token_file: {} }} }}\n  - {{ id: b, connect: {{ listen: '127.0.0.1:2', token_file: /no/such/file }} }}\n",
            short.display()
        ))
        .unwrap();
        let errors = load(&cfg).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("too short") && errors[0].contains("\"a\""), "{errors:?}");
        assert!(errors[1].contains("/no/such/file"), "{errors:?}");
        assert!(errors.iter().all(|e| !e.contains("hunter2")));
    }

    #[test]
    fn a_world_readable_token_file_is_a_warning_not_an_error() {
        let p = token_file("open", GOOD, 0o644);
        let l = load(&cfg("127.0.0.1:7550", Some(&p))).unwrap();
        assert!(
            l.warnings.iter().any(|w| w.contains("can be read by other users") && w.contains("chmod 600")),
            "{:?}",
            l.warnings
        );
    }

    #[test]
    fn a_loopback_listener_without_a_token_is_allowed_with_a_warning() {
        let l = load(&cfg("127.0.0.1:7550", None)).unwrap();
        assert!(l.tokens.is_empty());
        assert!(l.warnings.iter().any(|w| w.contains("any program on this machine")), "{:?}", l.warnings);
    }

    #[test]
    fn a_network_listener_always_gets_the_encryption_warning() {
        let p = token_file("net", GOOD, 0o600);
        let l = load(&cfg("0.0.0.0:7550", Some(&p))).unwrap();
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("not encrypted"));
    }

    #[test]
    fn run_plugins_and_configs_without_agents_have_nothing_to_load() {
        let c = Config::from_yaml("plugins: [{ id: a, run: { command: x } }]").unwrap();
        let l = load(&c).unwrap();
        assert!(l.tokens.is_empty() && l.warnings.is_empty());
    }
}
