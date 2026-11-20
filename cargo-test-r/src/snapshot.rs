use crate::selection::Selection;
use anyhow::{Context, bail};
use camino::Utf8Path;
#[cfg(target_os = "linux")]
use camino::Utf8PathBuf;
use sha2::{Digest, Sha256};
use std::fs::{self, File};

fn hash(path: &Utf8Path) -> anyhow::Result<String> {
    let mut file = File::open(path).with_context(|| format!("Cannot open {path}"))?;
    let mut digest = Sha256::new();
    use std::io::Read;
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(metadata.permissions().readonly())
    }
}

pub fn has_hashes(selection: &Selection) -> bool {
    selection
        .binaries
        .iter()
        .any(|b| b.sha256.is_some() || b.mode.is_some() || b.dependencies.is_some())
}

/// Verify all entries before the caller starts any executable. Never invoke a loader.
pub fn verify(
    path: &Utf8Path,
    selection: &Selection,
    expect_digest: Option<&str>,
    skip_library_hashes: bool,
) -> anyhow::Result<()> {
    if let Some(expected) = expect_digest
        && expected != format!("sha256:{}", hash(path)?)
    {
        bail!("Selection digest mismatch for {path}");
    }
    if !has_hashes(selection) {
        if expect_digest.is_some() {
            return Ok(());
        }
        bail!("Selection has no snapshot hashes");
    }
    let bin = path.parent().unwrap_or(Utf8Path::new(".")).join("bin");
    if bin.symlink_metadata()?.file_type().is_symlink() {
        bail!("Snapshot bin/ must not be a symlink");
    }
    let bin = bin
        .canonicalize_utf8()
        .context("Missing snapshot bin directory")?;
    for binary in &selection.binaries {
        let executable = binary.executable.canonicalize_utf8()?;
        // Canonicalize both sides: ../ and symlinks must not escape the snapshot.
        if !executable.starts_with(&bin) {
            bail!(
                "Executable for {} is outside snapshot bin/: {}",
                binary.binary_id,
                executable
            );
        }
        let expected = binary
            .sha256
            .as_ref()
            .context("Incomplete snapshot: missing sha256")?;
        if &hash(&executable)? != expected {
            bail!("Binary sha256 mismatch: {}", binary.binary_id);
        }
        if Some(mode(&fs::metadata(&executable)?)) != binary.mode {
            bail!("Binary mode mismatch: {}", binary.binary_id);
        }
        for dependency in binary
            .dependencies
            .as_ref()
            .context("Incomplete snapshot: missing dependencies")?
        {
            if !dependency.path.is_absolute() || !dependency.path.is_file() {
                bail!("Missing recorded library: {}", dependency.path);
            }
            if !skip_library_hashes && hash(&dependency.path)? != dependency.sha256 {
                bail!("Library sha256 mismatch: {}", dependency.path);
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn snapshot(_path: &Utf8Path, _out: &Utf8Path) -> anyhow::Result<String> {
    bail!("snapshot is not supported on this platform (requires Linux glibc)")
}

#[cfg(target_os = "linux")]
pub fn snapshot(path: &Utf8Path, out: &Utf8Path) -> anyhow::Result<String> {
    linux::snapshot(path, out)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::selection::Dependency;
    use goblin::elf::Elf;
    use nextest_metadata::BinaryListSummary;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::process::Command;

    // Resolve the metadata at snapshot time, not verification time. Its input digest
    // binds the mutable-output roots to the selection produced by list.
    fn mutable_roots(selection: &Selection) -> anyhow::Result<Vec<Utf8PathBuf>> {
        if selection.inputs.is_empty() {
            bail!("snapshot requires original nextest metadata inputs");
        }
        let mut roots = Vec::new();
        for input in &selection.inputs {
            if hash(&input.path)? != input.sha256 {
                bail!("Metadata digest mismatch: {}", input.path);
            }
            let metadata: BinaryListSummary = serde_json::from_slice(&fs::read(&input.path)?)?;
            let target = metadata.rust_build_meta.target_directory;
            if !target.is_absolute() {
                bail!("Metadata target directory must be absolute: {target}");
            }
            // The producer's target may no longer exist for an extracted archive.
            roots.push(if target.exists() {
                target.canonicalize_utf8()?
            } else {
                target.clone()
            });
            for linked in &metadata.rust_build_meta.linked_paths {
                // nextest linked-paths are relative to the target directory.
                let linked = target.join(linked);
                if linked.exists() {
                    roots.push(linked.canonicalize_utf8()?);
                }
            }
            // Extracted archives relocate the target under their metadata directory.
            if let Some(nextest) = input.path.parent()
                && nextest.file_name() == Some("nextest")
                && let Some(target) = nextest.parent()
            {
                roots.push(target.canonicalize_utf8()?);
                for linked in &metadata.rust_build_meta.linked_paths {
                    let linked = target.join(linked);
                    if linked.exists() {
                        roots.push(linked.canonicalize_utf8()?);
                    }
                }
            }
        }
        Ok(roots)
    }

    fn resolution(
        interpreter: &str,
        executable: &Utf8Path,
        working_directory: &Utf8Path,
    ) -> anyhow::Result<BTreeMap<String, Utf8PathBuf>> {
        let output = Command::new(interpreter)
            .current_dir(working_directory)
            .arg("--list")
            .arg(executable)
            .output()
            .with_context(|| format!("Cannot inspect loader dependencies for {executable}"))?;
        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() || stdout.contains("not found") || stderr.contains("not found")
        {
            bail!("Loader dependency inspection failed for {executable}: {stdout}{stderr}");
        }
        let mut libraries = BTreeMap::new();
        for line in stdout.lines() {
            let line = line.trim();
            let (name, rest) = line.split_once(" => ").unwrap_or(("", line));
            // Strip the trailing load address, retaining spaces in file names.
            let resolved = rest.rsplit_once(" (").map_or(rest, |(path, _)| path);
            if resolved.starts_with('/') {
                // Keep the loader's path, not only its canonical destination: a
                // symlink retargeted later must also fail library verification.
                let path = Utf8PathBuf::from(resolved);
                libraries.insert(
                    if name.is_empty() {
                        resolved.to_owned()
                    } else {
                        name.to_owned()
                    },
                    path,
                );
            } else if !line.starts_with("linux-vdso.") && !line.starts_with("linux-gate.") {
                bail!("Unrecognized loader dependency for {executable}: {line}");
            }
        }
        // The interpreter is part of the runtime contract too.
        libraries.insert(interpreter.to_owned(), Utf8PathBuf::from(interpreter));
        Ok(libraries)
    }

    fn dependencies(
        source: &Utf8Path,
        copy: &Utf8Path,
        roots: &[Utf8PathBuf],
        working_directory: &Utf8Path,
    ) -> anyhow::Result<Vec<Dependency>> {
        let bytes = fs::read(copy)?;
        let elf = Elf::parse(&bytes).context("snapshot requires an ELF executable")?;
        let Some(interpreter) = elf.interpreter else {
            if !elf.libraries.is_empty() {
                bail!("ELF has DT_NEEDED but no interpreter: {source}");
            }
            return Ok(Vec::new());
        };
        if interpreter.contains("musl") || cfg!(target_env = "musl") {
            bail!("snapshot is not supported on musl");
        }
        if !Utf8Path::new(interpreter).is_absolute() {
            bail!("snapshot requires an absolute ELF interpreter: {interpreter}");
        }
        // goblin parses DT_NEEDED, DT_RPATH and DT_RUNPATH. The loader applies
        // their precedence and expansion; compare its actual resolution below.
        let original = resolution(interpreter, source, working_directory)?;
        let copied = resolution(interpreter, copy, working_directory)?;
        if original != copied {
            bail!(
                "Library resolution changed after copying {source} (RPATH/RUNPATH, including $ORIGIN)"
            );
        }
        for needed in &elf.libraries {
            if !original.contains_key(*needed) {
                bail!("Loader did not resolve DT_NEEDED {needed} for {source}");
            }
        }
        let mut dependencies = BTreeMap::new();
        for library in original.into_values() {
            let canonical = library.canonicalize_utf8()?;
            if roots
                .iter()
                .any(|root| library.starts_with(root) || canonical.starts_with(root))
            {
                bail!("Library resolves inside mutable target/linked-paths: {library}");
            }
            dependencies.insert(
                library.clone(),
                Dependency {
                    sha256: hash(&library)?,
                    path: library,
                },
            );
        }
        Ok(dependencies.into_values().collect())
    }

    pub fn snapshot(path: &Utf8Path, out: &Utf8Path) -> anyhow::Result<String> {
        if cfg!(target_env = "musl") {
            bail!("snapshot is not supported on musl");
        }
        let mut selection = Selection::read(path)?;
        let out = Utf8PathBuf::from_path_buf(std::path::absolute(out)?)
            .map_err(|p| anyhow::anyhow!("Non-UTF-8 output: {p:?}"))?;
        if out.symlink_metadata().is_ok() {
            bail!("Snapshot output already exists: {out}");
        }
        let parent = out
            .parent()
            .context("Snapshot output needs a parent directory")?;
        let parent = parent.canonicalize_utf8()?;
        let out = parent.join(out.file_name().context("Snapshot output needs a name")?);
        let roots = mutable_roots(&selection)?;
        let temp = tempfile::Builder::new()
            .prefix(&format!(
                ".{}.tmp-{}-",
                out.file_name().unwrap(),
                std::process::id()
            ))
            .tempdir_in(&parent)?;
        let staging = Utf8Path::from_path(temp.path()).context("Non-UTF-8 staging directory")?;
        fs::create_dir(staging.join("bin"))?;
        for binary in &mut selection.binaries {
            let id_hash: String = Sha256::digest(binary.binary_id.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let name: String = binary
                .binary_id
                .chars()
                .take(100)
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let name = format!("{name}-{id_hash}");
            let copy = staging.join("bin").join(&name);
            let source = &binary.executable;
            let before = hash(source)?;
            let permissions = fs::metadata(source)?.permissions();
            // create_new + io::copy guarantees a new independent regular file.
            let mut destination = File::create_new(&copy)?;
            std::io::copy(&mut File::open(source)?, &mut destination)?;
            destination.set_permissions(permissions)?;
            destination.sync_all()?;
            let copied_mode = mode(&destination.metadata()?);
            drop(destination);
            if hash(&copy)? != before {
                bail!("source changed while copying: {source}");
            }
            binary.dependencies = Some(dependencies(
                source,
                &copy,
                &roots,
                &binary.working_directory,
            )?);
            if hash(source)? != before {
                bail!("source changed while copying: {source}");
            }
            binary.mode = Some(copied_mode);
            binary.sha256 = Some(before);
            binary.executable = out.join("bin").join(name);
        }
        let manifest = staging.join("selection.json");
        let mut file = File::create_new(&manifest)?;
        file.write_all(&serde_json::to_vec_pretty(&selection)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        let digest = format!("sha256:{}", hash(&manifest)?);
        File::open(staging.join("bin"))?.sync_all()?;
        File::open(staging)?.sync_all()?;
        let parent_directory = File::open(&parent)?;
        parent_directory.sync_all()?;
        // Unlike rename(), NOREPLACE never overwrites a concurrently created out.
        use std::ffi::CString;
        let from = CString::new(staging.as_str())?;
        let to = CString::new(out.as_str())?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINVAL) {
                return Err(error).context("Cannot publish snapshot");
            }
            // Some filesystems do not implement NOREPLACE. This fallback keeps
            // publication atomic, but cannot exclude a concurrent creator.
            match out.symlink_metadata() {
                Ok(_) => bail!("Snapshot output already exists: {out}"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("Cannot check snapshot output"),
            }
            fs::rename(staging, &out).context("Cannot publish snapshot")?;
        }
        // The staging path no longer belongs to us after either rename succeeds.
        let _ = temp.keep();
        if let Err(error) = parent_directory.sync_all() {
            fs::remove_dir_all(&out)
                .context("Cannot roll back snapshot after parent fsync failure")?;
            return Err(error).context("Cannot fsync published snapshot directory entry");
        }
        Ok(digest)
    }
}
