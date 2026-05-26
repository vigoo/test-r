use crate::args::{Arguments, TimeThreshold};
use crate::bench::Bencher;
use crate::execution::{DepWireBytes, TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests_sync, get_ensure_time, CapturedOutput, CloneableCodec, DepScope, FailureCause,
    FlakinessControl, HostedRpcChannel, HostedRpcError, HostedRpcOwnerCell, HostedRpcTransport,
    InProcessHostedRpcTransport, RegisteredDependency, RegisteredTest, RpcFactory, SuiteResult,
    TestFunction, TestResult, WorkerReconstructor,
};
use crate::ipc::{ipc_name, read_frame, write_frame, HostedRpcReplyBody, IpcCommand, IpcResponse};
use crate::output::{test_runner_output, TestRunnerOutput};
use desert_rust::{deserialize, serialize_to_byte_vec};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName};
use std::any::Any;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use std::time::Instant;
use uuid::Uuid;

pub fn test_runner() -> ExitCode {
    crate::panic_hook::install_panic_hook();
    let mut args = Arguments::from_args();
    // When the parent spawned this process as a worker it passed
    // `--worker-index <N>`. Stash it so `crate::worker::worker_index()`
    // returns the correct value for PerWorker dep constructors.
    if let Some(idx) = args.worker_index {
        crate::worker::set_worker_index(idx);
    }
    let output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let registered_dependency_constructors =
        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS.lock().unwrap();
    let registered_testsuite_props = internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap();
    let registered_test_generators = internal::REGISTERED_TEST_GENERATORS.lock().unwrap();

    let generated_tests = generate_tests_sync(&registered_test_generators);

    let all_tests: Vec<RegisteredTest> = registered_tests
        .iter()
        .cloned()
        .chain(generated_tests)
        .collect();

    if args.list {
        output.test_list(&all_tests);
        ExitCode::SUCCESS
    } else {
        let mut remaining_retries = args.flaky_run.unwrap_or(1);
        let mut exit_code = ExitCode::from(101);

        while remaining_retries > 0 {
            let (mut execution, filtered_tests) = TestSuiteExecution::construct(
                &args,
                registered_dependency_constructors.as_slice(),
                &all_tests,
                registered_testsuite_props.as_slice(),
            );
            args.finalize_for_execution(&execution, output.clone());
            let is_top_level_parent = args.is_top_level_parent();
            let has_selected_tests = execution.remaining() > 0;
            // Parent-side collection for dependency scopes whose worker-side
            // value is shipped as bytes or represented as an RPC stub. This is
            // skipped when the filter selected no tests so expensive Hosted
            // resources are not started for an empty run.
            let needs_parent_shared = execution.has_cloneable_dependencies()
                || execution.has_hosted_dependencies()
                || execution.has_hosted_rpc_dependencies();
            let parent_shared = if is_top_level_parent && has_selected_tests && needs_parent_shared
            {
                execution.collect_parent_shared_dependencies_sync()
            } else {
                crate::execution::ParentSharedDependencies {
                    cloneable_wire_bytes: Vec::new(),
                    cloneable_local_values: Vec::new(),
                    hosted_descriptor_bytes: Vec::new(),
                    hosted_owners: Vec::new(),
                    hosted_rpc_owner_cells: Vec::new(),
                }
            };
            let cloneable_wire_bytes = parent_shared.cloneable_wire_bytes;
            let cloneable_local_values = parent_shared.cloneable_local_values;
            let hosted_descriptor_bytes = parent_shared.hosted_descriptor_bytes;
            let _hosted_owners = parent_shared.hosted_owners;
            let hosted_rpc_owner_cells: HashMap<String, Arc<HostedRpcOwnerCell>> =
                parent_shared.hosted_rpc_owner_cells.into_iter().collect();
            // Build a Cloneable/Hosted codec/worker lookup table now, before
            // `test_thread` workers are spawned, so the test_thread workers
            // do not need to lock the global REGISTERED_DEPENDENCY_CONSTRUCTORS
            // (which `test_runner` already holds).
            // Keyed by the dep's fully-qualified id (`{crate}::{module}::{name}`)
            // so that workers can route an incoming `ProvideCloneable` /
            // `ProvideHostedDescriptor` to the correct dep even when two deps
            // share a local `name` in different modules.
            let wire_codecs: HashMap<String, (CloneableCodec, WorkerReconstructor)> =
                build_worker_wire_codecs(&registered_dependency_constructors);
            // Pre-built RpcFactory lookup keyed by qualified id, so worker
            // subprocesses can build stubs without re-locking the global
            // REGISTERED_DEPENDENCY_CONSTRUCTORS.
            let rpc_factories: HashMap<String, RpcFactory> =
                build_rpc_factories(&registered_dependency_constructors);
            // Mode-consistent Cloneable semantics for the no-spawn-workers
            // path: reuse the parent-constructed value directly instead of
            // re-running the user constructor in `materialize_deps_sync`.
            // Without this, a Cloneable dep's constructor would run twice
            // (once for parent-side `collect_parent_shared_dependencies_sync`
            // and once for the in-process test execution), which both
            // violates the "constructor runs once" expectation and can
            // deadlock when the constructor takes a runtime-wide lock.
            if is_top_level_parent && !args.spawn_workers && !cloneable_local_values.is_empty() {
                apply_cloneable_values_locally(&mut execution, &cloneable_local_values);
            }
            // Mode-consistent Hosted semantics: when this is the top-level
            // parent AND we do NOT spawn workers (e.g. --nocapture, single
            // process), the test functions run in this same process, but
            // they must still see the *worker-side handle* produced by
            // `HostedDep::from_descriptor` — not the raw owner value.
            // Reconstruct each handle locally via the descriptor round-trip
            // and pre-populate the execution tree, so `materialize_deps_sync`
            // skips re-running the constructor.
            if is_top_level_parent && !args.spawn_workers && !hosted_descriptor_bytes.is_empty() {
                apply_hosted_descriptors_locally(
                    &mut execution,
                    &wire_codecs,
                    &hosted_descriptor_bytes,
                );
            }
            // Mode-consistent HostedRpc semantics for the no-spawn-workers
            // path: install in-process stubs that route straight to the
            // parent-held owner cells, so tests see the same `Stub` value
            // whether or not the runner spawns workers.
            if is_top_level_parent && !args.spawn_workers && !hosted_rpc_owner_cells.is_empty() {
                install_local_hosted_rpc_stubs(
                    &mut execution,
                    &rpc_factories,
                    &hosted_rpc_owner_cells,
                );
            }
            if args.spawn_workers {
                execution.skip_creating_dependencies();
            }

            // println!("Execution plan: {execution:?}");
            // println!("Final args: {args:?}");
            // println!("Has dependencies: {:?}", execution.has_dependencies());

            let count = execution.remaining();
            let mut results = Vec::with_capacity(count);

            let start = Instant::now();
            output.start_suite(&filtered_tests);

            let execution = Arc::new(Mutex::new(execution));
            let cloneable_wire_bytes = Arc::new(cloneable_wire_bytes);
            let hosted_descriptor_bytes = Arc::new(hosted_descriptor_bytes);
            let wire_codecs = Arc::new(wire_codecs);
            let rpc_factories = Arc::new(rpc_factories);
            let hosted_rpc_owner_cells = Arc::new(hosted_rpc_owner_cells);
            let threads = args.test_threads().get();
            let mut handles = Vec::with_capacity(threads);
            for worker_idx in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                // Stamp each test-thread's args with the worker index it will
                // hand to its spawned child via `--worker-index <N>`. The
                // parent process itself never observes this field (only
                // children read it back through `worker::set_worker_index`).
                let mut args_clone = args.clone();
                if args_clone.spawn_workers {
                    args_clone.worker_index = Some(worker_idx);
                }
                let wire_bytes_clone = cloneable_wire_bytes.clone();
                let hosted_bytes_clone = hosted_descriptor_bytes.clone();
                let codecs_clone = wire_codecs.clone();
                let rpc_factories_clone = rpc_factories.clone();
                let hosted_rpc_owner_cells_clone = hosted_rpc_owner_cells.clone();
                handles.push(spawn(move || {
                    test_thread(
                        args_clone,
                        execution_clone,
                        output_clone,
                        count,
                        wire_bytes_clone,
                        hosted_bytes_clone,
                        codecs_clone,
                        rpc_factories_clone,
                        hosted_rpc_owner_cells_clone,
                    )
                }));
            }

            for handle in handles {
                results.extend(handle.join().unwrap());
            }

            drop(execution);

            output.finished_suite(&all_tests, &results, start.elapsed());
            exit_code = SuiteResult::exit_code(&results);

            if exit_code == ExitCode::SUCCESS {
                break;
            } else {
                remaining_retries -= 1;
            }
        }
        exit_code
    }
}

