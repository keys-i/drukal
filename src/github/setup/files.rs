use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use super::SourceRef;
use crate::Result;

const MAX_CONFIGURATION_BYTES: u64 = 64 * 1024;

pub fn checks(values: &[String]) -> Result<Vec<String>> {
    if values.is_empty() || values.len() > 32 || values.iter().any(|value| !valid_check(value)) {
        bail!("provide nonempty CI checks that do not name Koelu dependasolve itself");
    }
    let mut seen = BTreeSet::new();
    let checks = values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect::<Vec<_>>();
    if checks.len() > 32 {
        bail!("provide at most 32 CI checks");
    }
    Ok(checks)
}

fn valid_check(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 200
        && !value.chars().any(char::is_control)
        && !value.starts_with("Koelu dependasolve")
}

pub fn local_files(
    directory: &Path,
    source: &SourceRef,
    required: &[String],
    overwrite: bool,
) -> Result<BTreeMap<PathBuf, String>> {
    setup_files(directory, source, required, overwrite, None, false)
}

pub(super) fn existing_configuration(directory: &Path) -> Result<Option<Value>> {
    let root = directory
        .canonicalize()
        .context("--directory must be an existing directory")?;
    if !root.is_dir() {
        bail!("--directory must be a directory");
    }
    for (name, legacy) in [(".github/koelu.toml", false), (".github/koelu.json", true)] {
        let config = safe_path(&root, name)?;
        match fs::symlink_metadata(&config) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let text = read_configuration(&config, &metadata)?;
                return Ok(if legacy {
                    serde_json::from_str(&text).ok()
                } else {
                    toml::from_str(&text).ok()
                });
            }
            Ok(_) => bail!("refusing to read existing content: {}", config.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

pub(super) fn refuse_existing_configuration(directory: &Path, overwrite: bool) -> Result<()> {
    if overwrite {
        return Ok(());
    }
    let root = directory
        .canonicalize()
        .context("--directory must be an existing directory")?;
    if !root.is_dir() {
        bail!("--directory must be a directory");
    }
    for name in [".github/koelu.toml", ".github/koelu.json"] {
        let config = safe_path(&root, name)?;
        match fs::symlink_metadata(&config) {
            Ok(_) => bail!(
                "refusing to overwrite existing content: {}",
                config.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn setup_files(
    directory: &Path,
    source: &SourceRef,
    required: &[String],
    overwrite: bool,
    agreement: Option<&Value>,
    autofix: bool,
) -> Result<BTreeMap<PathBuf, String>> {
    let root = directory
        .canonicalize()
        .context("--directory must be an existing directory")?;
    if !root.is_dir() {
        bail!("--directory must be a directory");
    }
    let mut configuration =
        json!({"schema": 1, "source": source.joined(), "checks": required, "autofix": autofix});
    if let Some(agreement) = agreement {
        configuration["agreement"] = agreement.clone();
    }
    let configuration = toml::to_string_pretty(&configuration)?;
    let config = safe_path(&root, ".github/koelu.toml")?;
    let mut files = BTreeMap::from([(config.clone(), configuration)]);
    let dependabot = [
        safe_path(&root, ".github/dependabot.yml")?,
        safe_path(&root, ".github/dependabot.yaml")?,
    ];
    if !dependabot.iter().any(|path| path.exists()) {
        files.insert(dependabot[0].clone(), dependabot_config(&root)?);
    }
    for (path, content) in &files {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file()
                    || (read_configuration(path, &metadata)? != *content
                        && (!overwrite || path != &config))
                {
                    bail!("refusing to overwrite existing content: {}", path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(files)
}

fn read_configuration(path: &Path, metadata: &fs::Metadata) -> Result<String> {
    if metadata.len() > MAX_CONFIGURATION_BYTES {
        bail!(
            "existing Koelu configuration is too large: {}",
            path.display()
        );
    }
    fs::read_to_string(path).map_err(Into::into)
}

const DEPENDABOT_ECOSYSTEMS: &[(&str, &[&str], &[&str])] = &[
    ("cargo", &["Cargo.toml"], &[]),
    ("npm", &["package.json"], &[]),
    (
        "pip",
        &["pyproject.toml", "requirements.txt", "Pipfile"],
        &[],
    ),
    ("bundler", &["Gemfile"], &[]),
    ("gomod", &["go.mod"], &[]),
    ("maven", &["pom.xml"], &[]),
    ("gradle", &["build.gradle", "build.gradle.kts"], &[]),
    ("composer", &["composer.json"], &[]),
    (
        "nuget",
        &["packages.config"],
        &["csproj", "fsproj", "vbproj"],
    ),
    ("docker", &["Dockerfile"], &[]),
];

fn dependabot_config(root: &Path) -> Result<String> {
    let directories = manifest_directories(root)?;
    let mut updates = dependabot_update("github-actions", "/");
    for (ecosystem, manifests, extensions) in DEPENDABOT_ECOSYSTEMS {
        for (directory, path) in &directories {
            if has_manifest(path, manifests, extensions)? {
                updates.push_str(&dependabot_update(ecosystem, directory));
            }
        }
    }
    Ok(format!("version: 2\nupdates:\n{updates}"))
}

fn manifest_directories(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut directories = BTreeMap::from([("/".to_owned(), root.to_path_buf())]);
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let path = entry.path();
            directories.insert(format!("/{}", entry.file_name().to_string_lossy()), path);
        }
    }
    Ok(directories)
}

fn has_manifest(directory: &Path, names: &[&str], extensions: &[&str]) -> Result<bool> {
    if names.iter().any(|name| directory.join(name).is_file()) {
        return Ok(true);
    }
    if extensions.is_empty() {
        return Ok(false);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extensions.contains(&extension))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn dependabot_update(ecosystem: &str, directory: &str) -> String {
    format!(
        "  - package-ecosystem: {ecosystem}\n    directory: {directory:?}\n    schedule:\n      interval: weekly\n    open-pull-requests-limit: 3\n    groups:\n      {ecosystem}-minor-and-patch:\n        patterns: [\"*\"]\n        update-types: [minor, patch]\n"
    )
}

pub(super) fn write_setup_file(path: &Path, content: &[u8], overwrite: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("setup path has no parent directory"))?;
    fs::create_dir_all(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!("refusing to overwrite existing content: {}", path.display());
            }
            if fs::read(path)? == content {
                return Ok(());
            }
            if !overwrite {
                bail!("refusing to overwrite existing content: {}", path.display());
            }
            let mut temporary = NamedTempFile::new_in(parent)?;
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
            temporary.write_all(content)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .with_context(|| format!("could not replace setup file {}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(content)?;
            file.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn safe_path(root: &Path, name: &str) -> Result<PathBuf> {
    let path = root.join(name);
    let parent = path.parent().ok_or_else(|| anyhow!("invalid local path"))?;
    let resolved_parent = nearest_existing(parent)?.canonicalize()?;
    if !resolved_parent.starts_with(root) {
        bail!("refusing a path outside --directory: {name}");
    }
    Ok(path)
}

fn nearest_existing(path: &Path) -> Result<&Path> {
    let mut current = path;
    while !current.exists() {
        current = current
            .parent()
            .ok_or_else(|| anyhow!("path has no existing parent"))?;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_configuration_overwrites_by_default_and_can_be_protected() -> Result<()> {
        let source = SourceRef::parse(&format!("keys-i/koelu@{}", "b".repeat(40)))?;
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join(".github/koelu.toml");
        fs::create_dir_all(path.parent().expect("generated file parent"))?;
        fs::write(&path, "existing generated content\n")?;
        assert!(
            local_files(temporary.path(), &source, &["test".into()], false).is_err(),
            "--no-overwrite must protect the agreement file"
        );
        let files = local_files(temporary.path(), &source, &["test".into()], true)?;
        let generated = files
            .iter()
            .find(|(candidate, _)| candidate.ends_with(".github/koelu.toml"))
            .map(|(_, content)| content)
            .expect("generated configuration");
        write_setup_file(&path, generated.as_bytes(), true)?;
        assert_eq!(fs::read_to_string(&path)?, *generated);
        assert!(write_setup_file(&path, b"blocked update\n", false).is_err());
        assert_eq!(fs::read_to_string(&path)?, *generated);
        fs::write(&path, vec![b'x'; MAX_CONFIGURATION_BYTES as usize + 1])?;
        assert!(
            local_files(temporary.path(), &source, &["test".into()], true).is_err(),
            "oversized configuration must fail before replacement"
        );
        Ok(())
    }

    #[test]
    fn existing_configuration_is_reused_only_when_it_is_parseable() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let config = temporary.path().join(".github/koelu.toml");
        fs::create_dir_all(config.parent().expect("configuration parent"))?;
        for (content, expected) in [
            ("schema = 1\n[agreement]\nterms = \"2026-09-23\"\n", true),
            ("not toml", false),
        ] {
            fs::write(&config, content)?;
            assert_eq!(
                existing_configuration(temporary.path())?.is_some(),
                expected,
                "{content}"
            );
        }
        assert!(refuse_existing_configuration(temporary.path(), false).is_err());
        assert!(refuse_existing_configuration(temporary.path(), true).is_ok());
        let legacy = temporary.path().join(".github/koelu.json");
        fs::write(&legacy, r#"{"schema":1}"#)?;
        assert!(existing_configuration(temporary.path())?.is_none());
        fs::remove_file(&config)?;
        assert_eq!(
            existing_configuration(temporary.path())?.unwrap()["schema"],
            1
        );
        assert!(refuse_existing_configuration(temporary.path(), false).is_err());
        Ok(())
    }

    #[test]
    fn generated_toml_preserves_consent_and_quoted_check_names() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let source = SourceRef::parse(&format!("keys-i/koelu@{}", "a".repeat(40)))?;
        let agreement = json!({
            "accepted_at_unix": 1790447725,
            "accepted_by": "keys-i",
            "comment": 5848798567_u64,
            "issue": 28,
            "privacy": "2026-09-27-p5",
            "terms": "2026-09-27-t5",
            "autofix": true
        });
        let files = setup_files(
            temporary.path(),
            &source,
            &["test \"quoted\"".into()],
            false,
            Some(&agreement),
            true,
        )?;
        let content = files
            .iter()
            .find(|(path, _)| path.ends_with(".github/koelu.toml"))
            .map(|(_, content)| content)
            .expect("generated TOML");
        let parsed: Value = toml::from_str(content)?;
        assert_eq!(parsed["agreement"], agreement);
        assert_eq!(parsed["checks"], json!(["test \"quoted\""]));
        assert_eq!(parsed["autofix"], true);
        Ok(())
    }

    #[test]
    fn setup_text_validation_is_table_driven() -> Result<()> {
        assert_eq!(
            checks(&["test".into(), "lint".into(), "test".into()])?,
            ["test", "lint"]
        );
        assert!(checks(&["Koelu dependasolve gate".into()]).is_err());
        Ok(())
    }

    #[test]
    fn generated_dependabot_config_covers_root_and_workspace_manifests() -> Result<()> {
        for (manifest, ecosystem, directory) in [
            ("Cargo.toml", "cargo", "/"),
            ("package.json", "npm", "/"),
            ("requirements.txt", "pip", "/"),
            ("Gemfile", "bundler", "/"),
            ("go.mod", "gomod", "/"),
            ("pom.xml", "maven", "/"),
            ("build.gradle.kts", "gradle", "/"),
            ("composer.json", "composer", "/"),
            ("project.csproj", "nuget", "/"),
            ("Dockerfile", "docker", "/"),
            ("web/package.json", "npm", "/web"),
        ] {
            let temporary = tempfile::tempdir()?;
            let path = temporary.path().join(manifest);
            fs::create_dir_all(path.parent().expect("manifest parent"))?;
            fs::write(path, "")?;
            let config = dependabot_config(temporary.path())?;
            assert!(
                config.contains(&format!(
                    "package-ecosystem: {ecosystem}\n    directory: {directory:?}"
                )),
                "{manifest}"
            );
            assert!(config.contains(&format!("{ecosystem}-minor-and-patch:")));
            assert!(config.contains("package-ecosystem: github-actions\n    directory: \"/\""));
        }
        for (manifest, ecosystem) in [("setup.py", "pip"), ("solution.sln", "nuget")] {
            let temporary = tempfile::tempdir()?;
            fs::write(temporary.path().join(manifest), "")?;
            assert!(
                !dependabot_config(temporary.path())?
                    .contains(&format!("package-ecosystem: {ecosystem}")),
                "{manifest} must not opt into executable or solution-file updates"
            );
        }
        Ok(())
    }
}
