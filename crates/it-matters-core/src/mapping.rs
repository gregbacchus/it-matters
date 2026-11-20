//! Maps a plugin's [`DeviceSpec`] onto Matter *units*. A unit becomes one bridged endpoint.
//!
//! Rules (anything not listed is rejected at registration with a message naming the capability,
//! never silently dropped):
//!
//! | Capability | Unit |
//! |---|---|
//! | read-only boolean, role `contact` or no role | Contact Sensor (Boolean State) |
//! | read-only boolean, role `occupancy` | Occupancy Sensor |
//! | writable boolean, no role | On/Off Plug-in Unit |
//! | one level + one boolean with role `mute` (both writable) | Speaker (Level Control + On/Off) |
//!
//! A Speaker's On/Off means *not muted*: `mute = true` is On/Off `false`. That is what the Matter
//! Speaker device type defines.

use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role};

/// The Matter device a unit becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    ContactSensor,
    OccupancySensor,
    OnOffPlugin,
    Speaker,
}

impl UnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnitKind::ContactSensor => "contact_sensor",
            UnitKind::OccupancySensor => "occupancy_sensor",
            UnitKind::OnOffPlugin => "on_off_plugin",
            UnitKind::Speaker => "speaker",
        }
    }
}

/// One bridged endpoint's worth of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Unique within the device: the (level) capability id for a speaker, else the capability id.
    pub id: String,
    pub kind: UnitKind,
    /// The capabilities this unit reads and writes, in a fixed order (level before mute).
    pub capabilities: Vec<String>,
}

impl Unit {
    /// Identifies the endpoint's cluster shape. If it changes, the unit needs a new endpoint id.
    /// Ranges (min/max) are deliberately not part of it: they do not change which clusters exist.
    pub fn shape(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.capabilities.join("+"))
    }
}

/// Maps a validated device spec to units, or says why it cannot be mapped.
pub fn map_device(spec: &DeviceSpec) -> Result<Vec<Unit>, String> {
    spec.validate().map_err(|e| e.0)?;
    let err = |m: String| Err(format!("device {:?}: {m}", spec.id));

    let levels: Vec<&CapabilitySpec> =
        spec.capabilities.iter().filter(|c| matches!(c.kind, CapabilityKind::Level { .. })).collect();
    let mutes: Vec<&CapabilitySpec> =
        spec.capabilities.iter().filter(|c| c.kind == CapabilityKind::Boolean && c.role == Some(Role::Mute)).collect();

    if levels.len() > 1 {
        return err("more than one level capability; at most one speaker per device is supported".into());
    }
    if mutes.len() > 1 {
        return err("more than one mute capability; at most one speaker per device is supported".into());
    }
    let mut units = Vec::new();
    match (levels.first(), mutes.first()) {
        (Some(level), Some(mute)) => {
            if !level.writable || !mute.writable {
                return err(format!("speaker capabilities {:?} and {:?} must both be writable", level.id, mute.id));
            }
            units.push(Unit {
                id: level.id.clone(),
                kind: UnitKind::Speaker,
                capabilities: vec![level.id.clone(), mute.id.clone()],
            });
        }
        (Some(level), None) => {
            return err(format!(
                "level capability {:?} needs a boolean with role \"mute\" to form a speaker",
                level.id
            ));
        }
        (None, Some(mute)) => {
            return err(format!("mute capability {:?} needs a level capability to form a speaker", mute.id));
        }
        (None, None) => {}
    }

    for c in &spec.capabilities {
        if c.kind != CapabilityKind::Boolean || c.role == Some(Role::Mute) {
            continue;
        }
        let kind = match (c.writable, c.role) {
            (false, None | Some(Role::Contact)) => UnitKind::ContactSensor,
            (false, Some(Role::Occupancy)) => UnitKind::OccupancySensor,
            (true, None) => UnitKind::OnOffPlugin,
            (true, Some(role)) => {
                return err(format!("capability {:?} has role {role:?} but is writable; sensors are read-only", c.id));
            }
            (false, Some(Role::Mute)) => unreachable!("mute capabilities are handled above"),
        };
        units.push(Unit { id: c.id.clone(), kind, capabilities: vec![c.id.clone()] });
    }

    if units.is_empty() {
        return err("no capability maps onto a Matter device".into());
    }
    Ok(units)
}

