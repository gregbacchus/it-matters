//! The daemon configuration (`config.yaml`).
//!
//! Every field is optional; missing fields fall back to defaults. Unknown fields are ignored in
//! release builds so older and newer files keep loading, and rejected in test builds so the
//! example and the docs cannot hide typos (see AGENTS.md).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use it_matters_model::valid_id;
use serde::Deserialize;

pub const DEFAULT_PLUGIN_TIMEOUT_MS: u64 = 2000;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct Config {
    pub watch: bool,
    pub bridge: BridgeConfig,
    pub matter: MatterConfig,
    pub ipc: IpcConfig,
    pub plugins: Vec<PluginConfig>,
    pub devices: BTreeMap<String, DeviceOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watch: true,
            bridge: BridgeConfig::default(),
            matter: MatterConfig::default(),
            ipc: IpcConfig::default(),
            plugins: Vec::new(),
            devices: BTreeMap::new(),
        }
    }
}

/// How the bridge presents itself to controllers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct BridgeConfig {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        // 0xFFF1 is the Matter test vendor id; fine for home use, not for certified products.
        Self { name: "it-matters".into(), vendor_id: 0xFFF1, product_id: 0x8000 }
    }
}

/// The Matter side: network port, persistence and commissioning codes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct MatterConfig {
    pub port: u16,
    pub storage_dir: PathBuf,
    /// Setup passcode; `None` means the stack's default / a generated one.
    pub passcode: Option<u32>,
    /// Long discriminator (12 bits); `None` means the stack's default.
    pub discriminator: Option<u16>,
}

impl Default for MatterConfig {
    fn default() -> Self {
        Self { port: 5540, storage_dir: PathBuf::from("/var/lib/it-matters"), passcode: None, discriminator: None }
    }
}

/// The local plugin socket.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct IpcConfig {
    /// Unix domain socket that locally running plugins connect to.
    pub socket: PathBuf,
}

impl Default for IpcConfig {
    fn default() -> Self {
        Self { socket: PathBuf::from("/run/it-matters/plugins.sock") }
    }
}

/// One plugin: either a process the daemon starts (`run`) or a remote agent that connects (`connect`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct PluginConfig {
    pub id: String,
    pub run: Option<RunConfig>,
    pub connect: Option<ConnectConfig>,
}

/// A plugin process started and supervised by the daemon.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct RunConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Hard timeout for the plugin's handshake and for each `set` it is asked to perform.
    pub timeout_ms: u64,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self { command: String::new(), args: Vec::new(), timeout_ms: DEFAULT_PLUGIN_TIMEOUT_MS }
    }
}

/// A TCP listener that remote plugin agents connect to.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct ConnectConfig {
    /// `host:port` to listen on, e.g. `0.0.0.0:7550`.
    pub listen: String,
    /// File holding the shared secret agents must present. Required unless `listen` is loopback.
    pub token_file: Option<PathBuf>,
}

/// Per-device overrides, keyed by device id.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
#[cfg_attr(test, serde(deny_unknown_fields))]
pub struct DeviceOverride {
    pub name: Option<String>,
}

