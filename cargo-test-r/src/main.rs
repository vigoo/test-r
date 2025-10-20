use anyhow::anyhow;
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser};
use glob_match::glob_match;
use humansize::{BINARY, format_size};
use nextest_metadata::{BinaryListSummary, RustTestBinaryKind, RustTestBinarySummary};
use std::fs::File;
use std::process::ExitStatus;
use tar::Archive;

#[derive(Parser, Debug, Clone)]
#[command()]
#[allow(clippy::large_enum_variant)]
enum Command {
    #[command()]
    ReuseNextestArchive(ReuseNextestArchiveSubcommand),

    #[command()]
    Run(RunSubcommand),
}

/// Unpacks a binary archive created by `cargo-nextest` to be used by follow-up `cargo-test-r run` commands.
#[derive(Args, Debug, Clone)]
struct ReuseNextestArchiveSubcommand {
    /// Path to the archive file
    #[clap(long)]
    archive_file: Utf8PathBuf,
    /// Directory for all generated artifacts and intermediate files
    #[clap(long)]
    target_dir: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct RunSubcommand {
    /// Compile, but don't run tests
    #[clap(long)]
    no_run: bool,
    /// Run all tests regardless of failure
    #[clap(long)]
    no_fail_fast: bool,
    /// Test only the specified package
    #[clap(short, long)]
    package: Vec<String>,
    /// Exclude the specified packages
    #[clap(long)]
    exclude: Vec<String>,
    /// Test the package's library
    #[clap(long)]
    lib: bool,
    /// Test only the specified binary
    #[clap(long)]
    bin: Option<String>,
    /// Test all binaries
    #[clap(long)]
    bins: bool,
    /// Test only the specified example
    #[clap(long)]
    example: Option<String>,
    /// Test all examples
    #[clap(long)]
    examples: bool,
    /// Test the specified integration test
    #[clap(long)]
    test: Vec<String>,
    /// Test all targets in test mode that have the test = true manifest flag set
    #[clap(long)]
    tests: bool,
    /// Test the specified benchmark
    #[clap(long)]
    bench: Option<String>,
    /// Test all benchmarks
    #[clap(long)]
    benches: bool,
    /// Test all targets
    #[clap(long)]
    all_targets: bool,
    /// Test ONLY the library's documentation
    #[clap(long)]
    doc: bool,
    /// Space or comma separated list of features to activate
    #[clap(short = 'F', long)]
    features: Option<String>,
    /// Do not activate the default feature of the selected packages
    #[clap(long)]
    no_default_features: bool,
    /// Test for a given architecture
    #[clap(long)]
    target: Option<String>,
    /// Test optimized artifacts with the release profile
    #[clap(short, long)]
    release: bool,
    /// Test with the given profile
    #[clap(long)]
    profile: Option<String>,
    /// Ignore `rust-version` specification in packages
    #[clap(long)]
    ignore_rust_version: bool,
    /// Output information how long each compilation takes
    #[clap(long)]
    timings: Option<String>,
    /// Directory for all generated artifacts and intermediate files
    #[clap(long)]
    target_dir: Option<String>,
    /// Use verbose output. May be specified twice for "very verbose" output
    #[clap(short, long, action = clap::ArgAction::Count)]
    verbose: Option<u8>,
    /// Do not print cargo log messages
    #[clap(short, long)]
    quiet: bool,
    /// Control when colored output is used
    #[clap(long)]
    color: Option<String>,
    /// The output format for diagnostic messages
    #[clap(long)]
    message_format: Option<String>,
    /// Path to the Cargo.toml file
    #[clap(long)]
    manifest_path: Option<String>,
    /// Require Cargo.lock and cache are up to date
    #[clap(long)]
    frozen: bool,
    /// Require Cargo.lock is up to date
    #[clap(long)]
    locked: bool,
    /// Run without accessing the network
    #[clap(long)]
    offline: bool,
    /// Number of parallel jobs to run
    #[clap(short, long)]
    jobs: Option<usize>,
    /// Build as many crates in the dependency graph as possible
    #[clap(long)]
    keep_going: bool,
    /// Displays a future-incompat report for any future-incompatible warnings
    #[clap(long)]
    future_incompat_report: bool,
    /// Test all members in the workspace.
    #[clap(long)]
    workspace: bool,
    /// Alias for --workspace
    #[clap(long)]
    all: bool,

    /// Test name filter
    testname: Option<String>,

    /// Arguments to be passed to the tests
    #[arg(last = true, num_args = 0..)]
    test_args: Vec<String>,
}

fn main() {
    match try_main() {
        Ok(status) => std::process::exit(status.code().unwrap_or(0)),
        Err(err) => {
            eprintln!("Error: {:#}", err);
            std::process::exit(1);
        }
    }
}

fn try_main() -> anyhow::Result<ExitStatus> {
    let command: Command = Parser::parse();
    let workspace_root = workspace_root::get_workspace_root();
    let workspace_root = Utf8Path::from_path(&workspace_root).ok_or_else(|| {
        anyhow!(
            "Unexpected characters in workspace root: {:?}",
            workspace_root
        )
    })?;

    match command {
        Command::ReuseNextestArchive(cmd) => reuse_nextest_archive(workspace_root, cmd),
        Command::Run(cmd) => run(workspace_root, cmd),
    }
}

fn reuse_nextest_archive(
    workspace_root: &Utf8Path,
    cmd: ReuseNextestArchiveSubcommand,
) -> anyhow::Result<ExitStatus> {
    let target = nextest_archive_target(workspace_root, &cmd.target_dir);

    if !target.exists() {
        println!("Creating target directory: {target}");
        std::fs::create_dir_all(&target)?;
    }

    let archive_size = std::fs::metadata(&cmd.archive_file)?.len();
    println!(
        "Extracting {} to {} ({})",
        cmd.archive_file,
        target,
        format_size(archive_size, BINARY)
    );
    extract_tar_zstd(&cmd.archive_file, &target)?;

    Ok(ExitStatus::default())
}

fn run(workspace_root: &Utf8Path, cmd: RunSubcommand) -> anyhow::Result<ExitStatus> {
    let nextest_archive_target = nextest_archive_target(workspace_root, &cmd.target_dir);
    let binaries_metadata_path =
        nextest_archive_target.join("target/nextest/binaries-metadata.json");
    if binaries_metadata_path.exists() {
        let raw = std::fs::read_to_string(binaries_metadata_path)?;
        let binary_list: BinaryListSummary = serde_json::from_str(&raw)?;

        println!("Running tests using cargo-nextest archive");

        validate_supported_test_args(&cmd)?;

        let test_binaries = filter_binaries(binary_list, workspace_root, &cmd)?;
        println!("{test_binaries:#?}"); // TODO: remove

        let mut test_args = vec![];
        if let Some(test_name) = &cmd.testname {
            test_args.push(test_name.clone());
        }
        test_args.extend(cmd.test_args);

        println!("{test_args:?}"); // TODO: remove

        for test_binary in test_binaries {
            let mut test_process = std::process::Command::new(&test_binary.binary_path);
            test_process.current_dir(&test_binary.working_directory);
            test_process.args(&test_args);

            let exit_code = test_process.status()?;
            if !exit_code.success() {
                return Ok(exit_code);
            }
        }

        Ok(ExitStatus::default())
    } else {
        println!("No nexttest archive was found, forwarding to cargo test");
        let cargo_path: Utf8PathBuf = std::env::var("CARGO").map_or("cargo".into(), Utf8PathBuf::from);
        let mut command = std::process::Command::new(cargo_path.clone());
        let mut args = std::env::args().collect::<Vec<_>>();
        args.remove(0); // process name
        args.remove(0); // 'run' command
        args.insert(0, "test".into());
        command.args(&args);

        println!("Executing {} {}", cargo_path, args.join(" "));

        Ok(command.status()?)
    }
}

fn validate_supported_test_args(cmd: &RunSubcommand) -> anyhow::Result<()> {
    // NOTE: this feature is experimental and only a small subset of the features are
    // supported. We fail if the user is trying to use something unsupported rather than
    // doing something unexpected.

    // Supported: -p
    // Supported: --test
    // Supported: --lib
    // Supported: --workspace --exclude X
    // Supported: testname, testargs

    if cmd.no_run {
        return Err(anyhow!("--no-run is not supported"));
    }
    if cmd.no_fail_fast {
        return Err(anyhow!("--no-fail-fast is not supported"));
    }
    if cmd.bin.is_some() {
        return Err(anyhow!("--bin is not supported"));
    }
    if cmd.bins {
        return Err(anyhow!("--bins is not supported"));
    }
    if cmd.example.is_some() {
        return Err(anyhow!("--example is not supported"));
    }
    if cmd.examples {
        return Err(anyhow!("--examples is not supported"));
    }
    if cmd.tests {
        return Err(anyhow!("--tests is not supported"));
    }
    if cmd.bench.is_some() {
        return Err(anyhow!("--bench is not supported"));
    }
    if cmd.benches {
        return Err(anyhow!("--benches is not supported"));
    }
    if cmd.all_targets {
        return Err(anyhow!("--all-targets is not supported"));
    }
    if cmd.doc {
        return Err(anyhow!("--doc is not supported"));
    }
    if cmd.features.is_some() {
        return Err(anyhow!("--features is not supported"));
    }
    if cmd.no_default_features {
        return Err(anyhow!("--no-default-features is not supported"));
    }
    if cmd.target.is_some() {
        return Err(anyhow!("--target is not supported"));
    }
    if cmd.release {
        return Err(anyhow!("--release is not supported"));
    }
    if cmd.profile.is_some() {
        return Err(anyhow!("--profile is not supported"));
    }
    if cmd.ignore_rust_version {
        return Err(anyhow!("--ignore-rust-version is not supported"));
    }
    if cmd.timings.is_some() {
        return Err(anyhow!("--timings is not supported"));
    }
    if cmd.color.is_some() {
        return Err(anyhow!("--color is not supported"));
    }
    if cmd.message_format.is_some() {
        return Err(anyhow!("--message-format is not supported"));
    }
    if cmd.manifest_path.is_some() {
        return Err(anyhow!("--manifest-path is not supported"));
    }
    if cmd.frozen {
        return Err(anyhow!("--frozen is not supported"));
    }
    if cmd.locked {
        return Err(anyhow!("--locked is not supported"));
    }
    if cmd.keep_going {
        return Err(anyhow!("--keep-going is not supported"));
    }
    if cmd.future_incompat_report {
        return Err(anyhow!("--future-incompat-report is not supported"));
    }

    Ok(())
}

fn package_path(summary: &RustTestBinarySummary) -> Option<Utf8PathBuf> {
    if let Some(path) = summary.package_id.strip_prefix("path+file://") {
        let version_part = path.rfind('#');
        if let Some(version_part) = version_part {
            let path = &path[..version_part];
            Some(Utf8Path::new(path).to_path_buf())
        } else {
            None
        }
    } else {
        None
    }
}

fn filter_by_package(
    mut binaries: BinaryListSummary,
    packages: &[String],
    excludes: &[String],
) -> anyhow::Result<BinaryListSummary> {
    binaries.rust_binaries.retain(|_, summary| {
        if let Some(path) = package_path(summary) {
            if let Some(file_name) = path.file_name() {
                packages
                    .iter()
                    .any(|condition| glob_match(condition, file_name))
                    && excludes
                        .iter()
                        .all(|condition| !glob_match(condition, file_name))
            } else {
                false
            }
        } else {
            false
        }
    });
    Ok(binaries)
}

fn filter_by_test(
    mut binaries: BinaryListSummary,
    test: &[String],
) -> anyhow::Result<BinaryListSummary> {
    binaries.rust_binaries.retain(|_, summary| {
        summary.kind == RustTestBinaryKind::TEST
            && test
                .iter()
                .any(|condition| glob_match(condition, &summary.binary_name))
    });
    Ok(binaries)
}

fn filter_by_kind(
    mut binaries: BinaryListSummary,
    kind: RustTestBinaryKind,
) -> anyhow::Result<BinaryListSummary> {
    binaries
        .rust_binaries
        .retain(|_, summary| summary.kind == kind);
    Ok(binaries)
}

fn filter_binaries(
    mut binaries: BinaryListSummary,
    workspace_root: &Utf8Path,
    cmd: &RunSubcommand,
) -> anyhow::Result<Vec<TestBinary>> {
    let mut filtered_binaries = vec![];

    let archive_root = nextest_archive_target(workspace_root, &cmd.target_dir);

    let original_target = binaries.rust_build_meta.target_directory.clone();
    let original_workspace = original_target.parent().ok_or_else(|| {
        anyhow!("The archive's original target directory {original_target} does not have a parent")
    })?;
    // TODO: this only works if the target directory was not customized when building the archive

    if !cmd.package.is_empty() {
        binaries = filter_by_package(binaries, &cmd.package, &cmd.exclude)?;
    }

    if !cmd.test.is_empty() {
        binaries = filter_by_test(binaries, &cmd.test)?;
    }

    if cmd.lib {
        binaries = filter_by_kind(binaries, RustTestBinaryKind::LIB)?;
    }

    for binary in binaries.rust_binaries.values() {
        let relative_path = binary.binary_path.strip_prefix(original_workspace)?;
        if let Some(relative_package_path) = package_path(binary) {
            filtered_binaries.push(TestBinary {
                binary_path: archive_root.join(relative_path.to_path_buf()),
                working_directory: workspace_root.join(
                    relative_package_path
                        .strip_prefix(original_workspace)?
                        .to_path_buf(),
                ),
            });
        }
    }

    Ok(filtered_binaries)
}

#[derive(Debug, Clone)]
struct TestBinary {
    binary_path: Utf8PathBuf,
    working_directory: Utf8PathBuf,
}

fn extract_tar_zstd(archive_file: &Utf8Path, target_root: &Utf8Path) -> anyhow::Result<()> {
    let decoder = zstd::Decoder::new(File::open(archive_file)?)?;
    let mut archive = Archive::new(decoder);
    archive.unpack(target_root)?;
    Ok(())
}

fn nextest_archive_target(workspace_root: &Utf8Path, target_dir: &Option<String>) -> Utf8PathBuf {
    if let Some(target_dir) = target_dir {
        Utf8Path::new(target_dir).join("nextest-archive")
    } else {
        workspace_root.join("target/nextest-archive")
    }
}
