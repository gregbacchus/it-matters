//! The Matter side's copy of every bridged endpoint's state, and how host events change it.
//!
//! Pure data: no rs-matter types, so it is unit-tested directly. [`Table::apply`] returns what changed
//! so the caller can tell rs-matter which attributes to report to subscribers.

use std::collections::BTreeMap;

use it_matters_core::frontend::{AttrValue, EndpointSpec, FrontendEvent};
use it_matters_core::mapping::UnitKind;

/// Everything a bridged endpoint serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointState {
    pub kind: UnitKind,
    pub name: String,
    pub unique_id: String,
    pub reachable: bool,
    /// Boolean State `StateValue` (Contact Sensor).
    pub contact: bool,
    /// Occupancy Sensing `Occupancy` bit 0.
    pub occupied: bool,
    /// On/Off `OnOff` (a speaker's On means *not muted*).
    pub on_off: bool,
    /// Level Control `CurrentLevel`.
    pub level: u8,
}

impl EndpointState {
    fn new(spec: &EndpointSpec) -> Self {
        let mut s = Self {
            kind: spec.kind,
            name: spec.name.clone(),
            unique_id: spec.unique_id.clone(),
            reachable: spec.reachable,
            contact: false,
            occupied: false,
            on_off: false,
            level: 0,
        };
        for a in &spec.attrs {
            s.set(*a);
        }
        s
    }

    /// Stores a value; returns whether it changed. Values that do not suit the endpoint's kind
    /// are ignored (the host never sends them; this keeps a bug from corrupting another cluster).
    fn set(&mut self, value: AttrValue) -> bool {
        let slot: Option<(&mut bool, bool)> = match (self.kind, value) {
            (UnitKind::ContactSensor, AttrValue::BooleanState(b)) => Some((&mut self.contact, b)),
            (UnitKind::OccupancySensor, AttrValue::Occupancy(b)) => Some((&mut self.occupied, b)),
            (UnitKind::OnOffPlugin | UnitKind::Speaker, AttrValue::OnOff(b)) => Some((&mut self.on_off, b)),
            _ => None,
        };
        if let Some((slot, new)) = slot {
            return std::mem::replace(slot, new) != new;
        }
        if let (UnitKind::Speaker, AttrValue::CurrentLevel(l)) = (self.kind, value) {
            return std::mem::replace(&mut self.level, l) != l;
        }
        false
    }
}

/// What a host event changed, for notifying subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Changed {
    /// An endpoint appeared or disappeared (the Aggregator's parts list changed).
    Topology {
        endpoint: u16,
    },
    Reachable {
        endpoint: u16,
    },
    Contact {
        endpoint: u16,
    },
    Occupancy {
        endpoint: u16,
    },
    OnOff {
        endpoint: u16,
    },
    Level {
        endpoint: u16,
    },
}

#[derive(Debug, Default)]
pub struct Table {
    endpoints: BTreeMap<u16, EndpointState>,
}

impl Table {
    pub fn get(&self, endpoint: u16) -> Option<&EndpointState> {
        self.endpoints.get(&endpoint)
    }

