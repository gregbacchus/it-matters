//! Stable Matter endpoint ids for bridged units.
//!
//! An id is keyed by `(device id, unit id, shape)` and, once handed out, belongs to that key
//! forever: it is persisted, never given to another key, and given back unchanged when the same
//! unit re-registers. A unit whose cluster shape changes is a different key and gets a new id,
//! because rs-matter requires an endpoint's cluster shape to stay fixed for its lifetime.
//!
//! Ids start at 2 (0 is the root endpoint, 1 the aggregator).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// First id handed out.
pub const FIRST_ENDPOINT_ID: u16 = 2;
/// Last valid Matter endpoint id (0xFFFF is reserved).
pub const LAST_ENDPOINT_ID: u16 = 0xFFFE;

/// Where the allocation table is kept.
pub trait Store {
    /// The saved text, or `None` if nothing was saved yet.
    fn load(&self) -> Result<Option<String>, String>;
    /// Replaces the saved text. Must not leave a half-written value behind on failure.
    fn save(&mut self, text: &str) -> Result<(), String>;
}

/// Keeps the table in memory (tests, and a bridge without persistence).
#[derive(Debug, Default, Clone)]
pub struct MemoryStore {
    pub text: Option<String>,
    /// Makes `save` fail, to test error paths.
    pub fail_saves: bool,
}

impl Store for MemoryStore {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.text.clone())
    }
    fn save(&mut self, text: &str) -> Result<(), String> {
        if self.fail_saves {
            return Err("simulated save failure".into());
        }
        self.text = Some(text.to_owned());
        Ok(())
    }
}

