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

    fn matches(&self, binary: &SelectedBinary) -> bool {
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectionInput {
    pub path: Utf8PathBuf,
    pub sha256: String,
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
    pub fn read(path: &Utf8Path) -> anyhow::Result<Self> {
        let raw =
            std::fs::read(path).with_context(|| format!("Failed to read selection {path}"))?;
        let mut selection: Self =
            serde_json::from_slice(&raw).with_context(|| format!("Invalid selection {path}"))?;
        if selection.format != "cargo-test-r-selection" || selection.version != 1 {
            bail!("Unsupported selection format/version in {path}");
        }
        selection.binaries = merge(selection.binaries)?;
        selection.validate()?;
        Ok(selection)
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

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Read metadata directly, or relocate an extracted archive into the current workspace.
pub fn select(
    paths: &[Utf8PathBuf],
    archive: Option<(&Utf8Path, &Utf8Path)>,
    filters: &Filters,
) -> anyhow::Result<Selection> {
    let mut inputs = Vec::new();
    let mut binaries = Vec::new();
    for path in paths {
        let raw = std::fs::read(path)
            .with_context(|| format!("Failed to read binaries metadata {path}"))?;
        let metadata: BinaryListSummary = serde_json::from_slice(&raw)
            .with_context(|| format!("Invalid binaries metadata {path}"))?;
        inputs.push(SelectionInput {
            path: path.clone(),
            sha256: sha256(&raw),
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
    let selection = Selection {
        format: "cargo-test-r-selection".into(),
        version: 1,
        inputs,
        binaries: merge(binaries)?,
    };
    selection.validate()?;
    Ok(selection)
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
