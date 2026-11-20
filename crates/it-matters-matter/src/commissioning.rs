//! The setup passcode and discriminator controllers use to commission the bridge.
//!
//! Values set in the configuration always win. Anything not configured is generated randomly on
//! first start and kept in `<storage_dir>/commissioning.json` (readable by the owner only), so the
//! pairing code stays the same across restarts but is never a well-known value.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// What the Matter stack is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commissioning {
    pub passcode: u32,
    pub discriminator: u16,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    passcode: Option<u32>,
    #[serde(default)]
    discriminator: Option<u16>,
}

/// Matter setup passcodes are 1..=99999998 and must not be one of the trivially guessable values.
pub fn valid_passcode(p: u32) -> bool {
    const FORBIDDEN: [u32; 12] = [
        0, 11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888, 99999999, 12345678, 87654321,
    ];
    p <= 99_999_998 && !FORBIDDEN.contains(&p)
}

fn generate_passcode(rng: &mut impl FnMut() -> u32) -> u32 {
    loop {
        let p = rng() % 100_000_000;
        if valid_passcode(p) {
            return p;
        }
    }
}

/// Chooses the commissioning codes; see the module docs. `rng` yields random `u32`s.
pub fn resolve(
    storage_dir: &Path,
    configured_passcode: Option<u32>,
    configured_discriminator: Option<u16>,
    mut rng: impl FnMut() -> u32,
) -> Result<Commissioning, String> {
    let file = storage_dir.join("commissioning.json");
    let mut stored: Stored = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::default(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    // A stored value that is no longer acceptable is replaced rather than trusted.
    stored.passcode = stored.passcode.filter(|p| valid_passcode(*p));
    stored.discriminator = stored.discriminator.filter(|d| *d <= 0x0FFF);

    let mut generated = false;
    let passcode = match configured_passcode.or(stored.passcode) {
        Some(p) => p,
        None => {
            generated = true;
            let p = generate_passcode(&mut rng);
            stored.passcode = Some(p);
            p
        }
    };
    let discriminator = match configured_discriminator.or(stored.discriminator) {
        Some(d) => d,
        None => {
            generated = true;
            let d = (rng() & 0x0FFF) as u16;
            stored.discriminator = Some(d);
            d
        }
    };
    if generated {
        save(&file, &stored)?;
    }
    Ok(Commissioning { passcode, discriminator })
}

fn save(file: &Path, stored: &Stored) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let err = |e: std::io::Error| format!("{}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    let tmp = file.with_extension("tmp");
    let text = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())?;
    let mut f =
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(err)?;
    f.write_all(text.as_bytes()).map_err(err)?;
    f.sync_all().map_err(err)?;
    std::fs::rename(&tmp, file).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("it-matters-comm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn seq(values: Vec<u32>) -> impl FnMut() -> u32 {
        let mut it = values.into_iter();
        move || it.next().expect("rng exhausted")
    }

    #[test]
    fn first_start_generates_valid_codes_and_keeps_them() {
        let d = dir("first");
        let a = resolve(&d, None, None, seq(vec![12345678, 31415926, 0x1ABC])).unwrap();
        assert_eq!(a, Commissioning { passcode: 31415926, discriminator: 0xABC });
        // The trivially guessable draw (12345678) was skipped; a second start reuses the stored codes.
        let b = resolve(&d, None, None, seq(vec![])).unwrap();
        assert_eq!(a, b);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_stored_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("perm");
        resolve(&d, None, None, seq(vec![20202020, 5])).unwrap();
        let mode = std::fs::metadata(d.join("commissioning.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn configured_values_win_and_are_not_stored() {
        let d = dir("conf");
        let c = resolve(&d, Some(20202021), Some(3840), seq(vec![])).unwrap();
        assert_eq!(c, Commissioning { passcode: 20202021, discriminator: 3840 });
        assert!(!d.join("commissioning.json").exists(), "nothing was generated, so nothing is written");
    }

    #[test]
    fn a_partly_configured_setup_generates_only_the_rest() {
        let d = dir("part");
        let c = resolve(&d, Some(20202021), None, seq(vec![0x0123])).unwrap();
        assert_eq!((c.passcode, c.discriminator), (20202021, 0x123));
        // The configured passcode is not persisted: removing it from the config brings back a generated one.
        let c2 = resolve(&d, None, None, seq(vec![40404040])).unwrap();
        assert_eq!((c2.passcode, c2.discriminator), (40404040, 0x123));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn config_overrides_what_was_stored() {
        let d = dir("override");
        resolve(&d, None, None, seq(vec![40404040, 7])).unwrap();
        let c = resolve(&d, Some(20202021), Some(9), seq(vec![])).unwrap();
        assert_eq!((c.passcode, c.discriminator), (20202021, 9));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unacceptable_stored_values_are_replaced_and_corrupt_files_are_errors() {
        let d = dir("bad");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("commissioning.json"), r#"{"passcode":11111111,"discriminator":5000}"#).unwrap();
        let c = resolve(&d, None, None, seq(vec![30303030, 0x0ABC])).unwrap();
        assert_eq!((c.passcode, c.discriminator), (30303030, 0xABC));
        std::fs::write(d.join("commissioning.json"), "garbage").unwrap();
        assert!(resolve(&d, None, None, seq(vec![])).unwrap_err().contains("commissioning.json"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn passcode_validity() {
        assert!(valid_passcode(20202021) && valid_passcode(1) && valid_passcode(99_999_998));
        assert!(
            !valid_passcode(0) && !valid_passcode(99_999_999) && !valid_passcode(12345678) && !valid_passcode(87654321)
        );
        assert!(!valid_passcode(11111111) && !valid_passcode(100_000_000));
    }
}
