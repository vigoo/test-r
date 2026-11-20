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

/// Loader contracts that `verify` knows how to check.
const PLATFORMS: &[&str] = &["linux-gnu"];

/// A snapshot marker, or any per-binary snapshot field, means the selection
/// must pass `verify` before anything runs.
pub fn requires_verification(selection: &Selection) -> bool {
    selection.snapshot.is_some()
        || selection
            .binaries
            .iter()
            .any(|b| b.sha256.is_some() || b.mode.is_some() || b.dependencies.is_some())
}

/// Verify all entries before the caller starts any executable. Never invoke a loader.
/// `digest` is the SHA-256 of the bytes `selection` was parsed from.
pub fn verify(
    path: &Utf8Path,
    selection: &Selection,
    digest: &str,
    expect_digest: Option<&str>,
    skip_library_hashes: bool,
) -> anyhow::Result<()> {
    if let Some(expected) = expect_digest
        && expected != format!("sha256:{digest}")
    {
        bail!("Selection digest mismatch for {path}");
    }
    let Some(info) = &selection.snapshot else {
        if requires_verification(selection) {
            bail!("Selection has snapshot fields but no \"snapshot\" marker: {path}");
        }
        if skip_library_hashes {
            bail!("--skip-library-hashes applies only to snapshots; {path} is not one");
        }
        if expect_digest.is_some() {
            return Ok(());
        }
        bail!("Selection is not a snapshot: {path}");
    };
    if !PLATFORMS.contains(&info.platform.as_str()) {
        bail!(
            "Unsupported snapshot platform {:?} in {path}; this cargo-test-r verifies {PLATFORMS:?}",
            info.platform
        );
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
    use crate::selection::{Dependency, InputKind, SnapshotInfo};
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
            match input.kind {
                InputKind::NextestBinaries => {}
                InputKind::CargoMessages => bail!(
                    "snapshot does not support Cargo-message inputs: {} lacks complete mutable target/linked-paths metadata; use nextest binaries metadata instead",
                    input.path
                ),
                InputKind::Other(ref kind) => bail!(
                    "snapshot does not support input kind {kind:?} of {} (misspelled, or written by a newer cargo-test-r)",
                    input.path
                ),
            }
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
        if !output.status.success() {
            bail!("Loader dependency inspection failed for {executable}: {stdout}{stderr}");
        }
        let mut libraries = parse_loader_list(&stdout)
            .with_context(|| format!("Loader dependency inspection failed for {executable}"))?;
        // The interpreter is part of the runtime contract too.
        libraries.insert(interpreter.to_owned(), Utf8PathBuf::from(interpreter));
        Ok(libraries)
    }

    /// Parse glibc `ld.so --list` output into loader name (or path, for
    /// entries listed without one) and the absolute path the loader resolved.
    fn parse_loader_list(stdout: &str) -> anyhow::Result<BTreeMap<String, Utf8PathBuf>> {
        let mut libraries = BTreeMap::new();
        for line in stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let (name, rest) = match line.split_once(" => ") {
                Some((name, rest)) => (Some(name), rest),
                None => (None, line),
            };
            if rest == "not found" {
                bail!("Library not found: {}", name.unwrap_or(line));
            }
            // Strip the trailing load address, retaining spaces in file names.
            let resolved = match rest.rsplit_once(" (") {
                Some((path, address)) if is_address(address) => path,
                _ => rest,
            };
            if resolved.starts_with('/') {
                // Keep the loader's path, not only its canonical destination: a
                // symlink retargeted later must also fail library verification.
                libraries.insert(
                    name.unwrap_or(resolved).to_owned(),
                    Utf8PathBuf::from(resolved),
                );
            } else if name.is_none() && is_vdso(line) {
                // The kernel maps the vDSO; it is not a file to record.
            } else {
                bail!("Unrecognized loader dependency: {line}");
            }
        }
        Ok(libraries)
    }

    fn is_address(text: &str) -> bool {
        text.strip_prefix("0x")
            .and_then(|text| text.strip_suffix(')'))
            .is_some_and(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
    }

    /// A vDSO line is a single name without a path, or nothing at all, followed
    /// by its load address: `linux-vdso.so.1 (0x...)`, `linux-vdso64.so.1
    /// (0x...)` or just `(0x...)`. Names differ between architectures.
    fn is_vdso(line: &str) -> bool {
        let Some((name, address)) = line.rsplit_once('(') else {
            return false;
        };
        let name = name.trim_end();
        is_address(address) && !name.contains('/') && !name.contains(char::is_whitespace)
    }

    /// The loader lists itself by path, while `DT_NEEDED` names it by file name.
    fn unresolved<'a>(
        needed: &[&'a str],
        resolved: &BTreeMap<String, Utf8PathBuf>,
    ) -> Option<&'a str> {
        needed.iter().copied().find(|name| {
            !resolved.contains_key(*name)
                && !resolved
                    .values()
                    .any(|path| path.file_name() == Some(*name))
        })
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
        if let Some(needed) = unresolved(&elf.libraries, &original) {
            bail!("Loader did not resolve DT_NEEDED {needed} for {source}");
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
        let (mut selection, _) = Selection::read(path)?;
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
        selection.snapshot = Some(SnapshotInfo {
            platform: "linux-gnu".into(),
        });
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
            // EINVAL: the filesystem lacks NOREPLACE. ENOSYS: an old kernel.
            // EPERM: a seccomp filter (some containers). A real permission
            // problem fails the plain rename below as well.
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINVAL | libc::ENOSYS | libc::EPERM)
            ) {
                return Err(error).context("Cannot publish snapshot");
            }
            // This fallback keeps publication atomic, but cannot exclude a
            // concurrent creator.
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

    #[cfg(test)]
    mod tests {
        use super::*;

        const UBUNTU: &str = "\tlinux-vdso.so.1 (0x00007ffd5a1f0000)
\tlibgcc_s.so.1 => /lib/x86_64-linux-gnu/libgcc_s.so.1 (0x00007f0e1a200000)
\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f0e1a000000)
\t/lib64/ld-linux-x86-64.so.2 (0x00007f0e1a500000)
";

        fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
            parse_loader_list(text)
                .map(|map| {
                    map.into_iter()
                        .map(|(name, path)| (name, path.into_string()))
                        .collect()
                })
                .map_err(|error| error.to_string())
        }

        #[test]
        fn records_named_and_path_only_entries_and_skips_the_vdso() {
            assert_eq!(
                parse(UBUNTU).unwrap(),
                [
                    ("/lib64/ld-linux-x86-64.so.2", "/lib64/ld-linux-x86-64.so.2"),
                    ("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
                    ("libgcc_s.so.1", "/lib/x86_64-linux-gnu/libgcc_s.so.1"),
                ]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
            );
        }

        #[test]
        fn accepts_vdso_names_of_other_architectures_and_unnamed_vdso() {
            for vdso in [
                "linux-vdso64.so.1 (0x00007fffb7d60000)",
                "linux-vdso32.so.1 (0x00100000)",
                "linux-gate.so.1 (0xf7f1b000)",
                "(0x00007fff9b7fe000)",
            ] {
                assert_eq!(parse(&format!("\t{vdso}\n")).unwrap(), [], "{vdso}");
            }
        }

        #[test]
        fn loader_listed_by_path_resolving_elsewhere_is_recorded_under_that_path() {
            // Fedora and Arch print the interpreter with its resolved location.
            let text =
                "\t/lib64/ld-linux-x86-64.so.2 => /usr/lib64/ld-linux-x86-64.so.2 (0x7f00)\n";
            let resolved = parse_loader_list(text).unwrap();
            assert_eq!(
                parse(text).unwrap(),
                [(
                    "/lib64/ld-linux-x86-64.so.2".to_owned(),
                    "/usr/lib64/ld-linux-x86-64.so.2".to_owned()
                )]
            );
            assert_eq!(unresolved(&["ld-linux-x86-64.so.2"], &resolved), None);
        }

        #[test]
        fn keeps_spaces_in_library_paths() {
            assert_eq!(
                parse("\tlibx.so => /opt/my libs/libx.so (0x1f)\n").unwrap(),
                [("libx.so".to_owned(), "/opt/my libs/libx.so".to_owned())]
            );
        }

        #[test]
        fn rejects_missing_relative_and_unknown_entries() {
            for (line, expected) in [
                (
                    "libmissing.so => not found",
                    "Library not found: libmissing.so",
                ),
                ("libx.so => libx.so (0x1)", "Unrecognized loader dependency"),
                ("statically linked", "Unrecognized loader dependency"),
                ("lib x.so (0x1)", "Unrecognized loader dependency"),
            ] {
                let error = parse(&format!("\t{line}\n")).unwrap_err();
                assert!(error.contains(expected), "{line}: {error}");
            }
        }

        #[test]
        fn loader_in_dt_needed_matches_its_path_only_entry() {
            let resolved = parse_loader_list(UBUNTU).unwrap();
            assert_eq!(
                unresolved(&["libc.so.6", "ld-linux-x86-64.so.2"], &resolved),
                None
            );
            assert_eq!(
                unresolved(&["libc.so.6", "libm.so.6"], &resolved),
                Some("libm.so.6")
            );
        }
    }
}
