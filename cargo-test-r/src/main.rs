use anyhow::{anyhow, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser};
use humansize::{BINARY, format_size};
use selection::{Filters, Selection};

mod events;
mod run;
mod selection;
mod snapshot;
mod supervise;

#[derive(Parser, Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Unpack a cargo-nextest archive for later list/run commands.
    ReuseNextestArchive(ReuseNextestArchiveSubcommand),
    /// Select and validate prebuilt binaries, printing selection JSON without running Cargo or tests.
    List(SelectionArgs),
    /// Copy a selection into an independently verified snapshot (Linux glibc).
    Snapshot(SnapshotSubcommand),
    /// Verify a snapshot without invoking the loader or tests.
    Verify(VerifySubcommand),
    Run(RunSubcommand),
}

#[derive(Args, Debug, Clone)]
struct SnapshotSubcommand {
    #[arg(long)]
    selection: Utf8PathBuf,
    #[arg(long)]
    out: Utf8PathBuf,
}

#[derive(Args, Debug, Clone)]
struct VerifySubcommand {
    #[arg(long)]
    selection: Utf8PathBuf,
    #[command(flatten)]
    verification: VerificationArgs,
}

#[derive(Args, Debug, Clone)]
struct VerificationArgs {
    /// Require the exact selection.json digest printed by snapshot.
    #[arg(long)]
    expect_digest: Option<String>,
    /// Check recorded libraries exist, but allow their contents to change.
    #[arg(long)]
    skip_library_hashes: bool,
}

#[derive(Args, Debug, Clone)]
struct ReuseNextestArchiveSubcommand {
    /// Path to the archive file.
    #[arg(long)]
    archive_file: Utf8PathBuf,
    /// Directory for all generated artifacts and intermediate files.
    #[arg(long)]
    target_dir: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct SelectionArgs {
    /// Read existing nextest binaries-only JSON (repeatable).
    #[arg(long, value_name = "PATH")]
    binaries_metadata: Vec<Utf8PathBuf>,
    /// Read Cargo JSON messages from an existing build (repeatable).
    #[arg(long, value_name = "PATH")]
    cargo_messages: Vec<Utf8PathBuf>,
    /// Select Cargo package names (globs supported).
    #[arg(short, long)]
    package: Vec<String>,
    /// Exclude Cargo package names (globs supported).
    #[arg(long)]
    exclude: Vec<String>,
    /// Select library unit tests.
    #[arg(long)]
    lib: bool,
    /// Select binary unit tests.
    #[arg(long)]
    bins: bool,
    /// Select integration test targets by name (globs supported, repeatable).
    #[arg(long)]
    test: Vec<String>,
    /// Select all library, binary and integration test targets.
    #[arg(long)]
    tests: bool,
    /// Select exact nextest binary ids (repeatable).
    #[arg(long)]
    binary_id: Vec<String>,
    /// Directory containing nextest-archive (default: workspace target directory).
    #[arg(long)]
    target_dir: Option<String>,
}

impl SelectionArgs {
    fn filters(&self) -> Filters {
        Filters {
            package: self.package.clone(),
            exclude: self.exclude.clone(),
            lib: self.lib,
            bins: self.bins,
            tests: self.tests,
            test: self.test.clone(),
            binary_id: self.binary_id.clone(),
        }
    }

    fn explicit(&self) -> bool {
        !self.binaries_metadata.is_empty() || !self.cargo_messages.is_empty()
    }

    /// The extracted archive directory and the current workspace root.
    fn archive(&self) -> anyhow::Result<(Utf8PathBuf, Utf8PathBuf)> {
        let workspace = workspace_root()?;
        Ok((
            selection::archive_target(&workspace, &self.target_dir),
            workspace,
        ))
    }

    /// Whether `select` has prebuilt input: explicit metadata or an extracted archive.
    fn has_prebuilt_input(&self) -> anyhow::Result<bool> {
        Ok(self.explicit() || self.archive()?.0.join(ARCHIVE_METADATA).exists())
    }

