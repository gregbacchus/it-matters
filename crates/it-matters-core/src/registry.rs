//! Which devices exist, who owns them, their last known values, and what the Matter side must be
//! told when something changes.
//!
//! The registry is a plain synchronous state machine: every operation returns the
//! [`FrontendEvent`]s to send, and the async host layer forwards them. That keeps the lifecycle
//! rules (below) testable without sockets or a runtime.
//!
//! Lifecycle:
//! * a device id belongs to the plugin that first registered it; another plugin cannot take it;
//! * a plugin disconnecting marks its devices *unreachable* but keeps their endpoints;
//! * registering again (after a reconnect) with the same shape reuses the same endpoints and
//!   makes them reachable; endpoints whose unit disappeared or changed shape are removed;
//! * an explicit unregister removes the endpoints (their ids stay reserved);
//! * the optional `devices.<id>.name` override replaces the plugin's name.
//!
//! A newly registered unit reports default values (false / the range minimum) until the plugin
//! sends its first `update`, so plugins should update right after registering.

use std::collections::BTreeMap;

use it_matters_model::{CapabilityKind, CapabilitySpec, DeviceSpec, Role, Value};

use crate::allocator::{Allocator, Store};
use crate::frontend::{AttrValue, EndpointSpec, FrontendEvent, Write};
use crate::mapping::{Unit, UnitKind, level_from_matter, level_to_matter, map_device, mute_to_on_off, on_off_to_mute};

/// Where a controller write must be sent: the plugin, device, capability and the plugin-side value.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub plugin: String,
    pub device: String,
    pub capability: String,
    pub value: Value,
}

#[derive(Debug, Clone)]
struct UnitEntry {
    unit: Unit,
    endpoint: u16,
    name: String,
}

#[derive(Debug, Clone)]
struct DeviceEntry {
    plugin: String,
    spec: DeviceSpec,
    units: Vec<UnitEntry>,
    values: BTreeMap<String, Value>,
    reachable: bool,
}

pub struct Registry<S: Store> {
    allocator: Allocator<S>,
    name_overrides: BTreeMap<String, String>,
    devices: BTreeMap<String, DeviceEntry>,
}

fn capability<'a>(spec: &'a DeviceSpec, id: &str) -> Option<&'a CapabilitySpec> {
    spec.capabilities.iter().find(|c| c.id == id)
}

fn default_value(c: &CapabilitySpec) -> Value {
    match c.kind {
        CapabilityKind::Boolean => Value::Bool(false),
        CapabilityKind::Level { min, .. } => Value::Int(min),
    }
}

/// Converts one capability's plugin value to the Matter attribute of its unit.
fn to_attr(kind: UnitKind, c: &CapabilitySpec, value: Value) -> AttrValue {
    match (kind, c.kind.clone(), value) {
        (UnitKind::Speaker, CapabilityKind::Level { min, max }, Value::Int(v)) => {
            AttrValue::CurrentLevel(level_to_matter(v, min, max))
        }
        (UnitKind::Speaker, _, Value::Bool(muted)) => AttrValue::OnOff(mute_to_on_off(muted)),
        (UnitKind::ContactSensor, _, Value::Bool(b)) => AttrValue::BooleanState(b),
        (UnitKind::OccupancySensor, _, Value::Bool(b)) => AttrValue::Occupancy(b),
        (UnitKind::OnOffPlugin, _, Value::Bool(b)) => AttrValue::OnOff(b),
        // Values are validated against the capability before they are stored.
        (_, _, Value::Int(_)) => unreachable!("value does not fit its capability"),
    }
}

impl DeviceEntry {
    fn value_of(&self, c: &CapabilitySpec) -> Value {
        self.values.get(&c.id).copied().unwrap_or_else(|| default_value(c))
    }

    fn attrs(&self, u: &UnitEntry) -> Vec<AttrValue> {
        u.unit
            .capabilities
            .iter()
            .filter_map(|id| capability(&self.spec, id))
            .map(|c| to_attr(u.unit.kind, c, self.value_of(c)))
            .collect()
    }

