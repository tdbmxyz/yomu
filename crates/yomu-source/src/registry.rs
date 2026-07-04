//! Loads every `*.toml` in a directory into a set of ready-to-use sources.
//! Adding a scan site = dropping one TOML file there and restarting.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::selector::{SelectorSource, SelectorSpec};
use crate::{Result, Source, SourceError};

#[derive(Default)]
pub struct Registry {
    sources: BTreeMap<String, Arc<dyn Source>>,
}

impl Registry {
    /// Load all source definitions from `dir`. A broken definition fails
    /// loudly rather than being skipped: a typo should not silently remove
    /// a source (and its library entries) from the server.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut registry = Self::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // Missing dir = no sources yet; that's a valid empty setup.
            Err(_) => return Ok(registry),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| SourceError::Definition(format!("{}: {e}", path.display())))?;
            let spec: SelectorSpec = toml::from_str(&raw)
                .map_err(|e| SourceError::Definition(format!("{}: {e}", path.display())))?;
            registry.insert(Arc::new(SelectorSource::new(spec)?));
        }
        Ok(registry)
    }

    pub fn insert(&mut self, source: Arc<dyn Source>) {
        self.sources.insert(source.id().to_string(), source);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Source>> {
        self.sources.get(id).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Source>> {
        self.sources.values()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}