#[allow(clippy::too_many_arguments)]
fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
    cloneable_wire_bytes: Arc<Vec<DepWireBytes>>,
    hosted_descriptor_bytes: Arc<Vec<DepWireBytes>>,
    wire_codecs: Arc<HashMap<String, (CloneableCodec, WorkerReconstructor)>>,
    rpc_factories: Arc<HashMap<String, RpcFactory>>,
    hosted_rpc_owner_cells: Arc<HashMap<String, Arc<HostedRpcOwnerCell>>>,
) -> Vec<(RegisteredTest, TestResult)> {
    let mut worker = spawn_worker_if_needed(&args);
    // Parent dispatches incoming `HostedRpcCall` frames against the owner
    // cells materialised in the top-level parent. Workers don't need the owner
    // cells (they own stubs instead), so they receive an empty map and the
    // dispatch code path is never reached in subprocesses.
    if let Some(worker) = worker.as_mut() {
        worker.set_hosted_rpc_owner_cells(hosted_rpc_owner_cells.clone());
    }
    let connection_arc = if let Some(ref name) = args.ipc {
        let name = ipc_name(name.clone());
        let stream = Stream::connect(name).expect("Failed to connect to IPC socket");
        Some(Arc::new(Mutex::new(stream)))
    } else {
        None
    };

    // If we own a worker (parent side), eagerly ship every Cloneable wire
    // payload to it. Workers stash them into the execution tree as
    // pre-materialised values so the original constructor never runs on the
    // worker side. The `dep_id` carried across the wire is the dep's
    // fully-qualified id, not its local `name`, so same-named deps in different
    // modules don't collide.
    if let Some(worker) = worker.as_mut() {
        for (dep_id, wire_bytes) in cloneable_wire_bytes.iter() {
            worker.provide_cloneable(dep_id.clone(), wire_bytes.clone());
        }
        // Ship every Hosted dep's descriptor bytes too. Workers run the
        // registered worker_fn (HostedDep::from_descriptor) to build a
        // per-worker handle pointing at the parent-held owner.
        for (dep_id, descriptor_bytes) in hosted_descriptor_bytes.iter() {
            worker.provide_hosted_descriptor(dep_id.clone(), descriptor_bytes.clone());
        }
    }

    // Worker subprocess side: build a stub for every HostedRpc dep registered
    // in this binary using the IPC-backed transport sharing the same socket as
    // the main IPC loop. Install the stubs in the execution tree so
    // materialize_deps_sync skips the parent-only owner constructor.
    if let Some(connection) = connection_arc.as_ref() {
        if !rpc_factories.is_empty() {
            install_worker_subprocess_hosted_rpc_stubs(
                &execution,
                &rpc_factories,
                connection.clone(),
            );
        }
    }

    let mut results = Vec::with_capacity(count);
    let mut expected_test = None;

    while !is_done(&execution) {
        if let Some(connection) = connection_arc.as_ref() {
            while expected_test.is_none() {
                let command_bytes = {
                    let mut conn = connection.lock().unwrap();
                    read_frame(&mut *conn).expect("Failed to read IPC command frame")
                };
                let command: IpcCommand =
                    deserialize(&command_bytes).expect("Failed to decode IPC command");

                match command {
                    IpcCommand::RunTest {
                        name,
                        crate_name,
                        module_path,
                    } => {
                        expected_test = Some((name, crate_name, module_path));
                    }
                    IpcCommand::ProvideCloneable { dep_id, wire_bytes } => {
                        // Worker-side: look up the registered Cloneable dep by
                        // its fully-qualified id, reconstruct the value via
                        // codec.from_wire_bytes + worker_fn, and stash it in
                        // the execution tree so materialize_deps_sync uses it
                        // instead of running the original constructor.
                        apply_provided_wire_bytes(
                            &execution,
                            &wire_codecs,
                            &dep_id,
                            &wire_bytes,
                            "ProvideCloneable",
                        );

                        let response = IpcResponse::CloneableAccepted { dep_id };
                        let msg = serialize_to_byte_vec(&response)
                            .expect("Failed to encode IPC response");
                        let mut conn = connection.lock().unwrap();
                        write_frame(&mut *conn, &msg).expect("Failed to write IPC response frame");
                    }
                    IpcCommand::ProvideHostedDescriptor { dep_id, wire_bytes } => {
                        // Worker-side: identical structure to ProvideCloneable,
                        // but the wire payload is the descriptor bytes and the
                        // registered worker_fn calls HostedDep::from_descriptor
                        // to produce the worker handle.
                        apply_provided_wire_bytes(
                            &execution,
                            &wire_codecs,
                            &dep_id,
                            &wire_bytes,
                            "ProvideHostedDescriptor",
                        );

                        let response = IpcResponse::HostedDescriptorAccepted { dep_id };
                        let msg = serialize_to_byte_vec(&response)
                            .expect("Failed to encode IPC response");
                        let mut conn = connection.lock().unwrap();
                        write_frame(&mut *conn, &msg).expect("Failed to write IPC response frame");
                    }
                    IpcCommand::HostedRpcReply { .. } => {
                        // Replies for worker-initiated HostedRpc calls are
                        // consumed inline by the IPC transport during test
                        // execution, never by this between-tests command
                        // loop. Receiving one here means the protocol got
                        // out of sync; surface that loudly rather than
                        // dropping the frame.
                        panic!(
                            "unexpected `HostedRpcReply` while waiting for the next \
                             `RunTest`/`Provide*` command — IPC protocol out of sync"
                        );
                    }
                }
            }
        }

        if let Some(next) = pick_next(&execution) {
            let skip = if let Some((name, crate_name, module_path)) = &expected_test {
                next.test.name != *name
                    || next.test.crate_name != *crate_name
                    || next.test.module_path != *module_path
            } else {
                false
            };

            if !skip {
                expected_test = None;

                output.start_running_test(&next.test, next.index, count);

                let result = if next.test.props.is_ignored && !args.include_ignored {
                    TestResult::Ignored {
                        captured: Vec::new(),
                    }
                } else if let Some(worker) = worker.as_mut() {
                    worker.run_test(args.nocapture, &next.test)
                } else {
                    let ensure_time = get_ensure_time(&args, &next.test);
                    run_sync_test_function(
                        output.clone(),
                        &next.test,
                        next.index,
                        count,
                        ensure_time,
                        next.deps.clone(),
                    )
                };

                output.finished_running_test(&next.test, next.index, count, &result);

                if let Some(connection) = connection_arc.as_ref() {
                    let finish_marker = Uuid::new_v4().to_string();
                    let finish_marker_line = format!("{finish_marker}\n");
                    std::io::stdout()
                        .write_all(finish_marker_line.as_bytes())
                        .unwrap();
                    std::io::stderr()
                        .write_all(finish_marker_line.as_bytes())
                        .unwrap();

                    std::io::stdout().flush().unwrap();
                    std::io::stderr().flush().unwrap();

                    let response = IpcResponse::TestFinished {
                        result: (&result).into(),
                        finish_marker,
                    };

                    let msg =
                        serialize_to_byte_vec(&response).expect("Failed to encode IPC response");
                    let mut conn = connection.lock().unwrap();
                    write_frame(&mut *conn, &msg).expect("Failed to write IPC response frame");
                }

                results.push((next.test.clone(), result));
            }
        }
    }
    results
}