    fn select(&self) -> anyhow::Result<Selection> {
        if self.explicit() {
            return selection::select(
                &self.binaries_metadata,
                &self.cargo_messages,
                None,
                &self.filters(),
            );
        }
        let (archive, workspace) = self.archive()?;
        selection::select(
            &[archive.join(ARCHIVE_METADATA)],
            &[],
            Some((&archive, &workspace)),
            &self.filters(),
        )
    }
}

const ARCHIVE_METADATA: &str = "target/nextest/binaries-metadata.json";

#[derive(Args, Debug, Clone)]
struct RunSubcommand {
    #[command(flatten)]
    selection_args: SelectionArgs,
    /// Run exactly the binaries in a selection file, without invoking Cargo.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["binaries_metadata", "cargo_messages"])]
    selection: Option<Utf8PathBuf>,
    #[command(flatten)]
    verification: VerificationArgs,
    /// Compile, but don't run tests.
    #[arg(long)]
    no_run: bool,
    /// Run all tests regardless of failure.
    #[arg(long)]
    no_fail_fast: bool,
    /// Whole invocation budget, including compilation on the Cargo fallback (e.g. 30m).
    #[arg(long, value_parser = supervise::parse_duration)]
    deadline: Option<std::time::Duration>,
    /// Time between TERM and KILL on Unix (e.g. 10s).
    #[arg(long, default_value = "10s", value_parser = supervise::parse_duration)]
    kill_grace: std::time::Duration,
    /// Keep daemons after normal prebuilt exits; cancellation/deadline still clean up.
    #[arg(long)]
    keep_leftovers: bool,
    /// Write lifecycle JSON Lines to a new file (prebuilt runs only).
    #[arg(long, value_name = "PATH")]
    events: Option<std::path::PathBuf>,
    /// Opaque invocation identity (defaults to a UUID).
    #[arg(long, value_name = "ID")]
    invocation_id: Option<String>,
    /// Test only the specified binary.
    #[arg(long)]
    bin: Option<String>,
    /// Test only the specified example.
    #[arg(long)]
    example: Option<String>,
    /// Test all examples.
    #[arg(long)]
    examples: bool,
    /// Test the specified benchmark.
    #[arg(long)]
    bench: Option<String>,
    /// Test all benchmarks.
    #[arg(long)]
    benches: bool,
    /// Test all targets.
    #[arg(long)]
    all_targets: bool,
    /// Test ONLY the library's documentation.
    #[arg(long)]
    doc: bool,
    /// Space or comma separated list of features to activate.
    #[arg(short = 'F', long)]
    features: Option<String>,
    /// Do not activate the default feature of the selected packages.
    #[arg(long)]
    no_default_features: bool,
    /// Test for a given architecture.
    #[arg(long)]
    target: Option<String>,
    /// Test optimized artifacts with the release profile.
    #[arg(short, long)]
    release: bool,
    /// Test with the given profile.
    #[arg(long)]
    profile: Option<String>,
    /// Ignore rust-version specification in packages.
    #[arg(long)]
    ignore_rust_version: bool,
    /// Output information how long each compilation takes.
    #[arg(long)]
    timings: Option<String>,
    /// Use verbose output. May be specified twice for very verbose output.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: Option<u8>,
    /// Do not print Cargo log messages.
    #[arg(short, long)]
    quiet: bool,
    /// Control when colored output is used.
    #[arg(long)]
    color: Option<String>,
    /// The output format for diagnostic messages.
    #[arg(long)]
    message_format: Option<String>,
    /// Path to the Cargo.toml file.
    #[arg(long)]
    manifest_path: Option<String>,
    /// Require Cargo.lock and cache are up to date.
    #[arg(long)]
    frozen: bool,
    /// Require Cargo.lock is up to date.
    #[arg(long)]
    locked: bool,
    /// Run without accessing the network.
    #[arg(long)]
    offline: bool,
    /// Number of parallel jobs to run.
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Build as many crates in the dependency graph as possible.
    #[arg(long)]
    keep_going: bool,
    /// Display a future-incompat report for future-incompatible warnings.
    #[arg(long)]
    future_incompat_report: bool,
    /// Test all members in the workspace.
    #[arg(long)]
    workspace: bool,
    /// Alias for --workspace.
    #[arg(long)]
    all: bool,
    /// Test name filters.
    testname: Vec<String>,
    /// Arguments passed to each test binary.
    #[arg(last = true, num_args = 0..)]
    test_args: Vec<String>,
}

fn main() {
    let started = std::time::Instant::now();
    match try_main(started) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("Error: {err:#}");
            std::process::exit(1);
        }
    }
}

