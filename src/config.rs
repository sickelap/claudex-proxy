use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::AppError;
use crate::application::StaticModelRegistry;
use crate::domain::{ModelMapping, ProviderKind};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    models: Vec<ModelMapping>,
}

pub fn load_registry(cli_path: Option<&Path>) -> Result<StaticModelRegistry, AppError> {
    let env_path = std::env::var_os("CLAUDEX_CONFIG");
    let mappings = load_mappings_from(cli_path, env_path)?;
    StaticModelRegistry::new(mappings).map_err(|error| AppError(error.message))
}

fn load_mappings_from(
    cli_path: Option<&Path>,
    env_path: Option<OsString>,
) -> Result<Vec<ModelMapping>, AppError> {
    let selected = cli_path.map(Path::to_path_buf).or_else(|| {
        env_path
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    match selected {
        Some(path) => load_file(&path),
        None => Ok(builtin_mappings()),
    }
}

fn load_file(path: &Path) -> Result<Vec<ModelMapping>, AppError> {
    let content = fs::read_to_string(path).map_err(|error| {
        AppError(format!(
            "failed to read model registry {}: {error}",
            path.display()
        ))
    })?;
    let file: RegistryFile = serde_json::from_str(&content).map_err(|error| {
        AppError(format!(
            "invalid model registry {}: {error}",
            path.display()
        ))
    })?;
    StaticModelRegistry::new(file.models.clone()).map_err(|error| AppError(error.message))?;
    Ok(file.models)
}

pub fn builtin_mappings() -> Vec<ModelMapping> {
    [
        "gpt-5.5",
        "gpt-5.5-fast",
        "gpt-5.6-luna",
        "gpt-5.6-terra",
        "gpt-5.6-sol",
    ]
    .into_iter()
    .map(|id| ModelMapping {
        id: id.into(),
        provider: ProviderKind::Codex,
        upstream_model: id.into(),
        capabilities: ["text", "tools", "reasoning", "streaming"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        supported_options: [
            "codex.reasoning_effort",
            "codex.reasoning_summary",
            "codex.text_verbosity",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        default_max_output_tokens: None,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ModelRegistry;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    fn write_config(value: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "claudex-registry-{}-{}.json",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, value).unwrap();
        path
    }

    fn valid(id: &str, provider: &str) -> String {
        format!(
            r#"{{"models":[{{"id":"{id}","provider":"{provider}","upstream_model":"upstream","capabilities":[],"supported_options":[]}}]}}"#
        )
    }

    #[test]
    fn loads_valid_mapping_and_rejects_duplicates_and_invalid_providers() {
        let path = write_config(&valid("public", "anthropic"));
        let mappings = load_mappings_from(Some(&path), None).unwrap();
        assert_eq!(mappings[0].provider, ProviderKind::Anthropic);
        let _ = fs::remove_file(path);

        let duplicate = write_config(
            r#"{"models":[{"id":"same","provider":"codex","upstream_model":"a"},{"id":"same","provider":"anthropic","upstream_model":"b"}]}"#,
        );
        assert!(
            load_mappings_from(Some(&duplicate), None)
                .unwrap_err()
                .0
                .contains("duplicate")
        );
        let _ = fs::remove_file(duplicate);

        let invalid = write_config(&valid("public", "other"));
        assert!(
            load_mappings_from(Some(&invalid), None)
                .unwrap_err()
                .0
                .contains("unknown variant")
        );
        let _ = fs::remove_file(invalid);
    }

    #[test]
    fn reports_missing_files_and_cli_takes_precedence_over_environment() {
        let missing = std::env::temp_dir().join("claudex-definitely-missing.json");
        assert!(
            load_mappings_from(Some(&missing), None)
                .unwrap_err()
                .0
                .contains("failed to read")
        );

        let cli = write_config(&valid("cli", "codex"));
        let env = write_config(&valid("env", "anthropic"));
        let mappings = load_mappings_from(Some(&cli), Some(env.clone().into_os_string())).unwrap();
        assert_eq!(mappings[0].id, "cli");
        let _ = fs::remove_file(cli);
        let _ = fs::remove_file(env);
    }

    #[test]
    fn builtins_are_conservative_and_resolvable() {
        let mappings = load_mappings_from(None, None).unwrap();
        assert!(
            mappings
                .iter()
                .all(|mapping| mapping.provider == ProviderKind::Codex)
        );
        let registry = StaticModelRegistry::new(mappings.clone()).unwrap();
        for mapping in mappings {
            assert!(registry.resolve(&mapping.id).is_ok());
        }
        let capabilities: BTreeSet<_> = builtin_mappings()
            .into_iter()
            .flat_map(|mapping| mapping.capabilities)
            .collect();
        assert!(capabilities.contains("streaming"));
    }

    #[test]
    fn every_checked_in_example_model_resolves() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("claudex.config.example.json");
        let mappings = load_mappings_from(Some(&path), None).unwrap();
        let registry = StaticModelRegistry::new(mappings.clone()).unwrap();
        assert!(!mappings.is_empty());
        for mapping in mappings {
            let resolved = registry.resolve(&mapping.id).unwrap();
            assert_eq!(resolved.upstream_model, mapping.upstream_model);
        }
    }

    #[test]
    fn documented_json_and_sample_files_are_valid_and_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "claudex.config.example.json",
            "examples/anthropic-messages-request.json",
            "opencode.json.sample",
        ] {
            let content = fs::read_to_string(root.join(relative))
                .unwrap_or_else(|error| panic!("missing documented sample {relative}: {error}"));
            serde_json::from_str::<serde_json::Value>(&content)
                .unwrap_or_else(|error| panic!("invalid JSON in {relative}: {error}"));
        }
        assert!(root.join("claude-code.env.sample").is_file());
        let metadata_example: serde_json::Value = serde_json::from_str(
            r#"{"include_metadata":true,"session_id":"my-opaque-correlation-id"}"#,
        )
        .unwrap();
        assert_eq!(metadata_example["include_metadata"], true);
    }
}