fn is_done(execution: &Arc<Mutex<TestSuiteExecution>>) -> bool {
    let execution = execution.lock().unwrap();
    execution.is_done()
}

/// Worker-side handler for `IpcCommand::ProvideCloneable` and
/// `IpcCommand::ProvideHostedDescriptor`. Looks up the registered dep by its
/// fully-qualified id (`{crate}::{module}::{name}`) in the pre-built
/// `wire_codecs` map (built once at startup to avoid re-locking the global
/// registry held by `test_runner`), reconstructs its value from the wire
/// bytes via codec + worker_fn, and stores it in the execution tree so the
/// next `materialize_deps_sync` call uses the pre-resolved value. The
/// `command_name` only appears in panic messages so the source command is
/// identifiable.
fn apply_provided_wire_bytes(
    execution: &Arc<Mutex<TestSuiteExecution>>,
    wire_codecs: &HashMap<String, (CloneableCodec, WorkerReconstructor)>,
    dep_id: &str,
    wire_bytes: &[u8],
    command_name: &'static str,
) {
    let (codec, worker_fn) = wire_codecs
        .get(dep_id)
        .unwrap_or_else(|| panic!("{command_name} referenced unknown wire-shared dep '{dep_id}'"));

    let wire_payload = (codec.from_wire_bytes)(wire_bytes);
    let empty_deps: Arc<dyn internal::DependencyView + Send + Sync> =
        Arc::new(HashMap::<String, Arc<dyn Any + Send + Sync>>::new());
    let reconstructed = match worker_fn {
        WorkerReconstructor::Sync(f) => f(wire_payload, empty_deps),
        WorkerReconstructor::Async(_) => {
            panic!(
                "Async WorkerReconstructor for dep '{dep_id}' is not supported by the sync runner"
            );
        }
    };

    let mut execution = execution.lock().unwrap();
    let applied = execution.provide_cloneable_value(dep_id, reconstructed);
    assert!(
        applied,
        "{command_name} for dep '{dep_id}' did not match any registered dep in this worker"
    );
}