fn workspace_root() -> anyhow::Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(workspace_root::get_workspace_root_directory()?)
        .map_err(|path| anyhow!("Unexpected characters in workspace root: {path:?}"))
}

fn try_main(started: std::time::Instant) -> anyhow::Result<i32> {
    let mut args = std::env::args().collect::<Vec<_>>();
    // Cargo passes the subcommand name to external subcommands.
    if args.get(1).is_some_and(|arg| arg == "test-r") {
        args.remove(1);
    }
    let command = Command::parse_from(&args);
    match command {
        Command::ReuseNextestArchive(cmd) => reuse_nextest_archive(&workspace_root()?, cmd),
        Command::List(cmd) => {
            let selection = cmd.select()?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &selection)?;
            println!();
            Ok(0)
        }
        Command::Run(cmd) => run(cmd, &args[2..], started),
        Command::Snapshot(cmd) => {
            let digest = snapshot::snapshot(&cmd.selection, &cmd.out)?;
            println!("snapshot {} {digest}", cmd.out);
            Ok(0)
        }
        Command::Verify(cmd) => {
            let (selection, digest) = Selection::read(&cmd.selection)?;
            snapshot::verify(
                &cmd.selection,
                &selection,
                &digest,
                cmd.verification.expect_digest.as_deref(),
                cmd.verification.skip_library_hashes,
            )?;
            Ok(0)
        }
    }
}

fn reuse_nextest_archive(
    workspace: &Utf8Path,
    cmd: ReuseNextestArchiveSubcommand,
) -> anyhow::Result<i32> {
    let target = selection::archive_target(workspace, &cmd.target_dir);
    if !target.exists() {
        println!("Creating target directory: {target}");
        std::fs::create_dir_all(&target)?;
    }
    if cmd.archive_file.is_file() {
        let archive_size = std::fs::metadata(&cmd.archive_file)?.len();
        println!(
            "Extracting {} to {} ({})",
            cmd.archive_file,
            target,
            format_size(archive_size, BINARY)
        );
        selection::extract_tar_zstd(&cmd.archive_file, &target)?;
    } else {
        println!("Moving files from {} to {}", cmd.archive_file, target);
        fs_extra::dir::move_dir(
            &cmd.archive_file,
            &target,
            &fs_extra::dir::CopyOptions::new()
                .overwrite(true)
                .content_only(true),
        )?;
    }
    Ok(0)
}

/// What `run` executes, resolved and checked before any process starts.
enum RunInput {
    /// Prebuilt binaries, and the SHA-256 that identifies them in events.
    Prebuilt {
        selection: Selection,
        digest: String,
    },
    /// No prebuilt input: forward to `cargo test`.
    Cargo,
}

impl RunSubcommand {
    fn resolve(&self) -> anyhow::Result<RunInput> {
        let verification = &self.verification;
        if let Some(path) = &self.selection {
            validate_supported_test_args(self)?;
            if !self.selection_args.filters().is_empty() || self.selection_args.target_dir.is_some()
            {
                bail!(
                    "--selection runs exactly the saved selection; package/target filters and --target-dir cannot be combined with it"
                );
            }
            let (selection, digest) = Selection::read(path)?;
            if verification.skip_library_hashes && !snapshot::requires_verification(&selection) {
                bail!("--skip-library-hashes applies only to snapshots; {path} is not one");
            }
            if snapshot::requires_verification(&selection) || verification.expect_digest.is_some() {
                snapshot::verify(
                    path,
                    &selection,
                    &digest,
                    verification.expect_digest.as_deref(),
                    verification.skip_library_hashes,
                )?;
            }
            return Ok(RunInput::Prebuilt { selection, digest });
        }
        if verification.expect_digest.is_some() || verification.skip_library_hashes {
            bail!("--expect-digest and --skip-library-hashes require --selection");
        }
        if self.selection_args.has_prebuilt_input()? {
            validate_supported_test_args(self)?;
            let selection = self.selection_args.select()?;
            let digest = selection::sha256(&serde_json::to_vec(&selection)?);
            return Ok(RunInput::Prebuilt { selection, digest });
        }
        if self.events.is_some() {
            bail!(
                "--events is supported only for prebuilt runs; the Cargo fallback cannot write lifecycle events"
            );
        }
        if !self.selection_args.binary_id.is_empty() {
            bail!("--binary-id requires prebuilt metadata or an extracted archive");
        }
        Ok(RunInput::Cargo)
    }
}

