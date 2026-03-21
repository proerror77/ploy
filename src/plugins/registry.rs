use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::domain::Domain;
use crate::error::{PloyError, Result};

use super::definition::{PluginDefinition, PluginKind};

#[derive(Debug, Default, Clone)]
pub struct PluginRegistry {
    definitions: Vec<PluginDefinition>,
    index: HashMap<String, usize>,
}

impl PluginRegistry {
    pub fn builtin_runtime_registry() -> Result<Self> {
        Self::from_definitions(vec![
            builtin_definition(
                "crypto.momentum.v1",
                PluginKind::ComposableCrypto,
                Domain::Crypto,
            ),
            builtin_definition(
                "crypto.pattern_memory.v1",
                PluginKind::ComposableCrypto,
                Domain::Crypto,
            ),
            builtin_definition(
                "crypto.split_arb.v1",
                PluginKind::ComposableCrypto,
                Domain::Crypto,
            ),
            builtin_definition(
                "politics.event_edge.v1",
                PluginKind::RegisteredStrategy,
                Domain::Politics,
            ),
            builtin_definition(
                "sports.nba_comeback.v1",
                PluginKind::RegisteredStrategy,
                Domain::Sports,
            ),
        ])
    }

    pub fn load_from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let mut entries =
            fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, std::io::Error>>()?;
        entries.sort_by_key(|entry| entry.path());

        let mut definitions = Vec::new();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            let definition: PluginDefinition = toml::from_str(&content).map_err(|err| {
                PloyError::Validation(format!(
                    "invalid plugin definition {}: {err}",
                    path.display()
                ))
            })?;
            definitions.push(definition);
        }

        Self::from_definitions(definitions)
    }

    pub fn plugin(&self, plugin_id: &str) -> Option<&PluginDefinition> {
        self.index
            .get(plugin_id)
            .and_then(|idx| self.definitions.get(*idx))
    }

    pub fn definitions(&self) -> &[PluginDefinition] {
        &self.definitions
    }

    pub fn from_definitions(definitions: Vec<PluginDefinition>) -> Result<Self> {
        let mut index = HashMap::new();
        for (idx, definition) in definitions.iter().enumerate() {
            if let Some(previous_idx) = index.insert(definition.plugin_id.clone(), idx) {
                let previous = &definitions[previous_idx];
                return Err(PloyError::Validation(format!(
                    "duplicate plugin_id: {} (already defined for domain {} version {})",
                    definition.plugin_id, previous.domain, previous.version
                )));
            }
        }

        Ok(Self { definitions, index })
    }
}

fn builtin_definition(plugin_id: &str, kind: PluginKind, domain: Domain) -> PluginDefinition {
    PluginDefinition {
        plugin_id: plugin_id.to_string(),
        kind,
        version: "v1".to_string(),
        domain,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::PluginRegistry;

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ploy-plugin-registry-{test_name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_plugin(dir: &Path, file_name: &str, body: &str) {
        fs::write(dir.join(file_name), body).expect("write plugin definition");
    }

    #[test]
    fn load_from_dir_reads_composable_crypto_definition() {
        let dir = make_temp_dir("composable-crypto");
        write_plugin(
            &dir,
            "momentum.toml",
            r#"
plugin_id = "crypto.momentum.v1"
kind = "composable_crypto"
version = "v1"
domain = "crypto"
"#,
        );

        let registry = PluginRegistry::load_from_dir(&dir).expect("load plugin registry");

        assert_eq!(registry.definitions().len(), 1);
        let plugin = registry
            .plugin("crypto.momentum.v1")
            .expect("plugin definition exists");
        assert_eq!(plugin.plugin_id, "crypto.momentum.v1");
    }

    #[test]
    fn load_from_dir_reads_registered_strategy_definition() {
        let dir = make_temp_dir("registered-strategy");
        write_plugin(
            &dir,
            "event-edge.toml",
            r#"
plugin_id = "politics.event_edge.v1"
kind = "registered_strategy"
version = "v1"
domain = "politics"
"#,
        );

        let registry = PluginRegistry::load_from_dir(&dir).expect("load plugin registry");

        assert_eq!(registry.definitions().len(), 1);
        let plugin = registry
            .plugin("politics.event_edge.v1")
            .expect("plugin definition exists");
        assert_eq!(plugin.plugin_id, "politics.event_edge.v1");
    }

    #[test]
    fn load_from_dir_rejects_duplicate_plugin_ids() {
        let dir = make_temp_dir("duplicate-plugin-id");
        let body = r#"
plugin_id = "crypto.shared.v1"
kind = "composable_crypto"
version = "v1"
domain = "crypto"
"#;
        write_plugin(&dir, "one.toml", body);
        write_plugin(&dir, "two.toml", body);

        let err = PluginRegistry::load_from_dir(&dir).expect_err("duplicate plugin_id");

        assert!(err.to_string().contains("duplicate plugin_id"));
    }

    #[test]
    fn load_from_dir_rejects_unknown_plugin_kind() {
        let dir = make_temp_dir("unknown-kind");
        write_plugin(
            &dir,
            "bad.toml",
            r#"
plugin_id = "crypto.invalid.v1"
kind = "surprise_mode"
version = "v1"
domain = "crypto"
"#,
        );

        let err = PluginRegistry::load_from_dir(&dir).expect_err("unknown kind");

        assert!(err.to_string().contains("unknown plugin kind"));
    }

    #[test]
    fn builtin_runtime_registry_includes_registered_strategy_plugins() {
        let registry = PluginRegistry::builtin_runtime_registry().expect("builtin registry");

        assert!(registry.plugin("politics.event_edge.v1").is_some());
        assert!(registry.plugin("sports.nba_comeback.v1").is_some());
    }
}