/// Mode-consistent Cloneable semantics for the no-spawn-workers path: takes
/// the parent-constructed Cloneable values (collected once by
/// `collect_parent_shared_dependencies_sync`) and installs them directly
/// into the parent's `TestSuiteExecution`, so `materialize_deps_sync`
/// reuses them instead of re-running the user constructor.
///
/// For `Cloneable`, the documented round-trip
/// `from_wire(to_wire(value))` is semantics-preserving, so reusing the
/// parent value directly is equivalent to round-tripping it through the
/// wire codec while avoiding the duplicate constructor run that would
/// otherwise occur on the no-spawn-workers code path. The duplicate run
/// historically caused user-visible problems (extra observable side
/// effects under `--nocapture`, and deadlocks when the constructor takes
/// a runtime-wide lock).
fn apply_cloneable_values_locally(
    execution: &mut TestSuiteExecution,
    cloneable_local_values: &[(String, Arc<dyn Any + Send + Sync>)],
) {
    for (dep_id, value) in cloneable_local_values {
        let applied = execution.provide_cloneable_value(dep_id, value.clone());
        assert!(
            applied,
            "Cloneable dep '{dep_id}' could not be pre-populated locally"
        );
    }
}

/// Mode-consistent Hosted semantics for the no-spawn-workers path: takes
/// the parent-collected descriptor bytes and reconstructs each Hosted
/// dep's worker-side handle (via the registered codec + worker_fn)
/// directly in the parent's `TestSuiteExecution`. This makes tests see the
/// same `HostedDep::from_descriptor` output whether or not the runner
/// spawns workers for capture.
fn apply_hosted_descriptors_locally(
    execution: &mut TestSuiteExecution,
    wire_codecs: &HashMap<String, (CloneableCodec, WorkerReconstructor)>,
    descriptor_bytes: &[DepWireBytes],
) {
    for (dep_id, wire_bytes) in descriptor_bytes {
        let (codec, worker_fn) = wire_codecs.get(dep_id).unwrap_or_else(|| {
            panic!("Hosted dep '{dep_id}' missing codec/worker_fn for local handle reconstruction")
        });
        let wire_payload = (codec.from_wire_bytes)(wire_bytes);
        let empty_deps: Arc<dyn internal::DependencyView + Send + Sync> =
            Arc::new(HashMap::<String, Arc<dyn Any + Send + Sync>>::new());
        let reconstructed = match worker_fn {
            WorkerReconstructor::Sync(f) => f(wire_payload, empty_deps),
            WorkerReconstructor::Async(_) => {
                panic!(
                    "Async WorkerReconstructor for Hosted dep '{dep_id}' is not supported by the sync runner"
                );
            }
        };
        let applied = execution.provide_cloneable_value(dep_id, reconstructed);
        assert!(
            applied,
            "Hosted dep '{dep_id}' could not be pre-populated locally"
        );
    }
}

/// Builds the combined Cloneable + Hosted codec/worker_fn lookup map used by
/// worker processes to reconstruct wire-shared deps. Keyed by qualified id.
fn build_worker_wire_codecs(
    registered: &[crate::internal::RegisteredDependency],
) -> HashMap<String, (CloneableCodec, WorkerReconstructor)> {
    registered
        .iter()
        .filter_map(|d| {
            let codec_opt = match d.scope {
                crate::internal::DepScope::Cloneable => d.cloneable_codec.as_ref(),
                crate::internal::DepScope::Hosted => d.hosted_codec.as_ref(),
                _ => None,
            };
            match (codec_opt, &d.worker_fn) {
                (Some(codec), Some(worker_fn)) => {
                    Some((d.qualified_id(), (codec.clone(), worker_fn.clone())))
                }
                _ => None,
            }
        })
        .collect()
}