    fn endpoint_spec(&self, device_id: &str, u: &UnitEntry) -> EndpointSpec {
        EndpointSpec {
            endpoint: u.endpoint,
            kind: u.unit.kind,
            name: u.name.clone(),
            unique_id: format!("{device_id}/{}", u.unit.id),
            reachable: self.reachable,
            attrs: self.attrs(u),
        }
    }
}

impl<S: Store> Registry<S> {
    pub fn new(allocator: Allocator<S>, name_overrides: BTreeMap<String, String>) -> Self {
        Self { allocator, name_overrides, devices: BTreeMap::new() }
    }

    /// Registers a device, or re-registers it after a reconnect. On error nothing changes
    /// (except that endpoint ids may have been reserved, which is harmless).
    pub fn register(&mut self, plugin: &str, spec: DeviceSpec) -> Result<Vec<FrontendEvent>, String> {
        let units = map_device(&spec)?;
        if let Some(existing) = self.devices.get(&spec.id)
            && existing.plugin != plugin
        {
            return Err(format!("device {:?} is already registered by plugin {:?}", spec.id, existing.plugin));
        }
        let mut with_ids = Vec::new();
        for u in units {
            let id = self.allocator.get_or_allocate(&spec.id, &u.id, &u.shape())?;
            with_ids.push((u, id));
        }

        let base = self.name_overrides.get(&spec.id).unwrap_or(&spec.name).clone();
        let multi = with_ids.len() > 1;
        let old = self.devices.remove(&spec.id);
        let (old_units, mut values) = old.map_or((Vec::new(), BTreeMap::new()), |o| (o.units, o.values));
        values.retain(|k, v| capability(&spec, k).is_some_and(|c| c.accepts(*v)));

        let mut events: Vec<FrontendEvent> = old_units
            .iter()
            .filter(|o| !with_ids.iter().any(|(_, id)| *id == o.endpoint))
            .map(|o| FrontendEvent::RemoveEndpoint { endpoint: o.endpoint })
            .collect();

        let units = with_ids
            .into_iter()
            .map(|(unit, endpoint)| {
                let name = if multi { format!("{base} {}", unit.id) } else { base.clone() };
                UnitEntry { unit, endpoint, name }
            })
            .collect::<Vec<_>>();
        let entry = DeviceEntry { plugin: plugin.to_owned(), spec, units, values, reachable: true };
        for u in &entry.units {
            if old_units.iter().any(|o| o.endpoint == u.endpoint) {
                events.push(FrontendEvent::SetReachable { endpoint: u.endpoint, reachable: true });
            } else {
                events.push(FrontendEvent::AddEndpoint(entry.endpoint_spec(&entry.spec.id, u)));
            }
        }
        self.devices.insert(entry.spec.id.clone(), entry);
        Ok(events)
    }

    /// Removes a device the plugin owns.
    pub fn unregister(&mut self, plugin: &str, device: &str) -> Result<Vec<FrontendEvent>, String> {
        self.owned(plugin, device)?;
        let entry = self.devices.remove(device).expect("checked above");
        Ok(entry.units.iter().map(|u| FrontendEvent::RemoveEndpoint { endpoint: u.endpoint }).collect())
    }

    /// Records a new value. Returns no events if the value did not change.
    pub fn update(
        &mut self,
        plugin: &str,
        device: &str,
        capability_id: &str,
        value: Value,
    ) -> Result<Vec<FrontendEvent>, String> {
        self.owned(plugin, device)?;
        let entry = self.devices.get_mut(device).expect("checked above");
        let cap = capability(&entry.spec, capability_id)
            .ok_or_else(|| format!("device {device:?} has no capability {capability_id:?}"))?
            .clone();
        if !cap.accepts(value) {
            return Err(format!("value {value:?} is not valid for {device:?}/{capability_id:?}"));
        }
        if entry.values.insert(capability_id.to_owned(), value) == Some(value) {
            return Ok(Vec::new());
        }
        let unit = entry
            .units
            .iter()
            .find(|u| u.unit.capabilities.iter().any(|c| c == capability_id))
            .expect("every capability belongs to a unit");
        Ok(vec![FrontendEvent::SetAttr { endpoint: unit.endpoint, value: to_attr(unit.unit.kind, &cap, value) }])
    }