/// The largest Level Control `CurrentLevel` (Matter reserves 255 for "null").
pub const MATTER_LEVEL_MAX: u8 = 254;

/// Scales a plugin level in `min..=max` to Matter's `0..=254`, rounding to nearest and clamping.
pub fn level_to_matter(value: i64, min: i64, max: i64) -> u8 {
    debug_assert!(min < max);
    let v = i128::from(value.clamp(min, max)) - i128::from(min);
    let span = i128::from(max) - i128::from(min);
    let n = i128::from(MATTER_LEVEL_MAX);
    ((v * n + span / 2) / span) as u8
}

/// The inverse of [`level_to_matter`]. Levels above 254 are clamped.
pub fn level_from_matter(level: u8, min: i64, max: i64) -> i64 {
    debug_assert!(min < max);
    let l = i128::from(level.min(MATTER_LEVEL_MAX));
    let span = i128::from(max) - i128::from(min);
    let n = i128::from(MATTER_LEVEL_MAX);
    (i128::from(min) + (l * span + n / 2) / n) as i64
}

/// A plugin's `mute` value as the Speaker's On/Off attribute (On = not muted).
pub fn mute_to_on_off(muted: bool) -> bool {
    !muted
}

/// A controller's Speaker On/Off value as the plugin's `mute` value.
pub fn on_off_to_mute(on: bool) -> bool {
    !on
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str, kind: CapabilityKind, writable: bool, role: Option<Role>) -> CapabilitySpec {
        CapabilitySpec { id: id.into(), kind, writable, role }
    }
    fn level(id: &str, writable: bool) -> CapabilitySpec {
        cap(id, CapabilityKind::Level { min: 0, max: 100 }, writable, None)
    }
    fn boolean(id: &str, writable: bool, role: Option<Role>) -> CapabilitySpec {
        cap(id, CapabilityKind::Boolean, writable, role)
    }
    fn device(caps: Vec<CapabilitySpec>) -> DeviceSpec {
        DeviceSpec { id: "pc".into(), name: "PC".into(), capabilities: caps }
    }
    fn kinds(d: &DeviceSpec) -> Vec<(String, UnitKind)> {
        map_device(d).unwrap().into_iter().map(|u| (u.id, u.kind)).collect()
    }

    #[test]
    fn level_and_mute_form_a_speaker() {
        let d = device(vec![level("volume", true), boolean("mute", true, Some(Role::Mute))]);
        let units = map_device(&d).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].kind, UnitKind::Speaker);
        assert_eq!(units[0].id, "volume");
        assert_eq!(units[0].capabilities, vec!["volume", "mute"]);
        assert_eq!(units[0].shape(), "speaker:volume+mute");
    }

    #[test]
    fn capability_order_does_not_change_the_speaker() {
        let d = device(vec![boolean("mute", true, Some(Role::Mute)), level("volume", true)]);
        assert_eq!(map_device(&d).unwrap()[0].capabilities, vec!["volume", "mute"]);
    }

    #[test]
    fn booleans_map_by_role_and_writability() {
        let d = device(vec![
            boolean("online", false, None),
            boolean("signed-in", false, Some(Role::Contact)),
            boolean("mic-active", false, Some(Role::Occupancy)),
            boolean("lamp", true, None),
        ]);
        assert_eq!(
            kinds(&d),
            vec![
                ("online".into(), UnitKind::ContactSensor),
                ("signed-in".into(), UnitKind::ContactSensor),
                ("mic-active".into(), UnitKind::OccupancySensor),
                ("lamp".into(), UnitKind::OnOffPlugin),
            ]
        );
    }

    #[test]
    fn a_speaker_and_sensors_can_share_a_device() {
        let d = device(vec![
            boolean("mic-active", false, Some(Role::Occupancy)),
            level("volume", true),
            boolean("mute", true, Some(Role::Mute)),
        ]);
        let got = kinds(&d);
        assert_eq!(got[0], ("volume".into(), UnitKind::Speaker), "the speaker comes first");
        assert_eq!(got[1], ("mic-active".into(), UnitKind::OccupancySensor));
    }

    #[test]
    fn unmappable_combinations_are_rejected_with_the_capability_named() {
        let e = |caps| map_device(&device(caps)).unwrap_err();
        assert!(e(vec![level("volume", true)]).contains("\"volume\" needs a boolean with role"));
        assert!(e(vec![boolean("mute", true, Some(Role::Mute))]).contains("needs a level"));
        assert!(e(vec![boolean("x", true, Some(Role::Occupancy))]).contains("sensors are read-only"));
        assert!(
            e(vec![level("a", true), level("b", true), boolean("m", true, Some(Role::Mute))])
                .contains("more than one level")
        );
        assert!(
            e(vec![level("v", true), boolean("m", true, Some(Role::Mute)), boolean("n", true, Some(Role::Mute))])
                .contains("more than one mute")
        );
        assert!(e(vec![level("v", false), boolean("m", true, Some(Role::Mute))]).contains("must both be writable"));
    }

    #[test]
    fn invalid_specs_are_rejected_before_mapping() {
        let mut d = device(vec![boolean("online", false, None)]);
        d.id = "Bad Id".into();
        assert!(map_device(&d).unwrap_err().contains("id must be"));
    }

    #[test]
    fn shape_ignores_ranges_and_names_but_not_roles() {
        let mut a = device(vec![level("volume", true), boolean("mute", true, Some(Role::Mute))]);
        let before = map_device(&a).unwrap()[0].shape();
        a.capabilities[0].kind = CapabilityKind::Level { min: 0, max: 255 };
        a.name = "Renamed".into();
        assert_eq!(map_device(&a).unwrap()[0].shape(), before);
        let sensor = device(vec![boolean("x", false, None)]);
        let occupancy = device(vec![boolean("x", false, Some(Role::Occupancy))]);
        assert_ne!(map_device(&sensor).unwrap()[0].shape(), map_device(&occupancy).unwrap()[0].shape());
    }

    #[test]
    fn level_scaling_hits_both_ends() {
        assert_eq!(level_to_matter(0, 0, 100), 0);
        assert_eq!(level_to_matter(100, 0, 100), 254);
        assert_eq!(level_to_matter(50, 0, 100), 127);
        assert_eq!(level_from_matter(0, 0, 100), 0);
        assert_eq!(level_from_matter(254, 0, 100), 100);
        assert_eq!(level_from_matter(255, 0, 100), 100, "255 is Matter's null; clamp");
    }

    #[test]
    fn level_scaling_clamps_out_of_range_input() {
        assert_eq!(level_to_matter(-5, 0, 100), 0);
        assert_eq!(level_to_matter(1000, 0, 100), 254);
        assert_eq!(level_to_matter(i64::MAX, i64::MIN, i64::MAX), 254);
    }

    #[test]
    fn every_plugin_level_survives_a_round_trip_when_the_range_fits() {
        for (min, max) in [(0, 100), (0, 254), (-60, 0), (1, 10)] {
            for v in min..=max {
                let m = level_to_matter(v, min, max);
                assert_eq!(level_from_matter(m, min, max), v, "range {min}..={max}, value {v}, matter {m}");
            }
        }
    }

    #[test]
    fn scaling_is_monotonic_and_stays_close_for_wide_ranges() {
        let (min, max) = (0, 100_000);
        let mut last = 0;
        for v in (0..=max).step_by(97) {
            let m = level_to_matter(v, min, max);
            assert!(m >= last);
            last = m;
            let back = level_from_matter(m, min, max);
            assert!((back - v).abs() <= max / 254, "{v} -> {m} -> {back}");
        }
    }

    #[test]
    fn mute_is_the_inverse_of_on_off() {
        assert!(!mute_to_on_off(true), "muted = Off");
        assert!(mute_to_on_off(false), "not muted = On");
        for b in [true, false] {
            assert_eq!(on_off_to_mute(mute_to_on_off(b)), b);
        }
    }
}