/// Builds a lookup of HostedRpc `RpcFactory` entries, keyed by fully-qualified
/// dep id. Worker subprocesses use this to build a stub for each registered
/// HostedRpc dep without re-locking the global registry, and the parent uses it
/// in `install_local_hosted_rpc_stubs` for the no-spawn-workers path.
fn build_rpc_factories(registered: &[RegisteredDependency]) -> HashMap<String, RpcFactory> {
    registered
        .iter()
        .filter_map(|d| {
            if d.scope == DepScope::HostedRpc {
                d.rpc_factory
                    .as_ref()
                    .map(|f| (d.qualified_id(), f.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Parent-side `--nocapture` / no-spawn-workers helper. Builds one stub per
/// HostedRpc dep using an [`InProcessHostedRpcTransport`] that points at the
/// parent-held owner cells, and stashes it in the execution tree so
/// `materialize_deps_sync` skips the owner-only constructor.
fn install_local_hosted_rpc_stubs(
    execution: &mut TestSuiteExecution,
    rpc_factories: &HashMap<String, RpcFactory>,
    owner_cells: &HashMap<String, Arc<HostedRpcOwnerCell>>,
) {
    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(InProcessHostedRpcTransport::new(owner_cells.clone()));
    for (dep_id, factory) in rpc_factories.iter() {
        if !owner_cells.contains_key(dep_id) {
            // No owner cell materialised for this dep (e.g. the dep is
            // registered globally but not pulled into the current filter).
            // Skip so we don't try to install a stub that nothing routes.
            continue;
        }
        let channel = HostedRpcChannel::new(dep_id.clone(), transport.clone());
        let stub = (factory.build_stub)(channel);
        let applied = execution.provide_cloneable_value(dep_id, stub);
        assert!(
            applied,
            "Local HostedRpc stub for '{dep_id}' did not match any registered dep"
        );
    }
}

/// Worker subprocess helper. Builds one stub per registered HostedRpc dep using
/// an IPC-backed transport that shares the connection `connection_arc` with the
/// main IPC command loop, and stashes the stub in the execution tree.
fn install_worker_subprocess_hosted_rpc_stubs(
    execution: &Arc<Mutex<TestSuiteExecution>>,
    rpc_factories: &HashMap<String, RpcFactory>,
    connection_arc: Arc<Mutex<Stream>>,
) {
    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(IpcHostedRpcTransport::new(connection_arc));
    for (dep_id, factory) in rpc_factories.iter() {
        let channel = HostedRpcChannel::new(dep_id.clone(), transport.clone());
        let stub = (factory.build_stub)(channel);
        let mut execution = execution.lock().unwrap();
        let applied = execution.provide_cloneable_value(dep_id, stub);
        // Not every binary that registers a HostedRpc dep will use it; if
        // the current execution tree doesn't reference it, just move on.
        let _ = applied;
    }
}

/// Worker subprocess `HostedRpcTransport` that sends one
/// `IpcResponse::HostedRpcCall` over the shared IPC stream and blocks until a
/// matching `IpcCommand::HostedRpcReply` comes back. Calls serialize on the
/// shared `Arc<Mutex<Stream>>` so they never interleave with the main command
/// loop (the main loop only reads frames between tests; stubs only call while a
/// test is mid-execution).
struct IpcHostedRpcTransport {
    connection: Arc<Mutex<Stream>>,
    next_request_id: AtomicU64,
}

impl IpcHostedRpcTransport {
    fn new(connection: Arc<Mutex<Stream>>) -> Self {
        Self {
            connection,
            next_request_id: AtomicU64::new(1),
        }
    }
}

impl HostedRpcTransport for IpcHostedRpcTransport {
    fn call(
        &self,
        dep_id: &str,
        method_idx: u32,
        args: Vec<u8>,
    ) -> Result<Vec<u8>, HostedRpcError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let call = IpcResponse::HostedRpcCall {
            request_id,
            dep_id: dep_id.to_string(),
            method_idx,
            args_bytes: args,
        };
        let msg = serialize_to_byte_vec(&call).map_err(|e| {
            HostedRpcError::Transport(format!("encode HostedRpcCall failed: {e:?}"))
        })?;
        // Hold the connection lock for the whole request/response so other
        // potential callers serialise behind us. This keeps one outstanding
        // call at a time on the shared worker connection.
        let mut conn = self
            .connection
            .lock()
            .map_err(|e| HostedRpcError::Transport(format!("connection mutex poisoned: {e}")))?;
        write_frame(&mut *conn, &msg)
            .map_err(|e| HostedRpcError::Transport(format!("write HostedRpcCall failed: {e}")))?;
        // One outstanding call at a time per connection, so the next command
        // frame must be the matching HostedRpcReply.
        let reply_bytes = read_frame(&mut *conn)
            .map_err(|e| HostedRpcError::Transport(format!("read reply failed: {e}")))?;
        let command: IpcCommand = deserialize(&reply_bytes).map_err(|e| {
            HostedRpcError::Transport(format!("decode HostedRpcReply failed: {e:?}"))
        })?;
        match command {
            IpcCommand::HostedRpcReply {
                request_id: reply_id,
                body,
            } => {
                if reply_id != request_id {
                    return Err(HostedRpcError::Transport(format!(
                        "HostedRpcReply request_id mismatch: expected {request_id}, got {reply_id}"
                    )));
                }
                match body {
                    HostedRpcReplyBody::Ok { result_bytes } => Ok(result_bytes),
                    HostedRpcReplyBody::Err { message } => Err(HostedRpcError::Dispatch(message)),
                }
            }
            other => Err(HostedRpcError::Transport(format!(
                "unexpected IpcCommand while waiting for HostedRpcReply: {other:?}"
            ))),
        }
    }
}

fn pick_next(execution: &Arc<Mutex<TestSuiteExecution>>) -> Option<TestExecution> {
    let mut execution = execution.lock().unwrap();
    execution.pick_next_sync()
}

fn run_with_flakiness_control(
    output: Arc<dyn TestRunnerOutput>,
    test_description: &RegisteredTest,
    idx: usize,
    count: usize,
    test: impl Fn(Instant) -> Result<Result<(), FailureCause>, Box<dyn Any + Send>>,
) -> Result<Result<(), FailureCause>, Box<dyn Any + Send>> {
    match &test_description.props.flakiness_control {
        FlakinessControl::None => {
            let start = Instant::now();
            test(start)
        }
        FlakinessControl::ProveNonFlaky(tries) => {
            for n in 0..*tries {
                if n > 0 {
                    output.repeat_running_test(
                        test_description,
                        idx,
                        count,
                        n + 1,
                        *tries,
                        "to ensure test is not flaky",
                    );
                }
                let start = Instant::now();
                match test(start) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Ok(Err(e)),
                    Err(e) => return Err(e),
                };
            }
            Ok(Ok(()))
        }
        FlakinessControl::RetryKnownFlaky(max_retries) => {
            let mut tries = 1;
            loop {
                let start = Instant::now();
                let result = test(start);

                if result.is_err() && tries < *max_retries {
                    tries += 1;
                    output.repeat_running_test(
                        test_description,
                        idx,
                        count,
                        tries,
                        *max_retries,
                        "because test is known to be flaky",
                    );
                } else {
                    break result;
                }
            }
        }
    }
}

#[allow(unreachable_patterns)]
pub(crate) fn run_sync_test_function(
    output: Arc<dyn TestRunnerOutput>,
    test_description: &RegisteredTest,
    idx: usize,
    count: usize,
    ensure_time: Option<TimeThreshold>,
    dependency_view: Arc<dyn internal::DependencyView + Send + Sync>,
) -> TestResult {
    let start = Instant::now();
    match test_description.run.clone() {
        TestFunction::Sync(test_fn) => {
            let detached_panic_policy = test_description.props.detached_panic_policy.clone();
            let result =
                run_with_flakiness_control(output, test_description, idx, count, move |start| {
                    let dependency_view = dependency_view.clone();
                    let test_fn = test_fn.clone();
                    let test_id = crate::panic_hook::next_test_id();
                    crate::panic_hook::set_current_test_id(test_id);
                    crate::panic_hook::create_detached_collector(test_id);
                    catch_unwind(AssertUnwindSafe(move || {
                        test_fn(dependency_view).into_result()?;
                        if let Some(ensure_time) = ensure_time {
                            let elapsed = start.elapsed();
                            if ensure_time.is_critical(&elapsed) {
                                return Err(FailureCause::HarnessError(format!(
                                    "Test run time exceeds critical threshold: {elapsed:?}"
                                )));
                            }
                        };
                        Ok(())
                    }))
                });
            let mut test_result = TestResult::from_result(
                &test_description.props.should_panic,
                start.elapsed(),
                result,
            );
            if let Some(test_id) = crate::panic_hook::current_test_id() {
                if let Some(collector) = crate::panic_hook::take_detached_collector(test_id) {
                    let panics = match collector.lock() {
                        Ok(p) => p,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if !panics.is_empty()
                        && detached_panic_policy == internal::DetachedPanicPolicy::FailTest
                        && test_result.is_passed()
                    {
                        let messages: Vec<String> = panics.iter().map(|p| p.render()).collect();
                        test_result = TestResult::failed(
                            start.elapsed(),
                            FailureCause::Panic(internal::PanicCause {
                                message: Some(format!(
                                    "Detached task(s) panicked:\n{}",
                                    messages.join("\n---\n")
                                )),
                                location: panics.first().and_then(|p| p.location.clone()),
                                backtrace: panics.first().and_then(|p| p.backtrace.clone()),
                            }),
                        );
                    }
                }
            }
            crate::panic_hook::clear_current_test_id();
            test_result
        }
        TestFunction::SyncBench(bench_fn) => {
            let detached_panic_policy = test_description.props.detached_panic_policy.clone();
            let mut bencher = Bencher::new();
            let test_id = crate::panic_hook::next_test_id();
            crate::panic_hook::set_current_test_id(test_id);
            crate::panic_hook::create_detached_collector(test_id);
            let result = catch_unwind(AssertUnwindSafe(|| {
                bench_fn(&mut bencher, dependency_view);
                bencher
                    .summary()
                    .expect("iter() was not called in bench function")
            }));
            let mut test_result = TestResult::from_summary(
                &test_description.props.should_panic,
                start.elapsed(),
                result,
                bencher.bytes,
            );
            if let Some(test_id) = crate::panic_hook::current_test_id() {
                if let Some(collector) = crate::panic_hook::take_detached_collector(test_id) {
                    let panics = match collector.lock() {
                        Ok(p) => p,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if !panics.is_empty()
                        && detached_panic_policy == internal::DetachedPanicPolicy::FailTest
                        && test_result.is_passed()
                    {
                        let messages: Vec<String> = panics.iter().map(|p| p.render()).collect();
                        test_result = TestResult::failed(
                            start.elapsed(),
                            FailureCause::Panic(internal::PanicCause {
                                message: Some(format!(
                                    "Detached task(s) panicked:\n{}",
                                    messages.join("\n---\n")
                                )),
                                location: panics.first().and_then(|p| p.location.clone()),
                                backtrace: panics.first().and_then(|p| p.backtrace.clone()),
                            }),
                        );
                    }
                }
            }
            crate::panic_hook::clear_current_test_id();
            test_result
        }
        _ => {
            panic!("Async tests are not supported in sync mode, enable the 'tokio' feature")
        }
    }
}

struct Worker {
    listener: interprocess::local_socket::Listener,
    process: Child,
    out_handle: JoinHandle<()>,
    err_handle: JoinHandle<()>,
    out_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
    err_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
    capture_enabled: Arc<Mutex<bool>>,
    connection: Stream,
    /// Parent-held HostedRpc owner cells keyed by fully-qualified dep id. Used
    /// to dispatch incoming `IpcResponse::HostedRpcCall` frames from the worker
    /// subprocess back to the right owner.
    hosted_rpc_owner_cells: Arc<HashMap<String, Arc<HostedRpcOwnerCell>>>,
}

impl Worker {
    /// Installs the parent-side map of HostedRpc owner cells so this worker can
    /// route incoming `IpcResponse::HostedRpcCall` frames to the right
    /// `HostedRpcOwnerCell` while waiting for a worker subprocess response.
    fn set_hosted_rpc_owner_cells(&mut self, cells: Arc<HashMap<String, Arc<HostedRpcOwnerCell>>>) {
        self.hosted_rpc_owner_cells = cells;
    }

    /// Parent-side dispatcher for a single `IpcResponse::HostedRpcCall`. Looks
    /// up the owner cell by fully-qualified dep id, runs the dispatch on the
    /// parent's stored owner, and writes the matching
    /// `IpcCommand::HostedRpcReply` back to the worker subprocess.
    fn handle_hosted_rpc_call(
        &mut self,
        dump_on_ipc_failure: &DumpOnFailure,
        request_id: u64,
        dep_id: String,
        method_idx: u32,
        args_bytes: Vec<u8>,
    ) {
        let body = match self.hosted_rpc_owner_cells.get(&dep_id) {
            Some(cell) => match cell.dispatch(method_idx, &args_bytes) {
                Ok(result_bytes) => HostedRpcReplyBody::Ok { result_bytes },
                Err(message) => HostedRpcReplyBody::Err { message },
            },
            None => HostedRpcReplyBody::Err {
                message: format!(
                    "HostedRpc dispatch: unknown dep id '{dep_id}' in parent owner-cell map"
                ),
            },
        };
        let reply = IpcCommand::HostedRpcReply { request_id, body };
        let msg = serialize_to_byte_vec(&reply).expect("Failed to encode HostedRpcReply");
        dump_on_ipc_failure.run(write_frame(&mut self.connection, &msg));
    }

    pub fn run_test(&mut self, nocapture: bool, test: &RegisteredTest) -> TestResult {
        let mut capture_enabled = self.capture_enabled.lock().unwrap();
        *capture_enabled = test.props.capture_control.requires_capturing(!nocapture);
        drop(capture_enabled);

        // Send IPC command and wait for IPC response, and in the meantime read from the stdout/stderr channels
        let cmd = IpcCommand::RunTest {
            name: test.name.clone(),
            crate_name: test.crate_name.clone(),
            module_path: test.module_path.clone(),
        };

        let dump_on_ipc_failure = self.dump_on_failure();

        let msg = serialize_to_byte_vec(&cmd).expect("Failed to encode IPC command");
        dump_on_ipc_failure.run(write_frame(&mut self.connection, &msg));

        let response = loop {
            let response_bytes = dump_on_ipc_failure.run(read_frame(&mut self.connection));
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes));
            match response {
                IpcResponse::TestFinished { .. } => break response,
                IpcResponse::CloneableAccepted { .. }
                | IpcResponse::HostedDescriptorAccepted { .. } => {
                    // Out-of-band ack from a previous Provide*; ignore.
                    continue;
                }
                IpcResponse::HostedRpcCall {
                    request_id,
                    dep_id,
                    method_idx,
                    args_bytes,
                } => {
                    self.handle_hosted_rpc_call(
                        &dump_on_ipc_failure,
                        request_id,
                        dep_id,
                        method_idx,
                        args_bytes,
                    );
                    continue;
                }
            }
        };

        let IpcResponse::TestFinished {
            result,
            finish_marker,
        } = response
        else {
            unreachable!("loop only breaks on TestFinished")
        };

        if test.props.capture_control.requires_capturing(!nocapture) {
            let out_lines: Vec<_> =
                Self::drain_until(self.out_lines.clone(), finish_marker.clone());
            let err_lines: Vec<_> =
                Self::drain_until(self.err_lines.clone(), finish_marker.clone());
            result.into_test_result(out_lines, err_lines)
        } else {
            result.into_test_result(Vec::new(), Vec::new())
        }
    }

    /// Sends a Cloneable wire payload to this worker process and waits for
    /// the matching `CloneableAccepted` response. `dep_id` is the dep's
    /// fully-qualified id (`{crate}::{module}::{name}`). Discards any
    /// unexpected `TestFinished` payloads (none are expected before a
    /// `RunTest`, but the loop keeps the IPC channel in lockstep regardless).
    fn provide_cloneable(&mut self, dep_id: String, wire_bytes: Vec<u8>) {
        let dump_on_ipc_failure = self.dump_on_failure();
        let cmd = IpcCommand::ProvideCloneable {
            dep_id: dep_id.clone(),
            wire_bytes,
        };
        let msg = serialize_to_byte_vec(&cmd).expect("Failed to encode IPC command");
        dump_on_ipc_failure.run(write_frame(&mut self.connection, &msg));

        loop {
            let response_bytes = dump_on_ipc_failure.run(read_frame(&mut self.connection));
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes));
            match response {
                IpcResponse::CloneableAccepted { dep_id: ack_id } => {
                    if ack_id == dep_id {
                        return;
                    }
                }
                IpcResponse::HostedDescriptorAccepted { .. } => {
                    // Out-of-band ack from a previous ProvideHostedDescriptor; ignore.
                }
                IpcResponse::TestFinished { .. } => {
                    // Should not happen before any RunTest.
                }
                IpcResponse::HostedRpcCall {
                    request_id,
                    dep_id: call_dep_id,
                    method_idx,
                    args_bytes,
                } => {
                    // Defensive: a worker subprocess shouldn't emit
                    // HostedRpcCall before its first RunTest, but if it does
                    // (e.g. stub built during a ProvideCloneable round-trip in
                    // a future extension) we still dispatch.
                    self.handle_hosted_rpc_call(
                        &dump_on_ipc_failure,
                        request_id,
                        call_dep_id,
                        method_idx,
                        args_bytes,
                    );
                }
            }
        }
    }

    /// Sends a Hosted descriptor payload to this worker process and
    /// waits for the matching `HostedDescriptorAccepted` response.
    /// `dep_id` is the dep's fully-qualified id (`{crate}::{module}::{name}`).
    fn provide_hosted_descriptor(&mut self, dep_id: String, wire_bytes: Vec<u8>) {
        let dump_on_ipc_failure = self.dump_on_failure();
        let cmd = IpcCommand::ProvideHostedDescriptor {
            dep_id: dep_id.clone(),
            wire_bytes,
        };
        let msg = serialize_to_byte_vec(&cmd).expect("Failed to encode IPC command");
        dump_on_ipc_failure.run(write_frame(&mut self.connection, &msg));

        loop {
            let response_bytes = dump_on_ipc_failure.run(read_frame(&mut self.connection));
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes));
            match response {
                IpcResponse::HostedDescriptorAccepted { dep_id: ack_id } => {
                    if ack_id == dep_id {
                        return;
                    }
                }
                IpcResponse::CloneableAccepted { .. } => {
                    // Out-of-band ack from a previous ProvideCloneable; ignore.
                }
                IpcResponse::TestFinished { .. } => {
                    // Should not happen before any RunTest.
                }
                IpcResponse::HostedRpcCall {
                    request_id,
                    dep_id: call_dep_id,
                    method_idx,
                    args_bytes,
                } => {
                    self.handle_hosted_rpc_call(
                        &dump_on_ipc_failure,
                        request_id,
                        call_dep_id,
                        method_idx,
                        args_bytes,
                    );
                }
            }
        }
    }

    fn dump_on_failure(&self) -> DumpOnFailure {
        DumpOnFailure {
            out_lines: self.out_lines.clone(),
            err_lines: self.err_lines.clone(),
        }
    }

    fn drain_until(
        source: Arc<Mutex<VecDeque<CapturedOutput>>>,
        finish_marker: String,
    ) -> Vec<CapturedOutput> {
        let mut result = Vec::new();
        loop {
            let mut source = source.lock().unwrap();
            while let Some(line) = source.pop_front() {
                if line.line() == finish_marker {
                    return result;
                } else {
                    result.push(line.clone());
                }
            }
            drop(source);

            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

struct DumpOnFailure {
    out_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
    err_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
}

impl DumpOnFailure {
    pub fn run<T, E>(&self, result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(_error) => {
                let out_lines: Vec<_> = self.out_lines.lock().unwrap().drain(..).collect();
                let err_lines: Vec<_> = self.err_lines.lock().unwrap().drain(..).collect();
                let mut all_lines = [out_lines, err_lines].concat();
                all_lines.sort();

                for line in all_lines {
                    eprintln!("{}", line.line());
                }

                std::process::exit(1);
            }
        }
    }
}

fn spawn_worker_if_needed(args: &Arguments) -> Option<Worker> {
    if args.spawn_workers {
        let id = Uuid::new_v4();
        let name_str = format!("{id}.sock");
        let name = name_str
            .clone()
            .to_ns_name::<GenericNamespaced>()
            .expect("Invalid local socket name");
        let opts = ListenerOptions::new().name(name.clone());
        let listener = opts
            .create_sync()
            .expect("Failed to create local socket listener");

        let exe = std::env::current_exe().expect("Failed to get current executable path");

        let mut args = args.clone();
        args.ipc = Some(name_str);
        args.spawn_workers = false;
        args.logfile = None;
        let args = args.to_args();

        #[allow(clippy::zombie_processes)]
        let mut process = Command::new(exe)
            .args(args)
            .stdin(Stdio::inherit())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to spawn worker process");

        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let out_lines = Arc::new(Mutex::new(VecDeque::new()));
        let err_lines = Arc::new(Mutex::new(VecDeque::new()));
        let capture_enabled = Arc::new(Mutex::new(true));

        let out_lines_clone = out_lines.clone();
        let capture_enabled_clone = capture_enabled.clone();
        let out_handle = spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        //eprintln!("[WORKER OUT] {line}");
                        if *capture_enabled_clone.lock().unwrap() {
                            out_lines_clone
                                .lock()
                                .unwrap()
                                .push_back(CapturedOutput::stdout(line));
                        } else {
                            println!("{line}");
                        }
                    }
                    Err(error) => {
                        eprintln!("Failed to read from worker stdout: {error}");
                        return;
                    }
                }
            }
        });

        let err_lines_clone = err_lines.clone();
        let capture_enabled_clone = capture_enabled.clone();
        let err_handle = spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        //eprintln!("[WORKER ERR] {line}");
                        if *capture_enabled_clone.lock().unwrap() {
                            err_lines_clone
                                .lock()
                                .unwrap()
                                .push_back(CapturedOutput::stderr(line));
                        } else {
                            eprintln!("{line}");
                        }
                    }
                    Err(error) => {
                        eprintln!("Failed to read from worker stdout: {error}");
                        return;
                    }
                }
            }
        });

        let connection = listener.accept().expect("Failed to accept connection");

        Some(Worker {
            listener,
            process,
            out_handle,
            err_handle,
            out_lines,
            err_lines,
            capture_enabled,
            connection,
            hosted_rpc_owner_cells: Arc::new(HashMap::new()),
        })
    } else {
        None
    }
}