    /// Marks every device of a plugin unreachable, keeping the endpoints.
    pub fn plugin_disconnected(&mut self, plugin: &str) -> Vec<FrontendEvent> {
        let mut events = Vec::new();
        for entry in self.devices.values_mut().filter(|d| d.plugin == plugin && d.reachable) {
            entry.reachable = false;
            events.extend(
                entry.units.iter().map(|u| FrontendEvent::SetReachable { endpoint: u.endpoint, reachable: false }),
            );
        }
        events
    }

    /// Works out where a controller write has to go. Fails if the endpoint is unknown, the device
    /// is unreachable, or the write does not suit the endpoint.
    pub fn route(&self, endpoint: u16, write: Write) -> Result<Route, String> {
        let (entry, unit) = self
            .devices
            .values()
            .find_map(|d| d.units.iter().find(|u| u.endpoint == endpoint).map(|u| (d, u)))
            .ok_or_else(|| format!("no device at endpoint {endpoint}"))?;
        if !entry.reachable {
            return Err(format!("device {:?} is not reachable", entry.spec.id));
        }
        let cap_by = |role_is_mute: bool| {
            unit.unit
                .capabilities
                .iter()
                .filter_map(|id| capability(&entry.spec, id))
                .find(|c| (c.role == Some(Role::Mute)) == role_is_mute)
        };
        let (cap, value) = match (unit.unit.kind, write) {
            (UnitKind::OnOffPlugin, Write::OnOff(on)) => (cap_by(false), Value::Bool(on)),
            (UnitKind::Speaker, Write::OnOff(on)) => (cap_by(true), Value::Bool(on_off_to_mute(on))),
            (UnitKind::Speaker, Write::CurrentLevel(l)) => {
                let cap = cap_by(false);
                let CapabilityKind::Level { min, max } = cap.map(|c| c.kind.clone()).unwrap_or(CapabilityKind::Boolean)
                else {
                    return Err("speaker has no level capability".into());
                };
                (cap, Value::Int(level_from_matter(l, min, max)))
            }
            _ => return Err(format!("endpoint {endpoint} does not accept this write")),
        };
        let cap = cap.ok_or_else(|| format!("endpoint {endpoint} has no matching capability"))?;
        if !cap.writable {
            return Err(format!("capability {:?} is read-only", cap.id));
        }
        Ok(Route { plugin: entry.plugin.clone(), device: entry.spec.id.clone(), capability: cap.id.clone(), value })
    }

    /// Applies a write the plugin confirmed, so controllers see it without waiting for the
    /// plugin's own `update` (which, if it differs, corrects it).
    pub fn confirm(&mut self, route: &Route) -> Vec<FrontendEvent> {
        self.update(&route.plugin, &route.device, &route.capability, route.value).unwrap_or_default()
    }

    /// `AddEndpoint` events for everything registered, to bring a fresh frontend up to date.
    pub fn snapshot(&self) -> Vec<FrontendEvent> {
        self.devices
            .values()
            .flat_map(|d| d.units.iter().map(|u| FrontendEvent::AddEndpoint(d.endpoint_spec(&d.spec.id, u))))
            .collect()
    }

    /// The plugin owning a device, if registered.
    pub fn owner(&self, device: &str) -> Option<&str> {
        self.devices.get(device).map(|d| d.plugin.as_str())
    }

