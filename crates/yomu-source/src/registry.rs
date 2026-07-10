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
    /// Load all source definitions from `dir`. Broken definitions are
    /// skipped and returned as messages for the caller to log loudly: a
    /// typo removes that one source until it's fixed, but must not take
    /// the server — and every other source with it — down (a definition
    /// written for a newer engine once crashlooped the whole service).
    pub fn load(dir: &Path) -> Result<(Self, Vec<String>)> {
        let mut registry = Self::default();
        let mut broken = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // Missing dir = no sources yet; that's a valid empty setup.
            Err(_) => return Ok((registry, broken)),
        };
        for entry in entries {
            let entry = entry
                .map_err(|e| SourceError::Definition(format!("reading {}: {e}", dir.display())))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let mut load_one = || -> Result<()> {
                let raw = std::fs::read_to_string(&path)
                    .map_err(|e| SourceError::Definition(e.to_string()))?;
                let spec: SelectorSpec =
                    toml::from_str(&raw).map_err(|e| SourceError::Definition(e.to_string()))?;
                registry.insert(Arc::new(SelectorSource::new(spec)?))
            };
            if let Err(e) = load_one() {
                broken.push(format!("{}: {e}", path.display()));
            }
        }
        Ok((registry, broken))
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

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        id = "good"
        name = "Good"
        base_url = "https://good.test"
        [search]
        url = "{base}/?s={query}"
        item = ".card"
        link = "a@href"
        [manga]
        chapter_item = "li"
        chapter_link = "a@href"
        [pages]
        image = "img@src"
    "#;

    #[test]
    fn broken_definitions_are_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("yomu-registry-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.toml"), GOOD).unwrap();
        // written for a newer engine: unknown field
        std::fs::write(
            dir.join("bad.toml"),
            GOOD.replace("id = \"good\"", "id = \"bad\"\nfrom_the_future = true"),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not a definition").unwrap();

        let (registry, broken) = Registry::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(registry.get("good").is_some());
        assert!(registry.get("bad").is_none());
        assert_eq!(broken.len(), 1, "{broken:?}");
        assert!(broken[0].contains("bad.toml"), "{broken:?}");
        assert!(broken[0].contains("from_the_future"), "{broken:?}");
    }
}