impl Config {
    /// Parses YAML. An empty (or comment-only) file is the default configuration.
    pub fn from_yaml(text: &str) -> Result<Config, String> {
        if text.lines().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')) {
            return Ok(Config::default());
        }
        serde_yaml_ng::from_str(text).map_err(|e| e.to_string())
    }

    /// Semantic checks that the types cannot express. Returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.bridge.name.trim().is_empty() {
            errors.push("bridge.name must not be empty".into());
        }
        if let Some(d) = self.matter.discriminator
            && d > 0x0FFF
        {
            errors.push(format!("matter.discriminator {d} is out of range (0-4095)"));
        }
        if let Some(p) = self.matter.passcode
            && !valid_passcode(p)
        {
            errors.push(format!("matter.passcode {p} is not a valid Matter setup passcode"));
        }
        let mut ids = std::collections::HashSet::new();
        let mut listens = std::collections::HashSet::new();
        for (i, p) in self.plugins.iter().enumerate() {
            let at = format!("plugins[{i}]");
            if !valid_id(&p.id) {
                errors.push(format!("{at}.id {:?} must be 1-64 characters of a-z, 0-9, '-' or '_'", p.id));
            } else if !ids.insert(p.id.as_str()) {
                errors.push(format!("{at}.id {:?} is used by more than one plugin", p.id));
            }
            match (&p.run, &p.connect) {
                (None, None) => errors.push(format!("{at} needs either `run` or `connect`")),
                (Some(_), Some(_)) => errors.push(format!("{at} has both `run` and `connect`; use one")),
                (Some(r), None) => {
                    if r.command.trim().is_empty() {
                        errors.push(format!("{at}.run.command must not be empty"));
                    }
                    if r.timeout_ms == 0 {
                        errors.push(format!("{at}.run.timeout_ms must be greater than 0"));
                    }
                }
                (None, Some(c)) => match c.listen.parse::<SocketAddr>() {
                    Err(_) => errors.push(format!("{at}.connect.listen {:?} is not a host:port address", c.listen)),
                    Ok(addr) => {
                        if !listens.insert(addr) {
                            errors.push(format!("{at}.connect.listen {addr} is already used by another plugin"));
                        }
                        if !addr.ip().is_loopback() && c.token_file.is_none() {
                            errors.push(format!(
                                "{at}.connect.token_file is required when listening on a non-loopback address ({addr})"
                            ));
                        }
                    }
                },
            }
        }
        for (id, d) in &self.devices {
            if !valid_id(id) {
                errors.push(format!("devices.{id:?}: not a valid device id"));
            }
            if d.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
                errors.push(format!("devices.{id}.name must not be empty"));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

/// Matter setup passcodes are 1..=99999998 and must not be one of the trivially guessable values.
pub fn valid_passcode(p: u32) -> bool {
    const FORBIDDEN: [u32; 12] = [
        0, 11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888, 99999999, 12345678, 87654321,
    ];
    p <= 99_999_998 && !FORBIDDEN.contains(&p)
}

/// Where to look for the config file, most specific first. Pure so it can be tested.
pub fn config_search_paths(xdg_config_home: Option<&str>, home: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match (xdg_config_home.filter(|s| !s.is_empty()), home.filter(|s| !s.is_empty())) {
        (Some(x), _) => out.push(PathBuf::from(x).join("it-matters/config.yaml")),
        (None, Some(h)) => out.push(PathBuf::from(h).join(".config/it-matters/config.yaml")),
        (None, None) => {}
    }
    out.push(PathBuf::from("/etc/it-matters/config.yaml"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> Config {
        Config::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn empty_and_comment_only_files_are_the_defaults() {
        assert_eq!(cfg(""), Config::default());
        assert_eq!(cfg("# nothing here\n\n"), Config::default());
        assert!(Config::default().validate().is_ok());
        assert!(Config::default().watch);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let c = cfg("matter: { port: 5541 }\n");
        assert_eq!(c.matter.port, 5541);
        assert_eq!(c.matter.storage_dir, PathBuf::from("/var/lib/it-matters"));
        assert_eq!(c.bridge, BridgeConfig::default());
    }

    #[test]
    fn hex_ids_and_plugins_parse() {
        let c = cfg(
            "bridge: { vendor_id: 0xFFF1, product_id: 0x8001 }\nplugins:\n  - id: a\n    run: { command: /bin/true }\n",
        );
        assert_eq!(c.bridge.product_id, 0x8001);
        assert_eq!(c.plugins[0].run.as_ref().unwrap().timeout_ms, DEFAULT_PLUGIN_TIMEOUT_MS);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn typos_are_rejected_in_test_builds() {
        assert!(Config::from_yaml("bridg: {}\n").is_err());
        assert!(Config::from_yaml("matter: { prot: 1 }\n").is_err());
    }

    #[test]
    fn malformed_yaml_reports_an_error() {
        assert!(Config::from_yaml("matter: [1, 2\n").is_err());
        assert!(Config::from_yaml("matter: { port: lots }\n").is_err());
    }

    fn problems(yaml: &str) -> String {
        cfg(yaml).validate().unwrap_err().join("\n")
    }

    #[test]
    fn plugin_needs_exactly_one_of_run_and_connect() {
        assert!(problems("plugins: [{ id: a }]").contains("either `run` or `connect`"));
        let both = "plugins: [{ id: a, run: { command: x }, connect: { listen: '127.0.0.1:7550' } }]";
        assert!(problems(both).contains("both"));
    }

    #[test]
    fn plugin_ids_must_be_valid_and_unique() {
        let p = problems(
            "plugins:\n  - { id: A b, run: { command: x } }\n  - { id: ok, run: { command: x } }\n  - { id: ok, run: { command: y } }\n",
        );
        assert!(p.contains("plugins[0].id") && p.contains("more than one plugin"), "{p}");
    }

    #[test]
    fn run_command_and_timeout_are_checked() {
        let p = problems("plugins: [{ id: a, run: { command: '', timeout_ms: 0 } }]");
        assert!(p.contains("command must not be empty") && p.contains("timeout_ms"), "{p}");
    }

    #[test]
    fn remote_listeners_require_a_token_but_loopback_does_not() {
        assert!(cfg("plugins: [{ id: a, connect: { listen: '127.0.0.1:7550' } }]").validate().is_ok());
        let p = problems("plugins: [{ id: a, connect: { listen: '0.0.0.0:7550' } }]");
        assert!(p.contains("token_file is required"), "{p}");
        assert!(cfg("plugins: [{ id: a, connect: { listen: '0.0.0.0:7550', token_file: /t } }]").validate().is_ok());
    }

    #[test]
    fn listen_must_be_an_address_and_not_shared() {
        assert!(problems("plugins: [{ id: a, connect: { listen: nonsense } }]").contains("host:port"));
        let p = problems(
            "plugins:\n  - { id: a, connect: { listen: '127.0.0.1:1' } }\n  - { id: b, connect: { listen: '127.0.0.1:1' } }\n",
        );
        assert!(p.contains("already used"), "{p}");
    }

    #[test]
    fn matter_codes_are_range_checked() {
        assert!(problems("matter: { discriminator: 4096 }").contains("discriminator"));
        assert!(problems("matter: { passcode: 11111111 }").contains("passcode"));
        assert!(problems("matter: { passcode: 99999999 }").contains("passcode"));
        assert!(cfg("matter: { passcode: 20202021, discriminator: 4095 }").validate().is_ok());
    }

    #[test]
    fn device_overrides_are_checked() {
        assert!(problems("devices: { 'Bad Id': { name: x } }").contains("not a valid device id"));
        assert!(problems("devices: { ok: { name: ' ' } }").contains("name must not be empty"));
    }

    #[test]
    fn search_paths_prefer_xdg_then_home_then_etc() {
        let p = config_search_paths(Some("/x"), Some("/h"));
        assert_eq!(p, vec![PathBuf::from("/x/it-matters/config.yaml"), PathBuf::from("/etc/it-matters/config.yaml")]);
        let p = config_search_paths(Some(""), Some("/h"));
        assert_eq!(p[0], PathBuf::from("/h/.config/it-matters/config.yaml"));
        assert_eq!(config_search_paths(None, None), vec![PathBuf::from("/etc/it-matters/config.yaml")]);
    }

    // -----------------------------------------------------------------------------------------
    // The shipped example and the docs are part of the contract (see AGENTS.md).
    // -----------------------------------------------------------------------------------------

    /// The example loads with the real types, rejecting unknown keys, and is itself valid.
    #[test]
    fn the_example_config_is_valid() {
        let c = Config::from_yaml(include_str!("../../../examples/config.yaml"))
            .unwrap_or_else(|e| panic!("examples/config.yaml: {e}"));
        c.validate().unwrap_or_else(|e| panic!("examples/config.yaml is invalid: {e:#?}"));
        assert!(!c.plugins.is_empty() && !c.devices.is_empty());
    }

    /// The field names of a config struct, read from serde's "unknown field ..., expected one of
    /// ..." error (test builds reject unknown fields).
    fn field_names<T: serde::de::DeserializeOwned>() -> Vec<String> {
        let err = serde_yaml_ng::from_str::<T>("{ __nope__: 1 }")
            .err()
            .expect("config structs deny unknown fields in test builds")
            .to_string();
        let after = err.split("expected").nth(1).unwrap_or("");
        after.split('`').skip(1).step_by(2).map(str::to_owned).collect()
    }

    /// Every config option as a dotted path (`[]` = each list entry, `<id>` = each map key).
    /// When you add a config struct, add it here (see AGENTS.md).
    fn all_option_paths() -> Vec<String> {
        // The list entry and map entry themselves are documented too.
        let mut out = vec!["plugins[]".to_owned(), "devices.<id>".to_owned()];
        let mut add = |prefix: &str, fields: Vec<String>| {
            for f in fields {
                out.push(if prefix.is_empty() { f } else { format!("{prefix}.{f}") });
            }
        };
        add("", field_names::<Config>());
        add("bridge", field_names::<BridgeConfig>());
        add("matter", field_names::<MatterConfig>());
        add("ipc", field_names::<IpcConfig>());
        add("plugins[]", field_names::<PluginConfig>());
        add("plugins[].run", field_names::<RunConfig>());
        add("plugins[].connect", field_names::<ConnectConfig>());
        add("devices.<id>", field_names::<DeviceOverride>());
        out
    }

    fn docs_text() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        let mut files = vec![root.join("README.md")];
        for e in std::fs::read_dir(root.join("config")).expect("docs/config exists") {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "md") {
                files.push(p);
            }
        }
        files.iter().map(|f| std::fs::read_to_string(f).unwrap()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn docs_cover_every_config_option() {
        let docs = docs_text();
        let missing: Vec<String> =
            all_option_paths().into_iter().filter(|p| !docs.contains(&format!("| `{p}` |"))).collect();
        assert!(missing.is_empty(), "options missing from docs/config/*.md (one table row each): {missing:#?}");
    }

    #[test]
    fn docs_do_not_describe_options_that_do_not_exist() {
        let known: std::collections::HashSet<String> = all_option_paths().into_iter().collect();
        let roots = ["watch", "bridge", "matter", "ipc", "plugins", "devices"];
        let mut unknown = Vec::new();
        for line in docs_text().lines().filter(|l| l.starts_with("| `")) {
            let Some(path) = line[3..].split('`').next() else {
                continue;
            };
            let root = path.split(['.', '[']).next().unwrap_or("");
            if roots.contains(&root) && !known.contains(path) {
                unknown.push(path.to_owned());
            }
        }
        assert!(unknown.is_empty(), "documented options that don't exist (renamed or removed?): {unknown:#?}");
    }

    /// The `yaml` blocks in the README and docs are usage examples: each must be a valid
    /// configuration. Use a `yml` fence for YAML that is deliberately not a config.
    #[test]
    fn yaml_examples_in_docs_are_valid_configs() {
        fn markdown_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    markdown_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "md") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = vec![root.join("README.md")];
        markdown_files(&root.join("docs"), &mut files);
        let mut checked = 0;
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let mut block: Option<String> = None;
            for line in text.lines() {
                match (&mut block, line.trim_end()) {
                    (None, "```yaml") => block = Some(String::new()),
                    (Some(b), "```") => {
                        let cfg = Config::from_yaml(b)
                            .unwrap_or_else(|e| panic!("{}: invalid yaml example: {e}\n{b}", file.display()));
                        cfg.validate()
                            .unwrap_or_else(|e| panic!("{}: example fails validation: {e:#?}\n{b}", file.display()));
                        checked += 1;
                        block = None;
                    }
                    (Some(b), l) => {
                        b.push_str(l);
                        b.push('\n');
                    }
                    _ => {}
                }
            }
        }
        assert!(checked >= 5, "expected the docs to contain yaml examples, found {checked}");
    }
}