    /// Endpoint ids in increasing order (rs-matter requires that order).
    pub fn ids(&self) -> impl Iterator<Item = (u16, UnitKind)> + '_ {
        self.endpoints.iter().map(|(id, s)| (*id, s.kind))
    }

    pub fn apply(&mut self, event: FrontendEvent) -> Vec<Changed> {
        match event {
            FrontendEvent::AddEndpoint(spec) => {
                self.endpoints.insert(spec.endpoint, EndpointState::new(&spec));
                vec![Changed::Topology { endpoint: spec.endpoint }]
            }
            FrontendEvent::RemoveEndpoint { endpoint } => {
                if self.endpoints.remove(&endpoint).is_some() {
                    vec![Changed::Topology { endpoint }]
                } else {
                    Vec::new()
                }
            }
            FrontendEvent::SetReachable { endpoint, reachable } => match self.endpoints.get_mut(&endpoint) {
                Some(s) if s.reachable != reachable => {
                    s.reachable = reachable;
                    vec![Changed::Reachable { endpoint }]
                }
                _ => Vec::new(),
            },
            FrontendEvent::SetAttr { endpoint, value } => {
                let Some(s) = self.endpoints.get_mut(&endpoint) else { return Vec::new() };
                if !s.set(value) {
                    return Vec::new();
                }
                vec![match value {
                    AttrValue::BooleanState(_) => Changed::Contact { endpoint },
                    AttrValue::Occupancy(_) => Changed::Occupancy { endpoint },
                    AttrValue::OnOff(_) => Changed::OnOff { endpoint },
                    AttrValue::CurrentLevel(_) => Changed::Level { endpoint },
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(endpoint: u16, kind: UnitKind, attrs: Vec<AttrValue>) -> EndpointSpec {
        EndpointSpec { endpoint, kind, name: "n".into(), unique_id: "d/u".into(), reachable: true, attrs }
    }
    fn add(t: &mut Table, endpoint: u16, kind: UnitKind, attrs: Vec<AttrValue>) -> Vec<Changed> {
        t.apply(FrontendEvent::AddEndpoint(spec(endpoint, kind, attrs)))
    }

    #[test]
    fn adding_stores_the_initial_values_and_reports_a_topology_change() {
        let mut t = Table::default();
        let changed = add(&mut t, 4, UnitKind::Speaker, vec![AttrValue::CurrentLevel(127), AttrValue::OnOff(true)]);
        assert_eq!(changed, vec![Changed::Topology { endpoint: 4 }]);
        let s = t.get(4).unwrap();
        assert_eq!((s.level, s.on_off, s.reachable), (127, true, true));
        assert_eq!(s.unique_id, "d/u");
    }

    #[test]
    fn ids_are_sorted() {
        let mut t = Table::default();
        add(&mut t, 9, UnitKind::ContactSensor, vec![]);
        add(&mut t, 3, UnitKind::OnOffPlugin, vec![]);
        add(&mut t, 5, UnitKind::OccupancySensor, vec![]);
        assert_eq!(
            t.ids().collect::<Vec<_>>(),
            vec![(3, UnitKind::OnOffPlugin), (5, UnitKind::OccupancySensor), (9, UnitKind::ContactSensor)]
        );
    }

    #[test]
    fn removing_reports_only_when_something_was_removed() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::ContactSensor, vec![]);
        assert_eq!(t.apply(FrontendEvent::RemoveEndpoint { endpoint: 2 }), vec![Changed::Topology { endpoint: 2 }]);
        assert!(t.apply(FrontendEvent::RemoveEndpoint { endpoint: 2 }).is_empty());
        assert!(t.get(2).is_none());
    }

    #[test]
    fn attribute_changes_are_reported_once_and_unchanged_values_are_silent() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::ContactSensor, vec![]);
        let set = |t: &mut Table, v| t.apply(FrontendEvent::SetAttr { endpoint: 2, value: v });
        assert_eq!(set(&mut t, AttrValue::BooleanState(true)), vec![Changed::Contact { endpoint: 2 }]);
        assert!(set(&mut t, AttrValue::BooleanState(true)).is_empty());
        assert!(t.get(2).unwrap().contact);
    }

    #[test]
    fn each_kind_changes_its_own_attribute() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::OccupancySensor, vec![]);
        add(&mut t, 3, UnitKind::OnOffPlugin, vec![]);
        add(&mut t, 4, UnitKind::Speaker, vec![]);
        let set = |t: &mut Table, endpoint, value| t.apply(FrontendEvent::SetAttr { endpoint, value });
        assert_eq!(set(&mut t, 2, AttrValue::Occupancy(true)), vec![Changed::Occupancy { endpoint: 2 }]);
        assert_eq!(set(&mut t, 3, AttrValue::OnOff(true)), vec![Changed::OnOff { endpoint: 3 }]);
        assert_eq!(set(&mut t, 4, AttrValue::CurrentLevel(9)), vec![Changed::Level { endpoint: 4 }]);
        assert_eq!(set(&mut t, 4, AttrValue::OnOff(true)), vec![Changed::OnOff { endpoint: 4 }]);
    }

    #[test]
    fn values_that_do_not_suit_the_endpoint_are_ignored() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::ContactSensor, vec![]);
        add(&mut t, 3, UnitKind::OnOffPlugin, vec![]);
        let set = |t: &mut Table, endpoint, value| t.apply(FrontendEvent::SetAttr { endpoint, value });
        assert!(set(&mut t, 2, AttrValue::OnOff(true)).is_empty());
        assert!(set(&mut t, 3, AttrValue::CurrentLevel(5)).is_empty());
        assert!(set(&mut t, 3, AttrValue::Occupancy(true)).is_empty());
        assert!(!t.get(2).unwrap().on_off && t.get(3).unwrap().level == 0);
    }

    #[test]
    fn unknown_endpoints_are_ignored() {
        let mut t = Table::default();
        assert!(t.apply(FrontendEvent::SetAttr { endpoint: 7, value: AttrValue::OnOff(true) }).is_empty());
        assert!(t.apply(FrontendEvent::SetReachable { endpoint: 7, reachable: false }).is_empty());
    }

    #[test]
    fn reachability_changes_are_reported_once() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::ContactSensor, vec![]);
        let r = |t: &mut Table, reachable| t.apply(FrontendEvent::SetReachable { endpoint: 2, reachable });
        assert_eq!(r(&mut t, false), vec![Changed::Reachable { endpoint: 2 }]);
        assert!(r(&mut t, false).is_empty());
        assert_eq!(r(&mut t, true), vec![Changed::Reachable { endpoint: 2 }]);
    }

    #[test]
    fn re_adding_an_endpoint_replaces_it() {
        let mut t = Table::default();
        add(&mut t, 2, UnitKind::ContactSensor, vec![AttrValue::BooleanState(true)]);
        add(&mut t, 2, UnitKind::ContactSensor, vec![]);
        assert!(!t.get(2).unwrap().contact);
        assert_eq!(t.ids().count(), 1);
    }
}
