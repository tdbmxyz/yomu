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
        for entry in entries {
            let entry = entry
                .map_err(|e| SourceError::Definition(format!("reading {}: {e}", dir.display())))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| SourceError::Definition(format!("{}: {e}", path.display())))?;
            let spec: SelectorSpec = toml::from_str(&raw)
                .map_err(|e| SourceError::Definition(format!("{}: {e}", path.display())))?;
            registry
                .insert(Arc::new(SelectorSource::new(spec)?))
                .map_err(|e| SourceError::Definition(format!("{}: {e}", path.display())))?;
        }
        Ok(registry)
    }

    /// Register a source. Fails on a duplicate id (two definitions silently
    /// shadowing each other would flip library entries between sites) and on
    /// ids that can't appear in a URL path segment.
    pub fn insert(&mut self, source: Arc<dyn Source>) -> Result<()> {
        let id = source.id().to_string();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(SourceError::Definition(format!(
                "source id {id:?} must be a slug (alphanumeric, '-', '_')"
            )));
        }
        if self.sources.contains_key(&id) {
            return Err(SourceError::Definition(format!(
                "duplicate source id {id:?}"
            )));
        }
        self.sources.insert(id, source);
        Ok(())
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
