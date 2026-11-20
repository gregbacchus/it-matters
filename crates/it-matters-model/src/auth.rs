//! Authenticating remote agents.
//!
//! A remote (TCP) agent shares a **token** with the bridge. The token never crosses the network:
//! the host sends a fresh random nonce (`challenge`) and the agent answers, in its `hello`, with a
//! **proof**, an HMAC-SHA256 keyed by the token over a version tag, the nonce and the plugin id:
//!
//! ```text
//! proof = hex( HMAC-SHA256( key = token bytes,
//!                           msg = "it-matters-agent-v1" 0x00 nonce 0x00 plugin_id ) )
//! ```
//!
//! The nonce makes a recorded proof useless on another connection, and the plugin id binds the proof to
//! one plugin. This protects the secret, **not the session**: nothing is encrypted or integrity-protected,
//! so someone who can intercept a live connection can still read or hijack it.

use std::io::Read;
use std::path::Path;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The shortest token accepted (characters): 32 hex characters carry 128 bits.
pub const MIN_TOKEN_LEN: usize = 32;
/// The longest token accepted.
pub const MAX_TOKEN_LEN: usize = 1024;

const TAG: &[u8] = b"it-matters-agent-v1";

fn mac(token: &[u8], nonce: &str, plugin: &str) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(token).expect("HMAC accepts keys of any length");
    m.update(TAG);
    m.update(&[0]);
    m.update(nonce.as_bytes());
    m.update(&[0]);
    m.update(plugin.as_bytes());
    m
}

/// The proof an agent sends for this challenge (lower-case hex).
pub fn proof(token: &[u8], nonce: &str, plugin: &str) -> String {
    hex(&mac(token, nonce, plugin).finalize().into_bytes())
}

/// Checks a proof in constant time. Anything that is not exactly the expected hex is rejected.
pub fn verify(token: &[u8], nonce: &str, plugin: &str, proof_hex: &str) -> bool {
    match unhex(proof_hex) {
        Some(bytes) => mac(token, nonce, plugin).verify_slice(&bytes).is_ok(),
        None => false,
    }
}

/// A fresh nonce: 16 random bytes from the operating system, as 32 hex characters.
pub fn new_nonce() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

/// What reading a token file found.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenFile {
    pub token: String,
    /// Set when other users can read the file (group or world permission bits).
    pub too_open: bool,
}

/// Reads a token file: UTF-8, the first line (trailing whitespace removed), at least [`MIN_TOKEN_LEN`] and
/// at most [`MAX_TOKEN_LEN`] characters. Never includes the token in an error message.
pub fn read_token_file(path: &Path) -> Result<TokenFile, String> {
    use std::os::unix::fs::PermissionsExt;
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    let token = text.lines().next().unwrap_or("").trim().to_owned();
    if token.is_empty() {
        return Err(format!("{shown}: the token file is empty"));
    }
    let len = token.chars().count();
    if len < MIN_TOKEN_LEN {
        return Err(format!(
            "{shown}: the token is too short ({len} characters; at least {MIN_TOKEN_LEN} are required). \
             Generate one with: openssl rand -hex 32"
        ));
    }
    if len > MAX_TOKEN_LEN {
        return Err(format!("{shown}: the token is too long (at most {MAX_TOKEN_LEN} characters)"));
    }
    let mode = std::fs::metadata(path).map_err(|e| format!("{shown}: {e}"))?.permissions().mode();
    Ok(TokenFile { token, too_open: mode & 0o077 != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &[u8] = b"0123456789abcdef0123456789abcdef";
    const NONCE: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn the_proof_matches_an_independent_implementation() {
        // Computed with Python's hmac module: hmac.new(token, b"it-matters-agent-v1\0" + nonce + b"\0" + plugin, sha256).
        assert_eq!(
            proof(TOKEN, NONCE, "desktop-agent"),
            "feb5c3b891baf76cef208f5821094b670d42662d044181dd9e4637ef1514de87"
        );
    }

    #[test]
    fn a_correct_proof_verifies() {
        let p = proof(TOKEN, NONCE, "desktop-agent");
        assert!(verify(TOKEN, NONCE, "desktop-agent", &p));
    }

    #[test]
    fn the_proof_is_bound_to_the_token_nonce_and_plugin() {
        let p = proof(TOKEN, NONCE, "desktop-agent");
        assert!(!verify(b"another-token-another-token-xxxxx", NONCE, "desktop-agent", &p), "wrong token");
        assert!(!verify(TOKEN, "ffeeddccbbaa99887766554433221100", "desktop-agent", &p), "another connection's nonce");
        assert!(!verify(TOKEN, NONCE, "other-agent", &p), "another plugin id");
    }

    #[test]
    fn malformed_proofs_are_rejected() {
        let good = proof(TOKEN, NONCE, "desktop-agent");
        for bad in
            ["", "zz", &good[..good.len() - 2], &format!("{good}00"), &good.to_uppercase().replace('F', "G"), "feb5 c3"]
        {
            assert!(!verify(TOKEN, NONCE, "desktop-agent", bad), "{bad:?}");
        }
        assert!(verify(TOKEN, NONCE, "desktop-agent", &good.to_uppercase()), "hex case does not matter");
    }

    #[test]
    fn nonces_are_32_hex_characters_and_differ() {
        let a = new_nonce().unwrap();
        let b = new_nonce().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    fn file(name: &str, content: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("it-matters-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("agent.token");
        std::fs::write(&p, content).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn a_token_file_is_read_trimmed_and_checked_for_permissions() {
        let strong = "ab".repeat(32);
        let p = file("ok", &format!("{strong}\n"), 0o600);
        assert_eq!(read_token_file(&p).unwrap(), TokenFile { token: strong.clone(), too_open: false });
        let p = file("open", &format!("{strong}\r\n"), 0o644);
        assert_eq!(read_token_file(&p).unwrap(), TokenFile { token: strong, too_open: true });
    }

    #[test]
    fn bad_token_files_are_refused_without_revealing_the_token() {
        let secret = "s3cr3t-too-short";
        for (name, content, needle) in
            [("empty", "\n", "empty"), ("short", secret, "too short"), ("long", &"x".repeat(2000), "too long")]
        {
            let e = read_token_file(&file(name, content, 0o600)).unwrap_err();
            assert!(e.contains(needle), "{e}");
            assert!(!e.contains(secret), "the token must never appear in an error");
        }
        assert!(read_token_file(Path::new("/nonexistent/agent.token")).unwrap_err().contains("/nonexistent"));
    }
}