fn run(
    cmd: RunSubcommand,
    cargo_args: &[String],
    started: std::time::Instant,
) -> anyhow::Result<i32> {
    let options = run::Options {
        no_fail_fast: cmd.no_fail_fast,
        deadline: cmd.deadline,
        kill_grace: cmd.kill_grace,
        keep_leftovers: cmd.keep_leftovers,
    };
    match cmd.resolve()? {
        RunInput::Prebuilt { selection, digest } => {
            // A supervisor that cannot start must not leave a log that looks
            // like an interrupted run.
            let mut supervisor =
                supervise::Supervisor::new(started, options.deadline, options.kill_grace)?;
            let mut events =
                events::Events::new(cmd.events.as_deref(), cmd.invocation_id, started)?;
            events.started(&digest, &options);
            let mut args = cmd.testname;
            args.extend(cmd.test_args);
            let result = run::run(&selection, &args, &options, &mut supervisor, &mut events);
            Ok(events.finish(&result))
        }
        RunInput::Cargo => {
            println!("No nextest archive was found, forwarding to cargo test");
            let cargo_path = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let mut args = run::cargo_args(cargo_args);
            args.insert(0, "test".into());
            println!("Executing {} {}", cargo_path, args.join(" "));
            let mut command = std::process::Command::new(cargo_path);
            command.args(args);
            let Some(deadline) = options.deadline else {
                // Nothing to enforce: run `cargo test` exactly as a direct call would,
                // in the terminal's foreground process group with default signals.
                return Ok(run::BinaryOutcome::from(command.status()?).exit_code());
            };
            let mut supervisor =
                supervise::Supervisor::new(started, Some(deadline), options.kill_grace)?;
            let outcome = supervisor.run(&mut command, false, None);
            Ok(supervisor
                .stop_if_requested()
                .map_or(outcome, run::BinaryOutcome::from)
                .exit_code())
        }
    }
}

fn validate_supported_test_args(cmd: &RunSubcommand) -> anyhow::Result<()> {
    let unsupported = [
        (cmd.no_run, "--no-run"),
        (cmd.bin.is_some(), "--bin"),
        (cmd.example.is_some(), "--example"),
        (cmd.examples, "--examples"),
        (cmd.bench.is_some(), "--bench"),
        (cmd.benches, "--benches"),
        (cmd.all_targets, "--all-targets"),
        (cmd.doc, "--doc"),
        (cmd.features.is_some(), "--features"),
        (cmd.no_default_features, "--no-default-features"),
        (cmd.target.is_some(), "--target"),
        (cmd.release, "--release"),
        (cmd.profile.is_some(), "--profile"),
        (cmd.ignore_rust_version, "--ignore-rust-version"),
        (cmd.timings.is_some(), "--timings"),
        (cmd.color.is_some(), "--color"),
        (cmd.message_format.is_some(), "--message-format"),
        (cmd.manifest_path.is_some(), "--manifest-path"),
        (cmd.frozen, "--frozen"),
        (cmd.locked, "--locked"),
        (cmd.keep_going, "--keep-going"),
        (cmd.future_incompat_report, "--future-incompat-report"),
    ];
    for (present, flag) in unsupported {
        if present {
            bail!("{flag} is not supported");
        }
    }
    Ok(())
}
