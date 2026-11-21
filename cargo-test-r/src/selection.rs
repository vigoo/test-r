use anyhow::{Context, anyhow, bail};
use camino::{Utf8Path, Utf8PathBuf};
use glob_match::glob_match;
use nextest_metadata::BinaryListSummary;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use tar::Archive;

#[derive(Debug, Default)]
pub struct Filters {
    pub package: Vec<String>,
    pub exclude: Vec<String>,
    pub lib: bool,
    pub bins: bool,
    pub tests: bool,
    pub test: Vec<String>,
    pub binary_id: Vec<String>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.package.is_empty()
            && self.exclude.is_empty()
            && !self.lib
            && !self.bins
            && !self.tests
            && self.test.is_empty()
            && self.binary_id.is_empty()
    }

    pub fn matches(&self, binary: &SelectedBinary) -> bool {
        let targets = self.lib || self.bins || self.tests || !self.test.is_empty();
        (self.package.is_empty()
            || self
                .package
                .iter()
                .any(|p| glob_match(p, &binary.package_name)))
            && !self
                .exclude
                .iter()
                .any(|p| glob_match(p, &binary.package_name))
            && (self.binary_id.is_empty() || self.binary_id.contains(&binary.binary_id))
            && (!targets
                || (self.lib && matches!(binary.kind.as_str(), "lib" | "proc-macro"))
                || (self.bins && binary.kind == "bin")
                || (self.tests
                    && matches!(binary.kind.as_str(), "lib" | "proc-macro" | "bin" | "test"))
                || (binary.kind == "test" && self.test.iter().any(|p| glob_match(p, &binary.name))))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Selection {
    pub format: String,
    pub version: u32,
    pub inputs: Vec<SelectionInput>,
    pub binaries: Vec<SelectedBinary>,
    /// Present only in snapshots. Its presence, not the per-binary fields,
    /// decides whether a selection must be verified before running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
}

/// Unknown keys are rejected: an older verifier must not silently skip a
/// newer integrity check.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SnapshotInfo {
    /// Which loader contract was applied, such as `linux-gnu`.
    pub platform: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectionInput {
    pub path: Utf8PathBuf,
    pub sha256: String,
    /// Selections written before `kind` existed meant nextest metadata.
    #[serde(default = "InputKind::nextest")]
    pub kind: InputKind,
}

/// Only `snapshot` reads inputs. `run` must accept kinds added by newer
/// versions, and rewriting a selection must keep their names.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InputKind {
    NextestBinaries,
    CargoMessages,
    CargoMetadata,
    #[serde(untagged)]
    Other(String),
}