    fn owned(&self, plugin: &str, device: &str) -> Result<(), String> {
        match self.devices.get(device) {
            None => Err(format!("device {device:?} is not registered")),
            Some(d) if d.plugin != plugin => Err(format!("device {device:?} belongs to plugin {:?}", d.plugin)),
            Some(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocator::MemoryStore;

    fn registry() -> Registry<MemoryStore> {
        Registry::new(Allocator::open(MemoryStore::default()).unwrap(), BTreeMap::new())
    }
    fn cap(id: &str, kind: CapabilityKind, writable: bool, role: Option<Role>) -> CapabilitySpec {
        CapabilitySpec { id: id.into(), kind, writable, role }
    }
    fn audio() -> DeviceSpec {
        DeviceSpec {
            id: "desktop-audio".into(),
            name: "Desktop Audio".into(),
            capabilities: vec![
                cap("volume", CapabilityKind::Level { min: 0, max: 100 }, true, None),
                cap("mute", CapabilityKind::Boolean, true, Some(Role::Mute)),
            ],
        }
    }
    fn pc() -> DeviceSpec {
        DeviceSpec {
            id: "pc".into(),
            name: "Office PC".into(),
            capabilities: vec![
                cap("online", CapabilityKind::Boolean, false, None),
                cap("mic-active", CapabilityKind::Boolean, false, Some(Role::Occupancy)),
            ],
        }
    }
    fn added(events: &[FrontendEvent]) -> Vec<&EndpointSpec> {
        events.iter().filter_map(|e| if let FrontendEvent::AddEndpoint(s) = e { Some(s) } else { None }).collect()
    }

    #[test]
    fn registering_adds_one_endpoint_per_unit_with_default_values() {
        let mut r = registry();
        let ev = r.register("p", pc()).unwrap();
        let eps = added(&ev);
        assert_eq!(eps.len(), 2);
        assert_eq!((eps[0].endpoint, eps[0].kind), (2, UnitKind::ContactSensor));
        assert_eq!((eps[1].endpoint, eps[1].kind), (3, UnitKind::OccupancySensor));
        assert_eq!(eps[0].name, "Office PC online", "multi-unit devices get the capability appended");
        assert_eq!(eps[0].unique_id, "pc/online");
        assert!(eps[0].reachable);
        assert_eq!(eps[0].attrs, vec![AttrValue::BooleanState(false)]);
    }

    #[test]
    fn a_single_unit_device_uses_the_plain_name() {
        let mut r = registry();
        let ev = r.register("p", audio()).unwrap();
        let eps = added(&ev);
        assert_eq!(eps[0].name, "Desktop Audio");
        assert_eq!(eps[0].attrs, vec![AttrValue::CurrentLevel(0), AttrValue::OnOff(true)], "not muted = On");
    }

    #[test]
    fn name_overrides_replace_the_plugins_name() {
        let ids = Allocator::open(MemoryStore::default()).unwrap();
        let mut r = Registry::new(ids, BTreeMap::from([("desktop-audio".to_owned(), "Office PC Audio".to_owned())]));
        let ev = r.register("p", audio()).unwrap();
        assert_eq!(added(&ev)[0].name, "Office PC Audio");
    }

    #[test]
    fn unmappable_devices_are_rejected_and_leave_no_trace() {
        let mut r = registry();
        let mut bad = audio();
        bad.capabilities.pop();
        assert!(r.register("p", bad).unwrap_err().contains("needs a boolean with role"));
        assert!(r.snapshot().is_empty() && r.owner("desktop-audio").is_none());
    }

    #[test]
    fn a_second_plugin_cannot_take_a_device_id() {
        let mut r = registry();
        r.register("a", pc()).unwrap();
        assert!(r.register("b", pc()).unwrap_err().contains("already registered by plugin \"a\""));
        assert!(r.update("b", "pc", "online", Value::Bool(true)).unwrap_err().contains("belongs to plugin"));
        assert!(r.unregister("b", "pc").is_err());
        assert_eq!(r.owner("pc"), Some("a"));
    }

    #[test]
    fn updates_become_attribute_events_and_unchanged_values_are_dropped() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        let ev = r.update("p", "pc", "mic-active", Value::Bool(true)).unwrap();
        assert_eq!(ev, vec![FrontendEvent::SetAttr { endpoint: 3, value: AttrValue::Occupancy(true) }]);
        assert!(r.update("p", "pc", "mic-active", Value::Bool(true)).unwrap().is_empty());
    }

    #[test]
    fn speaker_updates_are_scaled_and_mute_is_inverted() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        let ev = r.update("p", "desktop-audio", "volume", Value::Int(100)).unwrap();
        assert_eq!(ev, vec![FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(254) }]);
        let ev = r.update("p", "desktop-audio", "mute", Value::Bool(true)).unwrap();
        assert_eq!(ev, vec![FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::OnOff(false) }], "muted = Off");
    }

    #[test]
    fn bad_updates_are_rejected() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        assert!(r.update("p", "desktop-audio", "volume", Value::Int(101)).unwrap_err().contains("not valid"));
        assert!(r.update("p", "desktop-audio", "volume", Value::Bool(true)).is_err());
        assert!(r.update("p", "desktop-audio", "nope", Value::Bool(true)).unwrap_err().contains("no capability"));
        assert!(r.update("p", "ghost", "x", Value::Bool(true)).unwrap_err().contains("not registered"));
    }

    #[test]
    fn disconnect_keeps_endpoints_but_marks_them_unreachable_once() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        r.register("q", audio()).unwrap();
        let ev = r.plugin_disconnected("p");
        assert_eq!(
            ev,
            vec![
                FrontendEvent::SetReachable { endpoint: 2, reachable: false },
                FrontendEvent::SetReachable { endpoint: 3, reachable: false }
            ]
        );
        assert!(r.plugin_disconnected("p").is_empty(), "already unreachable");
        assert_eq!(r.owner("pc"), Some("p"), "the device is kept");
        assert!(r.plugin_disconnected("nobody").is_empty());
    }

    #[test]
    fn reregistering_after_a_reconnect_reuses_the_endpoints_and_keeps_values() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        r.update("p", "pc", "online", Value::Bool(true)).unwrap();
        r.plugin_disconnected("p");
        let ev = r.register("p", pc()).unwrap();
        assert_eq!(
            ev,
            vec![
                FrontendEvent::SetReachable { endpoint: 2, reachable: true },
                FrontendEvent::SetReachable { endpoint: 3, reachable: true }
            ]
        );
        let snap = r.snapshot();
        assert_eq!(added(&snap)[0].attrs, vec![AttrValue::BooleanState(true)], "last known value kept");
    }

    #[test]
    fn a_changed_shape_removes_the_old_endpoint_and_adds_a_new_one() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        let mut changed = pc();
        changed.capabilities[1].role = None; // occupancy sensor -> contact sensor
        let ev = r.register("p", changed).unwrap();
        assert_eq!(ev[0], FrontendEvent::RemoveEndpoint { endpoint: 3 });
        assert!(ev.contains(&FrontendEvent::SetReachable { endpoint: 2, reachable: true }));
        let new = added(&ev);
        assert_eq!(new.len(), 1);
        assert!(new[0].endpoint > 3, "a new id, the old one is not reused");
    }

    #[test]
    fn dropping_a_capability_on_reregister_removes_its_endpoint() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        let mut smaller = pc();
        smaller.capabilities.pop();
        let ev = r.register("p", smaller).unwrap();
        assert!(ev.contains(&FrontendEvent::RemoveEndpoint { endpoint: 3 }));
    }

    #[test]
    fn unregister_removes_endpoints_and_ids_come_back_if_it_registers_again() {
        let mut r = registry();
        r.register("p", pc()).unwrap();
        let ev = r.unregister("p", "pc").unwrap();
        assert_eq!(
            ev,
            vec![FrontendEvent::RemoveEndpoint { endpoint: 2 }, FrontendEvent::RemoveEndpoint { endpoint: 3 }]
        );
        assert!(r.snapshot().is_empty() && r.owner("pc").is_none());
        let ev = r.register("other", pc()).unwrap();
        assert_eq!(added(&ev)[0].endpoint, 2, "the same unit gets its old id back, even from another plugin");
    }

    #[test]
    fn controller_writes_are_routed_back_to_plugin_values() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        let level = r.route(2, Write::CurrentLevel(254)).unwrap();
        assert_eq!(
            level,
            Route {
                plugin: "p".into(),
                device: "desktop-audio".into(),
                capability: "volume".into(),
                value: Value::Int(100)
            }
        );
        let mute = r.route(2, Write::OnOff(false)).unwrap();
        assert_eq!((mute.capability.as_str(), mute.value), ("mute", Value::Bool(true)), "Off = muted");
    }

    #[test]
    fn plugs_route_on_off_and_sensors_refuse_writes() {
        let mut r = registry();
        r.register(
            "p",
            DeviceSpec {
                id: "lamp".into(),
                name: "Lamp".into(),
                capabilities: vec![cap("power", CapabilityKind::Boolean, true, None)],
            },
        )
        .unwrap();
        r.register("p", pc()).unwrap();
        let lamp = r.route(2, Write::OnOff(true)).unwrap();
        assert_eq!((lamp.capability.as_str(), lamp.value), ("power", Value::Bool(true)));
        assert!(r.route(3, Write::OnOff(true)).unwrap_err().contains("does not accept"));
        assert!(r.route(2, Write::CurrentLevel(5)).is_err());
        assert!(r.route(99, Write::OnOff(true)).unwrap_err().contains("no device"));
    }

    #[test]
    fn a_read_only_speaker_reports_values_but_refuses_every_controller_write() {
        let mut r = registry();
        let mut read_only = audio();
        for c in &mut read_only.capabilities {
            c.writable = false;
        }
        r.register("p", read_only).unwrap();
        // Values still flow to the Matter side...
        let ev = r.update("p", "desktop-audio", "volume", Value::Int(100)).unwrap();
        assert_eq!(ev, vec![FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(254) }]);
        // ...but a controller cannot change them, and the plugin is never asked.
        assert!(r.route(2, Write::CurrentLevel(10)).unwrap_err().contains("read-only"));
        assert!(r.route(2, Write::OnOff(false)).unwrap_err().contains("read-only"));
    }

    #[test]
    fn writes_to_unreachable_devices_are_refused() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        r.plugin_disconnected("p");
        assert!(r.route(2, Write::OnOff(true)).unwrap_err().contains("not reachable"));
    }

    #[test]
    fn a_confirmed_write_updates_the_state() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        let route = r.route(2, Write::CurrentLevel(127)).unwrap();
        let ev = r.confirm(&route);
        assert_eq!(ev, vec![FrontendEvent::SetAttr { endpoint: 2, value: AttrValue::CurrentLevel(127) }]);
    }

    #[test]
    fn the_snapshot_reflects_current_values_and_reachability() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        r.update("p", "desktop-audio", "mute", Value::Bool(true)).unwrap();
        r.plugin_disconnected("p");
        let snap = r.snapshot();
        let eps = added(&snap);
        assert!(!eps[0].reachable);
        assert!(eps[0].attrs.contains(&AttrValue::OnOff(false)));
    }

    #[test]
    fn stale_values_are_dropped_when_a_capability_changes_type() {
        let mut r = registry();
        r.register("p", audio()).unwrap();
        r.update("p", "desktop-audio", "volume", Value::Int(90)).unwrap();
        let mut narrower = audio();
        narrower.capabilities[0].kind = CapabilityKind::Level { min: 0, max: 50 };
        r.register("p", narrower).unwrap();
        let snap = r.snapshot();
        assert_eq!(added(&snap)[0].attrs[0], AttrValue::CurrentLevel(0), "90 no longer fits 0..=50");
    }
}