/// Keeps the table in a file, replaced atomically (write to a temporary file, sync, rename).
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Store for FileStore {
    fn load(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    fn save(&mut self, text: &str) -> Result<(), String> {
        use std::io::Write;
        let err = |e: std::io::Error| format!("{}: {e}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp).map_err(err)?;
        f.write_all(text.as_bytes()).map_err(err)?;
        f.sync_all().map_err(err)?;
        std::fs::rename(&tmp, &self.path).map_err(err)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    device: String,
    unit: String,
    shape: String,
    id: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct Table {
    next: u32,
    entries: Vec<Entry>,
}

type Key = (String, String, String);

/// The allocation table plus its store.
pub struct Allocator<S: Store> {
    store: S,
    next: u32,
    ids: BTreeMap<Key, u16>,
}

impl<S: Store> Allocator<S> {
    /// Loads the saved table. A table that exists but cannot be read is an error: starting fresh
    /// would hand out ids that controllers already know under other meanings.
    pub fn open(store: S) -> Result<Self, String> {
        let Some(text) = store.load()? else {
            return Ok(Self { store, next: u32::from(FIRST_ENDPOINT_ID), ids: BTreeMap::new() });
        };
        let table: Table = serde_json::from_str(&text).map_err(|e| format!("endpoint id table is corrupt: {e}"))?;
        let mut ids = BTreeMap::new();
        let mut used = std::collections::HashSet::new();
        let mut max = 0u32;
        for e in table.entries {
            if !(FIRST_ENDPOINT_ID..=LAST_ENDPOINT_ID).contains(&e.id) {
                return Err(format!("endpoint id table is corrupt: id {} is out of range", e.id));
            }
            if !used.insert(e.id) || ids.insert((e.device, e.unit, e.shape), e.id).is_some() {
                return Err("endpoint id table is corrupt: duplicate entry".into());
            }
            max = max.max(u32::from(e.id));
        }
        // Never trust `next` to be ahead of the entries.
        let next = table.next.max(max + 1).max(u32::from(FIRST_ENDPOINT_ID));
        Ok(Self { store, next, ids })
    }

    /// The id for a unit, allocating and persisting a new one if it has none yet. If saving
    /// fails nothing changes and the error is returned.
    pub fn get_or_allocate(&mut self, device: &str, unit: &str, shape: &str) -> Result<u16, String> {
        let key = (device.to_owned(), unit.to_owned(), shape.to_owned());
        if let Some(&id) = self.ids.get(&key) {
            return Ok(id);
        }
        if self.next > u32::from(LAST_ENDPOINT_ID) {
            return Err("no endpoint ids left".into());
        }
        let id = self.next as u16;
        self.ids.insert(key.clone(), id);
        let previous_next = self.next;
        self.next += 1;
        if let Err(e) = self.persist() {
            self.ids.remove(&key);
            self.next = previous_next;
            return Err(e);
        }
        Ok(id)
    }

    /// The id for a unit, if it has one.
    pub fn lookup(&self, device: &str, unit: &str, shape: &str) -> Option<u16> {
        self.ids.get(&(device.to_owned(), unit.to_owned(), shape.to_owned())).copied()
    }

    fn persist(&mut self) -> Result<(), String> {
        let entries = self
            .ids
            .iter()
            .map(|((device, unit, shape), id)| Entry {
                device: device.clone(),
                unit: unit.clone(),
                shape: shape.clone(),
                id: *id,
            })
            .collect();
        let text = serde_json::to_string_pretty(&Table { next: self.next, entries }).map_err(|e| e.to_string())?;
        self.store.save(&text)
    }

    pub fn into_store(self) -> S {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc() -> Allocator<MemoryStore> {
        Allocator::open(MemoryStore::default()).unwrap()
    }

    #[test]
    fn ids_start_at_two_and_increase() {
        let mut a = alloc();
        assert_eq!(a.get_or_allocate("pc", "volume", "speaker:volume+mute"), Ok(2));
        assert_eq!(a.get_or_allocate("pc", "online", "contact_sensor:online"), Ok(3));
    }

    #[test]
    fn the_same_unit_gets_the_same_id_back() {
        let mut a = alloc();
        let id = a.get_or_allocate("pc", "online", "contact_sensor:online").unwrap();
        a.get_or_allocate("other", "x", "contact_sensor:x").unwrap();
        assert_eq!(a.get_or_allocate("pc", "online", "contact_sensor:online"), Ok(id));
        assert_eq!(a.lookup("pc", "online", "contact_sensor:online"), Some(id));
        assert_eq!(a.lookup("pc", "online", "other-shape"), None);
    }

    #[test]
    fn a_changed_shape_gets_a_new_id_and_the_old_one_is_never_reused() {
        let mut a = alloc();
        let old = a.get_or_allocate("pc", "mic", "contact_sensor:mic").unwrap();
        let new = a.get_or_allocate("pc", "mic", "occupancy_sensor:mic").unwrap();
        assert_ne!(old, new);
        let third = a.get_or_allocate("pc", "other", "contact_sensor:other").unwrap();
        assert!(third > new, "ids only grow");
        // The old shape still maps to its original id.
        assert_eq!(a.get_or_allocate("pc", "mic", "contact_sensor:mic"), Ok(old));
    }

    #[test]
    fn ids_survive_a_restart() {
        let mut a = alloc();
        let x = a.get_or_allocate("pc", "a", "s").unwrap();
        let y = a.get_or_allocate("pc", "b", "s").unwrap();
        let mut b = Allocator::open(a.into_store()).unwrap();
        assert_eq!(b.lookup("pc", "a", "s"), Some(x));
        assert_eq!(b.get_or_allocate("pc", "b", "s"), Ok(y));
        assert_eq!(b.get_or_allocate("pc", "c", "s"), Ok(y + 1), "the counter was restored too");
    }

    #[test]
    fn a_failed_save_changes_nothing() {
        let mut a = Allocator::open(MemoryStore { fail_saves: true, ..Default::default() }).unwrap();
        assert!(a.get_or_allocate("pc", "a", "s").is_err());
        assert_eq!(a.lookup("pc", "a", "s"), None);
        let mut store = a.into_store();
        store.fail_saves = false;
        let mut a = Allocator::open(store).unwrap();
        assert_eq!(a.get_or_allocate("pc", "a", "s"), Ok(2), "the id was not burned");
    }

    #[test]
    fn a_corrupt_table_is_an_error_not_a_reset() {
        let bad = |text: &str| Allocator::open(MemoryStore { text: Some(text.into()), ..Default::default() }).err();
        assert!(bad("not json").unwrap().contains("corrupt"));
        assert!(
            bad(r#"{"next":3,"entries":[{"device":"a","unit":"u","shape":"s","id":1}]}"#)
                .unwrap()
                .contains("out of range")
        );
        let dup = r#"{"next":9,"entries":[{"device":"a","unit":"u","shape":"s","id":2},{"device":"b","unit":"u","shape":"s","id":2}]}"#;
        assert!(bad(dup).unwrap().contains("duplicate"));
    }

    #[test]
    fn a_stale_counter_cannot_cause_reuse() {
        let text = r#"{"next":2,"entries":[{"device":"a","unit":"u","shape":"s","id":7}]}"#;
        let mut a = Allocator::open(MemoryStore { text: Some(text.into()), ..Default::default() }).unwrap();
        assert_eq!(a.get_or_allocate("b", "u", "s"), Ok(8));
    }

    #[test]
    fn running_out_of_ids_is_an_error() {
        let text = format!(r#"{{"next":{},"entries":[]}}"#, u32::from(LAST_ENDPOINT_ID) + 1);
        let mut a = Allocator::open(MemoryStore { text: Some(text), ..Default::default() }).unwrap();
        assert!(a.get_or_allocate("a", "u", "s").unwrap_err().contains("no endpoint ids left"));
    }

    #[test]
    fn file_store_round_trips_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("it-matters-alloc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/endpoints.json");
        let mut a = Allocator::open(FileStore::new(&path)).unwrap();
        let id = a.get_or_allocate("pc", "online", "s").unwrap();
        assert!(path.is_file() && !path.with_extension("tmp").exists(), "no temporary file left behind");
        let b = Allocator::open(FileStore::new(&path)).unwrap();
        assert_eq!(b.lookup("pc", "online", "s"), Some(id));
        std::fs::write(&path, "garbage").unwrap();
        assert!(Allocator::open(FileStore::new(&path)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
