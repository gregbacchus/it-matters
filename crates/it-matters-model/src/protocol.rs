//! The plugin wire protocol: one JSON object per line, in each direction.
//!
//! Frames are limited to [`MAX_FRAME_BYTES`]; a longer line is rejected rather than buffered, so a
//! misbehaving plugin cannot make the host allocate without bound.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{DeviceSpec, PROTO_VERSION, Value};

/// Longest accepted frame, excluding the trailing newline.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Messages a plugin sends to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginMessage {
    /// Must be the first message. `plugin` has to match a plugin id from the host's config.
    Hello {
        plugin: String,
        version: String,
        proto: u32,
        /// Proof of knowing the agent's token (see [`crate::auth`]); only used on remote (TCP)
        /// connections, where it answers the host's `challenge`. The token itself is never sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof: Option<String>,
    },
    /// Registers (or re-registers, after a reconnect) a device.
    Register {
        device: DeviceSpec,
    },
    /// Removes a device the plugin registered.
    Unregister {
        device: String,
    },
    /// A capability's value changed.
    Update {
        device: String,
        capability: String,
        value: Value,
    },
    /// The answer to a host `set`.
    SetResult {
        req_id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Ping,
    Pong,
}

/// Messages the host sends to a plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Sent first, unasked, on every remote (TCP) connection: a fresh random nonce the agent proves it
    /// knows the token with, in its `hello`. Never sent on the local Unix socket.
    Challenge {
        nonce: String,
    },
    /// The answer to a good `hello`.
    Welcome {
        proto: u32,
    },
    /// A controller wants a writable capability changed. The plugin answers with `set_result`.
    Set {
        req_id: u64,
        device: String,
        capability: String,
        value: Value,
    },
    /// Tells the plugin why the host rejected something. The host closes the connection after
    /// errors that occur during the handshake.
    Error {
        message: String,
    },
    Ping,
    Pong,
}

/// Why a frame could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLong { len: usize },
    Empty,
    Invalid(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLong { len } => write!(f, "frame of {len} bytes exceeds the {MAX_FRAME_BYTES} byte limit"),
            FrameError::Empty => f.write_str("empty frame"),
            FrameError::Invalid(e) => write!(f, "invalid frame: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Serialises a message as one line, including the trailing newline.
pub fn encode_line<T: Serialize>(msg: &T) -> Result<String, FrameError> {
    let mut s = serde_json::to_string(msg).map_err(|e| FrameError::Invalid(e.to_string()))?;
    if s.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLong { len: s.len() });
    }
    s.push('\n');
    Ok(s)
}

/// Parses one line (with or without its newline).
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T, FrameError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLong { len: line.len() });
    }
    if line.trim().is_empty() {
        return Err(FrameError::Empty);
    }
    serde_json::from_str(line).map_err(|e| FrameError::Invalid(e.to_string()))
}

/// What a connecting plugin and the host must agree on before anything else is accepted.
pub fn proto_compatible(plugin_proto: u32) -> bool {
    plugin_proto == PROTO_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityKind, CapabilitySpec};

    fn hello() -> PluginMessage {
        PluginMessage::Hello { plugin: "linux-system".into(), version: "0.1.0".into(), proto: 1, proof: None }
    }

    #[test]
    fn messages_round_trip() {
        let msgs = vec![
            hello(),
            PluginMessage::Register {
                device: DeviceSpec {
                    id: "pc".into(),
                    name: "PC".into(),
                    capabilities: vec![CapabilitySpec {
                        id: "online".into(),
                        kind: CapabilityKind::Boolean,
                        writable: false,
                        role: None,
                    }],
                },
            },
            PluginMessage::Unregister { device: "pc".into() },
            PluginMessage::Update { device: "pc".into(), capability: "volume".into(), value: Value::Int(30) },
            PluginMessage::SetResult { req_id: 7, ok: false, error: Some("no such sink".into()) },
            PluginMessage::Ping,
            PluginMessage::Pong,
        ];
        for m in msgs {
            let line = encode_line(&m).unwrap();
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(decode_line::<PluginMessage>(&line).unwrap(), m);
        }
        let h =
            HostMessage::Set { req_id: 1, device: "pc".into(), capability: "mute".into(), value: Value::Bool(true) };
        assert_eq!(decode_line::<HostMessage>(&encode_line(&h).unwrap()).unwrap(), h);
    }

    #[test]
    fn wire_format_is_the_documented_json() {
        assert_eq!(encode_line(&PluginMessage::Ping).unwrap(), "{\"type\":\"ping\"}\n");
        let m: PluginMessage =
            decode_line(r#"{"type":"update","device":"pc","capability":"mute","value":true}"#).unwrap();
        assert_eq!(
            m,
            PluginMessage::Update { device: "pc".into(), capability: "mute".into(), value: Value::Bool(true) }
        );
        // Optional fields may be omitted on input, and are omitted on output.
        let m: PluginMessage = decode_line(r#"{"type":"hello","plugin":"a","version":"1","proto":1}"#).unwrap();
        assert!(matches!(m, PluginMessage::Hello { proof: None, .. }));
        assert!(!encode_line(&hello()).unwrap().contains("proof"));
    }

    #[test]
    fn rejects_garbage_unknown_types_and_empty_lines() {
        assert!(matches!(decode_line::<PluginMessage>("not json"), Err(FrameError::Invalid(_))));
        assert!(matches!(decode_line::<PluginMessage>(r#"{"type":"explode"}"#), Err(FrameError::Invalid(_))));
        assert_eq!(decode_line::<PluginMessage>("  \n"), Err(FrameError::Empty));
        assert!(decode_line::<PluginMessage>(r#"{"type":"update","device":"pc"}"#).is_err());
    }

    #[test]
    fn accepts_crlf_and_rejects_oversized_frames() {
        assert_eq!(decode_line::<PluginMessage>("{\"type\":\"ping\"}\r\n").unwrap(), PluginMessage::Ping);
        let big = format!(
            "{{\"type\":\"hello\",\"plugin\":\"{}\",\"version\":\"1\",\"proto\":1}}",
            "a".repeat(MAX_FRAME_BYTES)
        );
        assert!(matches!(decode_line::<PluginMessage>(&big), Err(FrameError::TooLong { .. })));
        let huge = PluginMessage::Unregister { device: "a".repeat(MAX_FRAME_BYTES) };
        assert!(matches!(encode_line(&huge), Err(FrameError::TooLong { .. })));
    }

    #[test]
    fn only_the_current_protocol_version_is_compatible() {
        assert!(proto_compatible(PROTO_VERSION));
        assert!(!proto_compatible(PROTO_VERSION + 1) && !proto_compatible(0));
    }
}
