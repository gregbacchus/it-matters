//! The Matter-agnostic model plugins use to describe what they expose.
//!
//! A plugin registers [`DeviceSpec`]s made of [`CapabilitySpec`]s (a boolean, a level, ...). The
//! host maps those onto Matter device types and clusters; plugins never see Matter. The wire
//! format is JSON, one message per line (see the plugin protocol docs once they exist).

use std::collections::HashSet;

pub mod protocol;

use serde::{Deserialize, Serialize};

/// Version of the plugin wire protocol. Bumped on incompatible changes; negotiated in `hello`.
pub const PROTO_VERSION: u32 = 1;

/// A device a plugin exposes: a named group of capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSpec {
    /// Stable identifier, unique within the host. Endpoint ids are persisted by this id.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub capabilities: Vec<CapabilitySpec>,
}

/// One readable (and optionally writable) value of a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySpec {
    /// Identifier, unique within the device.
    pub id: String,
    #[serde(flatten)]
    pub kind: CapabilityKind,
    /// Whether controllers may change the value (the host forwards a `set` to the plugin).
    #[serde(default)]
    pub writable: bool,
    /// Optional hint that steers the Matter mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

/// The shape of a capability's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CapabilityKind {
    Boolean,
    /// An integer in `min..=max`.
    Level {
        min: i64,
        max: i64,
    },
}

/// What a capability means, when the type alone is ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A contact-style sensor (open/closed, signed in/out).
    Contact,
    /// An occupancy-style sensor (in use / not in use).
    Occupancy,
    /// A mute switch (pairs with a level capability to form a speaker).
    Mute,
}

/// A value carried by `update` / `set` messages.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
}

/// Why a [`DeviceSpec`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError(pub String);

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

/// Identifiers are lowercase ASCII letters, digits, `-` and `_`, 1 to 64 long.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl DeviceSpec {
    /// Checks the spec is well formed. The host runs this before accepting a registration.
    pub fn validate(&self) -> Result<(), SpecError> {
        let err = |m: String| Err(SpecError(format!("device {:?}: {m}", self.id)));
        if !valid_id(&self.id) {
            return err("id must be 1-64 characters of a-z, 0-9, '-' or '_'".into());
        }
        if self.name.trim().is_empty() {
            return err("name must not be empty".into());
        }
        if self.capabilities.is_empty() {
            return err("needs at least one capability".into());
        }
        let mut seen = HashSet::new();
        for c in &self.capabilities {
            if !valid_id(&c.id) {
                return err(format!("capability id {:?} must be 1-64 characters of a-z, 0-9, '-' or '_'", c.id));
            }
            if !seen.insert(c.id.as_str()) {
                return err(format!("duplicate capability id {:?}", c.id));
            }
            if let CapabilityKind::Level { min, max } = c.kind
                && min >= max
            {
                return err(format!("capability {:?}: min must be less than max", c.id));
            }
        }
        Ok(())
    }
}

impl CapabilitySpec {
    /// Whether `value` is acceptable for this capability.
    pub fn accepts(&self, value: Value) -> bool {
        match (self.kind.clone(), value) {
            (CapabilityKind::Boolean, Value::Bool(_)) => true,
            (CapabilityKind::Level { min, max }, Value::Int(v)) => (min..=max).contains(&v),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
        "id": "desktop-audio",
        "name": "Desktop Audio",
        "capabilities": [
            { "id": "volume", "type": "level", "writable": true, "min": 0, "max": 100 },
            { "id": "mute", "type": "boolean", "writable": true, "role": "mute" }
        ]
    }"#;

    fn example() -> DeviceSpec {
        serde_json::from_str(EXAMPLE).unwrap()
    }

    #[test]
    fn parses_the_documented_example() {
        let d = example();
        assert_eq!(d.id, "desktop-audio");
        assert_eq!(d.capabilities[0].kind, CapabilityKind::Level { min: 0, max: 100 });
        assert!(d.capabilities[0].writable);
        assert_eq!(d.capabilities[1].role, Some(Role::Mute));
        assert_eq!(d.validate(), Ok(()));
    }

    #[test]
    fn writable_and_role_are_optional() {
        let c: CapabilitySpec = serde_json::from_str(r#"{ "id": "online", "type": "boolean" }"#).unwrap();
        assert!(!c.writable);
        assert_eq!(c.role, None);
    }

    #[test]
    fn round_trips_through_json() {
        let d = example();
        let again: DeviceSpec = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(d, again);
    }

    #[test]
    fn unknown_capability_type_is_rejected() {
        assert!(serde_json::from_str::<CapabilitySpec>(r#"{ "id": "x", "type": "colour" }"#).is_err());
    }

    #[test]
    fn rejects_bad_ids_names_and_empty_devices() {
        let mut d = example();
        d.id = "Desktop Audio".into();
        assert!(d.validate().is_err());
        let mut d = example();
        d.name = "  ".into();
        assert!(d.validate().is_err());
        let mut d = example();
        d.capabilities.clear();
        assert!(d.validate().is_err());
        assert!(!valid_id(&"a".repeat(65)));
        assert!(!valid_id(""));
    }

    #[test]
    fn rejects_duplicate_capability_ids_and_inverted_levels() {
        let mut d = example();
        d.capabilities[1].id = "volume".into();
        assert!(d.validate().unwrap_err().0.contains("duplicate"));
        let mut d = example();
        d.capabilities[0].kind = CapabilityKind::Level { min: 5, max: 5 };
        assert!(d.validate().unwrap_err().0.contains("min must be less than max"));
    }

    #[test]
    fn accepts_only_values_of_the_right_shape_and_range() {
        let d = example();
        let (volume, mute) = (&d.capabilities[0], &d.capabilities[1]);
        assert!(volume.accepts(Value::Int(0)) && volume.accepts(Value::Int(100)));
        assert!(!volume.accepts(Value::Int(101)) && !volume.accepts(Value::Bool(true)));
        assert!(mute.accepts(Value::Bool(false)) && !mute.accepts(Value::Int(1)));
    }

    #[test]
    fn values_serialize_as_bare_json() {
        assert_eq!(serde_json::to_string(&Value::Bool(true)).unwrap(), "true");
        assert_eq!(serde_json::from_str::<Value>("42").unwrap(), Value::Int(42));
    }
}