impl InputKind {
    fn nextest() -> Self {
        Self::NextestBinaries
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SelectedBinary {
    pub binary_id: String,
    pub package_id: String,
    pub package_name: String,
    pub kind: String,
    pub name: String,
    pub build_variant: String,
    pub executable: Utf8PathBuf,
    pub working_directory: Utf8PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<Dependency>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub path: Utf8PathBuf,
    pub sha256: String,
}

impl Selection {
    /// Returns the selection and the SHA-256 of exactly the bytes it was parsed from.
    pub fn read(path: &Utf8Path) -> anyhow::Result<(Self, String)> {
        let raw =
            std::fs::read(path).with_context(|| format!("Failed to read selection {path}"))?;
        let mut selection: Self =
            serde_json::from_slice(&raw).with_context(|| format!("Invalid selection {path}"))?;
        if selection.format != "cargo-test-r-selection" || selection.version != 1 {
            bail!("Unsupported selection format/version in {path}");
        }
        selection.binaries = merge(selection.binaries)?;
        selection.validate()?;
        Ok((selection, sha256(&raw)))
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.binaries.is_empty() {
            bail!("No test binaries match the selection");
        }
        for binary in &self.binaries {
            if !binary.executable.is_absolute() || !binary.working_directory.is_absolute() {
                bail!(
                    "Paths for test binary {} must be absolute",
                    binary.binary_id
                );
            }
            let metadata = std::fs::metadata(&binary.executable).with_context(|| {
                format!(
                    "Test binary {} does not exist at {}",
                    binary.binary_id, binary.executable
                )
            })?;
            if !metadata.is_file() {
                bail!(
                    "Test binary {} is not a file at {}",
                    binary.binary_id,
                    binary.executable
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    bail!(
                        "Test binary {} is not executable at {}",
                        binary.binary_id,
                        binary.executable
                    );
                }
            }
            #[cfg(windows)]
            if !binary
                .executable
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            {
                bail!(
                    "Test binary {} is not executable (.exe extension required) at {}",
                    binary.binary_id,
                    binary.executable
                );
            }
            if !binary.working_directory.is_dir() {
                bail!(
                    "Working directory for test binary {} does not exist at {}",
                    binary.binary_id,
                    binary.working_directory
                );
            }
        }
        Ok(())
    }
}

fn merge(binaries: Vec<SelectedBinary>) -> anyhow::Result<Vec<SelectedBinary>> {
    let mut merged = BTreeMap::new();
    for binary in binaries {
        if let Some(previous) = merged.get(&binary.binary_id) {
            if previous != &binary {
                bail!(
                    "Conflicting entries for binary id {}: {:?} and {:?}",
                    binary.binary_id,
                    previous,
                    binary
                );
            }
        } else {
            merged.insert(binary.binary_id.clone(), binary);
        }
    }
    Ok(merged.into_values().collect())
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Read metadata directly, or relocate an extracted archive into the current workspace.
/// Supplied Cargo metadata validates each selected raw artifact before duplicate merging.
pub fn select(
    paths: &[Utf8PathBuf],
    cargo_messages: &[Utf8PathBuf],
    archive: Option<(&Utf8Path, &Utf8Path)>,
    filters: &Filters,
    cargo_metadata: Option<&Utf8Path>,
) -> anyhow::Result<Selection> {
    let mut inputs = Vec::new();
    let mut binaries = Vec::new();
    let authority = if let Some(path) = cargo_metadata {
        if cargo_messages.is_empty() || !paths.is_empty() || archive.is_some() {
            bail!("Cargo metadata requires exclusively Cargo-message inputs");
        }
        let raw =
            std::fs::read(path).with_context(|| format!("Failed to read Cargo metadata {path}"))?;
        let authority: CargoMetadata = serde_json::from_slice(&raw)
            .with_context(|| format!("Invalid Cargo metadata {path}"))?;
        if authority.version != 1 {
            bail!("Unsupported Cargo metadata version in {path}");
        }
        canonical(&authority.workspace_root)
            .with_context(|| format!("Invalid workspace_root in Cargo metadata {path}"))?;
        inputs.push(SelectionInput {
            path: path.to_owned(),
            sha256: sha256(&raw),
            kind: InputKind::CargoMetadata,
        });
        Some((authority, path))
    } else {
        None
    };
    for path in paths {
        let raw = std::fs::read(path)
            .with_context(|| format!("Failed to read binaries metadata {path}"))?;
        let metadata: BinaryListSummary = serde_json::from_slice(&raw)
            .with_context(|| format!("Invalid binaries metadata {path}"))?;
        inputs.push(SelectionInput {
            path: path.clone(),
            sha256: sha256(&raw),
            kind: InputKind::NextestBinaries,
        });
        let original_workspace = if let Some((root, _)) = archive {
            let cargo_path = root.join("target/nextest/cargo-metadata.json");
            let cargo: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&cargo_path).with_context(|| {
                    format!("Failed to read archive Cargo metadata {cargo_path}")
                })?)
                .with_context(|| format!("Invalid archive Cargo metadata {cargo_path}"))?;
            Some(Utf8PathBuf::from(
                cargo["workspace_root"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing workspace_root in {cargo_path}"))?,
            ))
        } else {
            None
        };
        for (id, binary) in &metadata.rust_binaries {
            if id != &binary.binary_id {
                bail!(
                    "Binary id key {id} does not match {} in {path}",
                    binary.binary_id
                );
            }
            let (package_name, package_path) =
                package_identity(&binary.package_id).with_context(|| {
                    format!(
                        "Unsupported package id {} for test binary {}",
                        binary.package_id, id
                    )
                })?;
            // Include both new and legacy platform metadata: nextest has emitted both.
            let variant = serde_json::to_vec(&(
                &metadata.rust_build_meta.target_directory,
                &metadata.rust_build_meta.platforms,
                &metadata.rust_build_meta.target_platforms,
                &metadata.rust_build_meta.target_platform,
                &metadata.rust_build_meta.base_output_directories,
                binary.build_platform,
            ))?;
            let mut selected = SelectedBinary {
                binary_id: id.to_string(),
                package_id: binary.package_id.clone(),
                package_name,
                kind: binary.kind.to_string(),
                name: binary.binary_name.clone(),
                build_variant: sha256(&variant)[..12].to_owned(),
                executable: binary.binary_path.clone(),
                working_directory: package_path,
                sha256: None,
                mode: None,
                dependencies: None,
            };
            if !filters.matches(&selected) {
                continue;
            }
            if let Some((root, workspace)) = archive {
                let root = Utf8PathBuf::from_path_buf(std::path::absolute(root)?)
                    .map_err(|path| anyhow!("Non-UTF-8 archive directory: {path:?}"))?;
                selected.executable = root.join("target").join(
                    binary
                        .binary_path
                        .strip_prefix(&metadata.rust_build_meta.target_directory)
                        .with_context(|| {
                            format!("Binary {} is outside its target directory", id)
                        })?,
                );
                selected.working_directory = workspace.join(
                    selected
                        .working_directory
                        .strip_prefix(original_workspace.as_ref().unwrap())
                        .with_context(|| {
                            format!(
                                "Package {} is outside the archive workspace",
                                binary.package_id
                            )
                        })?,
                );
            }
            binaries.push(selected);
        }
    }
    for path in cargo_messages {
        let raw =
            std::fs::read(path).with_context(|| format!("Failed to read Cargo messages {path}"))?;
        binaries.extend(read_cargo_messages(
            &raw,
            path,
            filters,
            authority.as_ref(),
        )?);
        inputs.push(SelectionInput {
            path: path.clone(),
            sha256: sha256(&raw),
            kind: InputKind::CargoMessages,
        });
    }
    let selection = Selection {
        format: "cargo-test-r-selection".into(),
        version: 1,
        inputs,
        binaries: merge(binaries)?,
        snapshot: None,
    };
    selection.validate()?;
    Ok(selection)
}

#[derive(Deserialize)]
struct CargoArtifact {
    package_id: String,
    manifest_path: Utf8PathBuf,
    target: CargoTarget,
    profile: serde_json::Value,
    features: Vec<String>,
    executable: Option<Utf8PathBuf>,
    // Standard Cargo streams currently omit this; retain it if a producer supplies it.
    target_triple: Option<String>,
}

#[derive(Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
    name: String,
    // Source identity is interpreted only when metadata validation is requested.
    #[serde(default)]
    src_path: serde_json::Value,
}

#[derive(Deserialize)]
struct CargoMetadata {
    version: u32,
    workspace_root: Utf8PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    manifest_path: Utf8PathBuf,
    targets: Vec<CargoTarget>,
}

fn canonical(path: &Utf8Path) -> anyhow::Result<Utf8PathBuf> {
    if !path.is_absolute() {
        bail!("Expected absolute identity path, got {path}");
    }
    path.canonicalize_utf8()
        .with_context(|| format!("Cannot resolve identity path {path}"))
}

impl CargoMetadata {
    fn validate_artifact(&self, artifact: &CargoArtifact, name: &str) -> anyhow::Result<()> {
        let packages: Vec<_> = self
            .packages
            .iter()
            .filter(|p| p.id == artifact.package_id)
            .collect();
        let [package] = packages.as_slice() else {
            bail!(
                "Expected exactly one Cargo metadata package for {}",
                artifact.package_id
            );
        };
        if !self.workspace_members.contains(&package.id) {
            bail!(
                "Package {} is not a member of the Cargo metadata workspace",
                package.id
            );
        }
        if package.name != name {
            bail!(
                "Package name {name} does not match Cargo metadata name {}",
                package.name
            );
        }
        let manifest = canonical(&package.manifest_path)?;
        let directory = canonical(
            package
                .manifest_path
                .parent()
                .context("Manifest has no directory")?,
        )?;
        let (_, source) = package_identity(&artifact.package_id)?;
        if canonical(&source)? != directory {
            bail!(
                "Package source directory does not match Cargo metadata manifest for {}",
                package.id
            );
        }
        if canonical(&artifact.manifest_path)? != manifest {
            bail!(
                "Artifact manifest does not match Cargo metadata for {}",
                package.id
            );
        }
        if canonical(
            artifact
                .manifest_path
                .parent()
                .context("Artifact manifest has no directory")?,
        )? != directory
        {
            bail!(
                "Artifact manifest directory does not match Cargo metadata for {}",
                package.id
            );
        }
        let source = artifact
            .target
            .src_path
            .as_str()
            .map(Utf8Path::new)
            .context("Missing artifact target src_path")?;
        let source = canonical(source)?;
        for target in &package.targets {
            if target.name == artifact.target.name && target.kind == artifact.target.kind {
                let expected = target
                    .src_path
                    .as_str()
                    .map(Utf8Path::new)
                    .context("Missing Cargo metadata target src_path")?;
                if canonical(expected)? == source {
                    return Ok(());
                }
            }
        }
        bail!(
            "Artifact target name/kind/src_path does not match Cargo metadata for {}: {} {:?} at {}",
            package.id,
            artifact.target.name,
            artifact.target.kind,
            source
        )
    }
}

fn read_cargo_messages(
    raw: &[u8],
    path: &Utf8Path,
    filters: &Filters,
    authority: Option<&(CargoMetadata, &Utf8Path)>,
) -> anyhow::Result<Vec<SelectedBinary>> {
    let text = std::str::from_utf8(raw).with_context(|| format!("Invalid UTF-8 in {path}"))?;
    let mut binaries = Vec::new();
    for (line, text) in text.lines().enumerate() {
        let context = || format!("Invalid Cargo message at {path}:{}", line + 1);
        let message: serde_json::Value = serde_json::from_str(text).with_context(context)?;
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let mut artifact: CargoArtifact = serde_json::from_value(message).with_context(context)?;
        let is_test = artifact.profile["test"].as_bool().with_context(context)?;
        if !is_test {
            continue;
        }
        let Some(executable) = artifact.executable.as_ref() else {
            continue;
        };
        let package_name = package_name(&artifact.package_id).with_context(context)?;
        let working_directory = artifact
            .manifest_path
            .parent()
            .filter(|p| p.is_absolute())
            .with_context(|| format!("Missing absolute manifest directory at {path}:{}", line + 1))?
            .to_owned();
        let kind = match artifact.target.kind.as_slice() {
            [kind]
                if matches!(
                    kind.as_str(),
                    "bin" | "test" | "bench" | "example" | "proc-macro"
                ) =>
            {
                kind.as_str()
            }
            kinds
                if !kinds.is_empty()
                    && kinds.iter().all(|k| {
                        matches!(
                            k.as_str(),
                            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"
                        )
                    }) =>
            {
                "lib"
            }
            kinds => bail!(
                "Unsupported Cargo target kind {kinds:?} at {path}:{}",
                line + 1
            ),
        };
        let binary_id = match kind {
            "lib" | "proc-macro" => package_name.clone(),
            "test" => format!("{package_name}::{}", artifact.target.name),
            _ => format!("{package_name}::{kind}/{}", artifact.target.name),
        };
        artifact.features.sort();
        artifact.features.dedup();
        let variant = serde_json::to_vec(&(
            &artifact.profile,
            &artifact.features,
            &artifact.target_triple,
        ))?;
        let binary = SelectedBinary {
            binary_id,
            package_id: artifact.package_id.clone(),
            package_name,
            kind: kind.to_owned(),
            name: artifact.target.name.clone(),
            build_variant: sha256(&variant)[..12].to_owned(),
            executable: executable.clone(),
            working_directory,
            sha256: None,
            mode: None,
            dependencies: None,
        };
        if filters.matches(&binary) {
            if let Some((metadata, metadata_path)) = authority {
                metadata.validate_artifact(&artifact, &binary.package_name).with_context(|| {
                    format!("Invalid artifact identity at {path}:{} against Cargo metadata {metadata_path}", line + 1)
                })?;
            }
            binaries.push(binary);
        }
    }
    Ok(binaries)
}

fn package_name(id: &str) -> anyhow::Result<String> {
    if let Some((_, fragment)) = id.rsplit_once('#')
        && let Some((name, version)) = fragment.split_once('@')
        && !name.is_empty()
        && !version.is_empty()
    {
        return Ok(name.to_owned());
    }
    Ok(package_identity(id)?.0)
}

fn package_identity(id: &str) -> anyhow::Result<(String, Utf8PathBuf)> {
    let (source, fragment) = id
        .rsplit_once('#')
        .ok_or_else(|| anyhow!("Missing package version"))?;
    let url = url::Url::parse(
        source
            .strip_prefix("path+")
            .ok_or_else(|| anyhow!("Not a path package"))?,
    )?;
    let path = Utf8PathBuf::from_path_buf(
        url.to_file_path()
            .map_err(|_| anyhow!("Not a local file URL"))?,
    )
    .map_err(|_| anyhow!("Non-UTF-8 package directory"))?;
    let name = if let Some((name, _)) = fragment.split_once('@') {
        name
    } else {
        path.file_name().ok_or_else(|| anyhow!("No package name"))?
    };
    if name.is_empty() || fragment.is_empty() {
        bail!("Empty package name or version");
    }
    Ok((name.to_owned(), path))
}

pub fn archive_target(workspace_root: &Utf8Path, target_dir: &Option<String>) -> Utf8PathBuf {
    target_dir
        .as_ref()
        .map_or_else(|| workspace_root.join("target"), Utf8PathBuf::from)
        .join("nextest-archive")
}

pub fn extract_tar_zstd(archive_file: &Utf8Path, target_root: &Utf8Path) -> anyhow::Result<()> {
    let decoder = zstd::Decoder::new(File::open(archive_file)?)?;
    Archive::new(decoder).unpack(target_root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(json: &str) -> SelectionInput {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn input_kinds_default_to_nextest_and_keep_unknown_names() {
        assert_eq!(
            input(r#"{"path": "a", "sha256": "0"}"#).kind,
            InputKind::NextestBinaries
        );
        assert_eq!(
            input(r#"{"path": "a", "sha256": "0", "kind": "cargo-messages"}"#).kind,
            InputKind::CargoMessages
        );
        let future = input(r#"{"path": "a", "sha256": "0", "kind": "future-thing"}"#);
        assert_eq!(future.kind, InputKind::Other("future-thing".into()));
        assert_eq!(
            serde_json::to_value(&future).unwrap()["kind"],
            "future-thing"
        );
    }
}
