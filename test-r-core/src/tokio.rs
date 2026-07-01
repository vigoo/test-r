use crate::args::{Arguments, TimeThreshold};
use crate::bench::AsyncBencher;
use crate::execution::{DepWireBytes, TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests, get_ensure_time, CapturedOutput, CloneableCodec, FailureCause,
    FlakinessControl, HostedRpcChannel, HostedRpcError, HostedRpcOwnerCell, HostedRpcTransport,
    InProcessHostedRpcTransport, RegisteredTest, RpcFactory, SuiteResult, TestFunction, TestResult,
    WorkerReconstructor,
};
use crate::ipc::{
    ipc_name, read_frame_async, write_frame_async, HostedRpcReplyBody, IpcCommand, IpcResponse,
};
use crate::output::{test_runner_output, TestRunnerOutput};
use desert_rust::{deserialize, serialize_to_byte_vec};
use futures::FutureExt;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions};
use std::any::Any;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::process::{ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::spawn;
use tokio::sync::Mutex;
use tokio::task::{spawn_blocking, JoinHandle, JoinSet};
use tokio::time::Instant;
use uuid::Uuid;

pub fn test_runner() -> ExitCode {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_test_runner())
}

#[allow(clippy::await_holding_lock)]
async fn async_test_runner() -> ExitCode {
    crate::panic_hook::install_panic_hook();
    let mut args = Arguments::from_args();
    // When the parent spawned this process as a worker it passed
    // `--worker-index <N>`. Stash it so `crate::worker::worker_index()`
    // returns the correct value for PerWorker dep constructors.
    if let Some(idx) = args.worker_index {
        crate::worker::set_worker_index(idx);
    }
    // Host-side output capture is installed PER retry attempt below
    // (after `finalize_for_execution`), mirroring the sync runner.
    // See `crate::host_capture` for the pipeline.
    let output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let registered_dependency_constructors =
        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS.lock().unwrap();
    let registered_testsuite_props = internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap();
    let registered_test_generators = internal::REGISTERED_TEST_GENERATORS.lock().unwrap();

    let generated_tests = generate_tests(&registered_test_generators).await;

    let all_tests: Vec<RegisteredTest> = registered_tests
        .iter()
        .cloned()
        .chain(generated_tests)
        .collect();

    if args.list {
        // Apply suite properties (including runtime matrix-suite multiplication)
        // before listing, so matrix-multiplied cases appear in `--list` output
        // with their `<test>_<case>` names and `:tag:`-selectable auto-tags.
        let tests_with_props =
            internal::apply_suite_props_to_tests(&all_tests, &registered_testsuite_props);
        output.test_list(&tests_with_props);
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
            // Install host capture for this attempt, after
            // `finalize_for_execution` has decided whether workers
            // will spawn. See `sync::test_runner` for the rationale.
            let mut host_capture = crate::host_capture::install_if_needed(&args);
            let host_capture_epoch: Option<std::time::Instant> =
                host_capture.as_ref().map(|hc| hc.epoch());
            let host_capture_epoch_wall: Option<std::time::SystemTime> =
                host_capture.as_ref().map(|hc| hc.epoch_wall());
            let is_top_level_parent = args.is_top_level_parent();
            let has_selected_tests = execution.remaining() > 0;
            // Parent-side collection for dependency scopes whose worker-side
            // value is shipped as bytes or represented as an RPC stub. Async
            // constructors are awaited here, before workers receive their
            // reconstructed values. Skip it for empty filtered runs.
            let needs_parent_shared = execution.has_cloneable_dependencies()
                || execution.has_hosted_dependencies()
                || execution.has_hosted_rpc_dependencies();
            let parent_shared = if is_top_level_parent && has_selected_tests && needs_parent_shared
            {
                execution.collect_parent_shared_dependencies_async().await
            } else {
                crate::execution::ParentSharedDependencies {
                    cloneable_wire_bytes: Vec::new(),
                    cloneable_local_values: Vec::new(),
                    hosted_descriptor_bytes: Vec::new(),
                    hosted_owners: Vec::new(),
                    hosted_rpc_owner_cells: Vec::new(),
                    parent_constructed_shared_values: Vec::new(),
                }
            };
            let cloneable_wire_bytes = parent_shared.cloneable_wire_bytes;
            let cloneable_local_values = parent_shared.cloneable_local_values;
            let hosted_descriptor_bytes = parent_shared.hosted_descriptor_bytes;
            let _hosted_owners = parent_shared.hosted_owners;
            let hosted_rpc_owner_cells: HashMap<String, Arc<HostedRpcOwnerCell>> =
                parent_shared.hosted_rpc_owner_cells.into_iter().collect();
            let parent_constructed_shared_values = parent_shared.parent_constructed_shared_values;
            // Pre-built RpcFactory lookup keyed by qualified id, so worker
            // subprocesses can build stubs without re-locking the global
            // REGISTERED_DEPENDENCY_CONSTRUCTORS.
            let rpc_factories: HashMap<String, RpcFactory> = registered_dependency_constructors
                .iter()
                .filter_map(|d| {
                    if d.scope == crate::internal::DepScope::HostedRpc {
                        d.rpc_factory
                            .as_ref()
                            .map(|f| (d.qualified_id(), f.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            // Build a combined Cloneable + Hosted codec/worker lookup table
            // now, before `test_thread` workers are spawned (see sync.rs
            // for rationale). Keyed by the dep's fully-qualified id
            // (`{crate}::{module}::{name}`) so workers can route an incoming
            // `ProvideCloneable` / `ProvideHostedDescriptor` to the correct
            // dep even when two deps share a local `name` in different
            // modules.
            let cloneable_codecs: HashMap<String, (CloneableCodec, WorkerReconstructor)> =
                registered_dependency_constructors
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
                    .collect();
            // Mode-consistent Cloneable semantics for the no-spawn-workers
            // path (e.g. `--nocapture`): reuse the parent-constructed value
            // directly instead of re-running the user constructor in
            // `materialize_deps`. Without this, a Cloneable dep's
            // constructor would run twice (once for parent-side
            // `collect_parent_shared_dependencies_async`, once for the
            // in-process test execution), which both violates the
            // "constructor runs once" expectation and can deadlock when
            // the constructor takes a runtime-wide lock.
            if is_top_level_parent && !args.spawn_workers && !cloneable_local_values.is_empty() {
                apply_cloneable_values_locally(&mut execution, &cloneable_local_values);
            }
            // Mode-consistent Hosted semantics: when this is the top-level
            // parent AND we do NOT spawn workers (e.g. --nocapture), the
            // test functions run in this same process, but they must still
            // see the *worker-side handle* produced by
            // `HostedDep::from_descriptor`. Reconstruct each handle locally
            // via the descriptor round-trip and pre-populate the execution
            // tree.
            if is_top_level_parent && !args.spawn_workers && !hosted_descriptor_bytes.is_empty() {
                apply_hosted_descriptors_locally(
                    &mut execution,
                    &cloneable_codecs,
                    &hosted_descriptor_bytes,
                )
                .await;
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
            // Mirror of `sync::apply_parent_constructed_shared_values_locally`:
            // in no-spawn-workers mode, install any `Shared`/`PerWorker` dep
            // values the parent had to construct as transitive inputs to a
            // Cloneable/Hosted/HostedRpc dep. The in-process test thread's
            // `materialize_deps` then reuses them instead of re-running the
            // constructor in the same process.
            if is_top_level_parent
                && !args.spawn_workers
                && !parent_constructed_shared_values.is_empty()
            {
                apply_parent_constructed_shared_values_locally(
                    &mut execution,
                    &parent_constructed_shared_values,
                );
            }
            if args.spawn_workers {
                execution.skip_creating_dependencies();
            }

            // println!("Execution plan: {execution:?}");
            // println!("Final args: {args:?}");
            // println!("Has dependencies: {:?}", execution.has_dependencies());

            let count = execution.remaining();
            let results = Arc::new(Mutex::new(Vec::with_capacity(count)));
            // Parent-side per-test execution windows aligned 1:1 with
            // `results`. The host-capture finaliser uses them after all
            // test_threads finish to attribute spilled host-log records
            // to the test(s) whose window contains each record.
            let host_windows: Arc<Mutex<Vec<crate::host_capture::HostWindow>>> =
                Arc::new(Mutex::new(Vec::with_capacity(count)));

            let start = Instant::now();
            output.start_suite(&filtered_tests);

            let execution = Arc::new(Mutex::new(execution));
            let cloneable_wire_bytes = Arc::new(cloneable_wire_bytes);
            let hosted_descriptor_bytes = Arc::new(hosted_descriptor_bytes);
            let cloneable_codecs = Arc::new(cloneable_codecs);
            let rpc_factories = Arc::new(rpc_factories);
            let hosted_rpc_owner_cells = Arc::new(hosted_rpc_owner_cells);
            let mut join_set = JoinSet::new();
            let threads = args.test_threads().get();

            for worker_idx in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                // Stamp each test-thread's args with the worker index it will
                // hand to its spawned child via `--worker-index <N>`.
                let mut args_clone = args.clone();
                if args_clone.spawn_workers {
                    args_clone.worker_index = Some(worker_idx);
                }
                let results_clone = results.clone();
                let host_windows_clone = host_windows.clone();
                let wire_bytes_clone = cloneable_wire_bytes.clone();
                let hosted_bytes_clone = hosted_descriptor_bytes.clone();
                let codecs_clone = cloneable_codecs.clone();
                let rpc_factories_clone = rpc_factories.clone();
                let hosted_rpc_owner_cells_clone = hosted_rpc_owner_cells.clone();
                let handle = tokio::runtime::Handle::current();
                join_set.spawn_blocking(move || {
                    handle.block_on(test_thread(
                        args_clone,
                        execution_clone,
                        output_clone,
                        count,
                        results_clone,
                        host_windows_clone,
                        wire_bytes_clone,
                        hosted_bytes_clone,
                        codecs_clone,
                        rpc_factories_clone,
                        hosted_rpc_owner_cells_clone,
                        host_capture_epoch,
                    ))
                });
            }

            while let Some(res) = join_set.join_next().await {
                res.expect("Failed to join task");
            }

            drop(execution);

            let mut results = results.lock().await;
            // Drop parent-owned hosted / hosted-rpc owners BEFORE
            // finalising host capture so any `Drop` impls that
            // shutdown background threads / subprocesses get a chance
            // to emit their final lines through the still-active
            // capture pipe. If we restored fd 1/2 first, those late
            // lines would either land on the about-to-render
            // structured output or be swallowed entirely.
            drop(hosted_rpc_owner_cells);
            drop(_hosted_owners);

            // Finalise host capture (if any) BEFORE rendering the
            // suite so the attributed host-log records make it into
            // the per-test captured-output vecs the formatter walks.
            if let Some(hc) = host_capture.take() {
                let epoch_wall = host_capture_epoch_wall.unwrap_or_else(|| hc.epoch_wall());
                let records = hc.finalize();
                let windows = host_windows.lock().await;
                let windows_indexed: Vec<(usize, crate::host_capture::HostWindow)> =
                    windows.iter().copied().enumerate().collect();
                crate::host_capture::attribute_records_to_tests(
                    epoch_wall,
                    &records,
                    &windows_indexed,
                    &mut results,
                );
            }
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
async fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
    results: Arc<Mutex<Vec<(RegisteredTest, TestResult)>>>,
    host_windows: Arc<Mutex<Vec<crate::host_capture::HostWindow>>>,
    cloneable_wire_bytes: Arc<Vec<DepWireBytes>>,
    hosted_descriptor_bytes: Arc<Vec<DepWireBytes>>,
    cloneable_codecs: Arc<HashMap<String, (CloneableCodec, WorkerReconstructor)>>,
    rpc_factories: Arc<HashMap<String, RpcFactory>>,
    hosted_rpc_owner_cells: Arc<HashMap<String, Arc<HostedRpcOwnerCell>>>,
    host_capture_epoch: Option<std::time::Instant>,
) {
    let mut worker = spawn_worker_if_needed(&args).await;
    // Parent dispatches incoming `HostedRpcCall` frames against the owner
    // cells materialised in the top-level parent. Workers don't need the owner
    // cells (they own stubs instead), so they receive an empty map and the
    // dispatch code path is never reached in subprocesses.
    if let Some(worker) = worker.as_mut() {
        worker.set_hosted_rpc_owner_cells(hosted_rpc_owner_cells.clone());
    }
    let connection_arc = if let Some(ref name) = args.ipc {
        let name = ipc_name(name.clone());
        let stream = Stream::connect(name)
            .await
            .expect("Failed to connect to IPC socket");
        Some(Arc::new(Mutex::new(stream)))
    } else {
        None
    };

    if let Some(worker) = worker.as_mut() {
        for (dep_id, wire_bytes) in cloneable_wire_bytes.iter() {
            worker
                .provide_cloneable(dep_id.clone(), wire_bytes.clone())
                .await;
        }
        // Ship every Hosted dep's descriptor bytes too.
        for (dep_id, descriptor_bytes) in hosted_descriptor_bytes.iter() {
            worker
                .provide_hosted_descriptor(dep_id.clone(), descriptor_bytes.clone())
                .await;
        }
    }

    // Worker subprocess side: build a stub for every HostedRpc dep registered
    // in this binary using the IPC-backed transport sharing the same socket as
    // the main IPC loop. Install the stubs in the execution tree so dependency
    // materialisation skips the parent-only owner constructor.
    if let Some(connection) = connection_arc.as_ref() {
        if !rpc_factories.is_empty() {
            install_worker_subprocess_hosted_rpc_stubs(
                &execution,
                &rpc_factories,
                connection.clone(),
            )
            .await;
        }
    }

    let mut expected_test = None;

    while !is_done(&execution).await {
        if let Some(connection) = connection_arc.as_ref() {
            while expected_test.is_none() {
                let mut conn = connection.lock().await;
                let command_bytes = read_frame_async(&mut *conn)
                    .await
                    .expect("Failed to read IPC command frame");
                drop(conn);
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
                        // Worker-side reconstruction (see sync.rs::apply_provided_wire_bytes).
                        apply_provided_wire_bytes(
                            &execution,
                            &cloneable_codecs,
                            &dep_id,
                            &wire_bytes,
                            "ProvideCloneable",
                        )
                        .await;
                        let response = IpcResponse::CloneableAccepted { dep_id };
                        let msg = serialize_to_byte_vec(&response)
                            .expect("Failed to encode IPC response");
                        let mut conn = connection.lock().await;
                        write_frame_async(&mut *conn, &msg)
                            .await
                            .expect("Failed to write IPC response frame");
                    }
                    IpcCommand::ProvideHostedDescriptor { dep_id, wire_bytes } => {
                        // Worker-side reconstruction: same shape as
                        // ProvideCloneable but routed through the registered
                        // HostedDep worker_fn.
                        apply_provided_wire_bytes(
                            &execution,
                            &cloneable_codecs,
                            &dep_id,
                            &wire_bytes,
                            "ProvideHostedDescriptor",
                        )
                        .await;
                        let response = IpcResponse::HostedDescriptorAccepted { dep_id };
                        let msg = serialize_to_byte_vec(&response)
                            .expect("Failed to encode IPC response");
                        let mut conn = connection.lock().await;
                        write_frame_async(&mut *conn, &msg)
                            .await
                            .expect("Failed to write IPC response frame");
                    }
                    IpcCommand::HostedRpcReply { .. } => {
                        // HR1.2: replies for worker-initiated HostedRpc calls
                        // are consumed inline by the IPC transport during
                        // test execution, never by this between-tests
                        // command loop. Receiving one here means the
                        // protocol got out of sync; surface that loudly
                        // rather than dropping the frame.
                        panic!(
                            "unexpected `HostedRpcReply` while waiting for the next \
                             between-tests command in the tokio worker subprocess: a \
                             stub call must have left a reply on the wire without \
                             draining it inline"
                        );
                    }
                }
            }
        }

        if let Some(next) = pick_next(&execution).await {
            let skip = if let Some((name, crate_name, module_path)) = &expected_test {
                next.test.name != *name
                    || next.test.crate_name != *crate_name
                    || next.test.module_path != *module_path
            } else {
                false
            };

            if !skip {
                expected_test = None;

                let ensure_time = get_ensure_time(&args, &next.test);

                // Snapshot the parent's monotonic-clock view of the
                // test start. The matching end-instant is captured
                // after `finished_running_test`, and the pair becomes
                // a `HostWindow` for record attribution. Uses
                // `std::time::Instant` because the host-capture epoch
                // is `std::time::Instant` (tokio's `Instant` is a
                // different type without `From` interop).
                let window_start = std::time::Instant::now();

                output.start_running_test(&next.test, next.index, count);
                let result = run_test(
                    output.clone(),
                    next.index,
                    count,
                    args.nocapture,
                    args.include_ignored,
                    ensure_time,
                    next.deps.clone(),
                    &next.test,
                    &mut worker,
                )
                .await;
                output.finished_running_test(&next.test, next.index, count, &result);
                let window_end = std::time::Instant::now();

                if let Some(connection) = connection_arc.as_ref() {
                    let finish_marker = Uuid::new_v4().to_string();
                    let finish_marker_line = format!("{finish_marker}\n");
                    tokio::io::stdout()
                        .write_all(finish_marker_line.as_bytes())
                        .await
                        .unwrap();
                    tokio::io::stderr()
                        .write_all(finish_marker_line.as_bytes())
                        .await
                        .unwrap();
                    tokio::io::stdout().flush().await.unwrap();
                    tokio::io::stderr().flush().await.unwrap();

                    let response = IpcResponse::TestFinished {
                        result: (&result).into(),
                        finish_marker,
                    };
                    let msg =
                        serialize_to_byte_vec(&response).expect("Failed to encode IPC response");
                    let mut conn = connection.lock().await;
                    write_frame_async(&mut *conn, &msg)
                        .await
                        .expect("Failed to write IPC response frame");
                }

                // Push the result and its window under the same
                // critical section so the two vecs stay aligned in the
                // face of concurrent pushes from sibling test_threads.
                let window = crate::host_capture::HostWindow::from_instants(
                    host_capture_epoch,
                    window_start,
                    window_end,
                )
                .unwrap_or(crate::host_capture::HostWindow {
                    start: std::time::Duration::ZERO,
                    end: std::time::Duration::ZERO,
                });
                let mut results_guard = results.lock().await;
                let mut windows_guard = host_windows.lock().await;
                results_guard.push((next.test.clone(), result));
                windows_guard.push(window);
            }
        }
    }
}

async fn is_done(execution: &Arc<Mutex<TestSuiteExecution>>) -> bool {
    let execution = execution.lock().await;
    execution.is_done()
}

/// Async counterpart to `sync::apply_provided_wire_bytes`. Decodes the wire
/// bytes into a worker-side dependency value (looked up by the dep's
/// fully-qualified id `{crate}::{module}::{name}`) and stores it in the
/// execution tree so the next `materialize_deps` call uses the pre-resolved
/// value.
///
/// `source_command` is the textual name of the IPC command that delivered
/// the bytes (`"ProvideCloneable"` or `"ProvideHostedDescriptor"`); used
/// only in panic messages.
async fn apply_provided_wire_bytes(
    execution: &Arc<Mutex<TestSuiteExecution>>,
    wire_codecs: &HashMap<String, (CloneableCodec, WorkerReconstructor)>,
    dep_id: &str,
    wire_bytes: &[u8],
    source_command: &str,
) {
    let (codec, worker_fn) = wire_codecs.get(dep_id).unwrap_or_else(|| {
        panic!("{source_command} referenced unknown wire-shipped dep '{dep_id}'")
    });

    let wire_payload = (codec.from_wire_bytes)(wire_bytes);
    let empty_deps: Arc<dyn internal::DependencyView + Send + Sync> =
        Arc::new(HashMap::<String, Arc<dyn Any + Send + Sync>>::new());
    let reconstructed = match worker_fn {
        WorkerReconstructor::Sync(f) => f(wire_payload, empty_deps),
        WorkerReconstructor::Async(f) => f(wire_payload, empty_deps).await,
    };

    let mut execution = execution.lock().await;
    let applied = execution.provide_cloneable_value(dep_id, reconstructed);
    assert!(
        applied,
        "{source_command} for dep '{dep_id}' did not match any registered dep in this worker"
    );
}

/// Mode-consistent Cloneable semantics for the no-spawn-workers path on
/// the tokio runner. Mirrors `sync::apply_cloneable_values_locally`: takes
/// the parent-constructed Cloneable values and installs them directly
/// into the parent's `TestSuiteExecution`, so `materialize_deps` reuses
/// them instead of re-running the user constructor.
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

/// Tokio counterpart to `sync::apply_parent_constructed_shared_values_locally`.
/// In no-spawn-workers mode, installs `Shared`/`PerWorker` dep values that
/// the parent had to construct as transitive inputs to a
/// Cloneable/Hosted/HostedRpc dep, so the in-process test thread reuses
/// them instead of re-running the constructor in the same process.
fn apply_parent_constructed_shared_values_locally(
    execution: &mut TestSuiteExecution,
    values: &[(String, Arc<dyn Any + Send + Sync>)],
) {
    for (dep_id, value) in values {
        let applied = execution.provide_materialized_shared_value(dep_id, value.clone());
        assert!(
            applied,
            "Shared/PerWorker dep '{dep_id}' could not be pre-populated locally"
        );
    }
}

/// Mode-consistent Hosted semantics for the no-spawn-workers path on the
/// tokio runner. Mirrors `sync::apply_hosted_descriptors_locally`: takes
/// the parent-collected descriptor bytes and reconstructs each Hosted
/// dep's worker-side handle (via the registered codec + worker_fn)
/// directly in the parent's `TestSuiteExecution`.
///
/// Both `WorkerReconstructor::Sync` (`HostedDep::from_descriptor`) and
/// `WorkerReconstructor::Async` (`AsyncHostedDep::from_descriptor`) are
/// supported here so that `async_worker` Hosted deps see the same
/// worker-side handle whether the runner ended up in spawned-worker mode
/// or in the no-spawn fallback, matching the documented mode-consistent
/// Hosted contract in `book/src/advanced_features/dependency_sharing.md`.
async fn apply_hosted_descriptors_locally(
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
            WorkerReconstructor::Async(f) => f(wire_payload, empty_deps).await,
        };
        let applied = execution.provide_cloneable_value(dep_id, reconstructed);
        assert!(
            applied,
            "Hosted dep '{dep_id}' could not be pre-populated locally"
        );
    }
}

async fn pick_next(execution: &Arc<Mutex<TestSuiteExecution>>) -> Option<TestExecution> {
    let mut execution = execution.lock().await;
    execution.pick_next().await
}

async fn run_with_flakiness_control<F>(
    output: Arc<dyn TestRunnerOutput>,
    test_description: &RegisteredTest,
    idx: usize,
    count: usize,
    test: F,
) -> Result<Result<(), FailureCause>, Box<dyn Any + Send>>
where
    F: Fn(
            Instant,
        )
            -> Pin<Box<dyn Future<Output = Result<Result<(), FailureCause>, Box<dyn Any + Send>>>>>
        + Send
        + Sync,
{
    match &test_description.props.flakiness_control {
        FlakinessControl::None => {
            let start = Instant::now();
            test(start).await
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
                match test(start).await {
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
                let result = test(start).await;

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

#[allow(clippy::too_many_arguments)]
async fn run_test(
    output: Arc<dyn TestRunnerOutput>,
    idx: usize,
    count: usize,
    nocapture: bool,
    include_ignored: bool,
    ensure_time: Option<TimeThreshold>,
    dependency_view: Arc<dyn internal::DependencyView + Send + Sync>,
    test: &RegisteredTest,
    worker: &mut Option<Worker>,
) -> TestResult {
    if test.props.is_ignored && !include_ignored {
        TestResult::ignored()
    } else if let Some(worker) = worker.as_mut() {
        worker.run_test(nocapture, test).await
    } else {
        let start = Instant::now();
        let test = test.clone();
        match &test.run {
            TestFunction::Sync(_) => {
                let handle = spawn_blocking(move || {
                    let test = test.clone();
                    crate::sync::run_sync_test_function(
                        output,
                        &test,
                        idx,
                        count,
                        ensure_time,
                        dependency_view,
                    )
                });
                handle.await.unwrap_or_else(|join_error| {
                    TestResult::failed(
                        start.elapsed(),
                        FailureCause::HarnessError(format!(
                            "Failed joining test task: {join_error}"
                        )),
                    )
                })
            }
            TestFunction::Async(test_fn) => {
                let timeout = test.props.timeout;
                let test_fn = test_fn.clone();
                let detached_panic_policy = test.props.detached_panic_policy.clone();
                let result = run_with_flakiness_control(output, &test, idx, count, |start| {
                    let dependency_view = dependency_view.clone();
                    let test_fn = test_fn.clone();
                    Box::pin(async move {
                        let test_id = crate::panic_hook::next_test_id();
                        crate::panic_hook::set_current_test_id(test_id);
                        crate::panic_hook::create_detached_collector(test_id);
                        let result = AssertUnwindSafe(Box::pin(async move {
                            match timeout {
                                None => test_fn(dependency_view).await,
                                Some(duration) => {
                                    let result =
                                        tokio::time::timeout(duration, test_fn(dependency_view))
                                            .await;
                                    match result {
                                        Ok(result) => result,
                                        Err(_) => {
                                            return Err(FailureCause::HarnessError(
                                                "Test timed out".to_string(),
                                            ))
                                        }
                                    }
                                }
                            }
                            .into_result()?;
                            if let Some(ensure_time) = ensure_time {
                                let elapsed = start.elapsed();
                                if ensure_time.is_critical(&elapsed) {
                                    return Err(FailureCause::HarnessError(format!(
                                        "Test run time exceeds critical threshold: {elapsed:?}"
                                    )));
                                }
                            }
                            Ok(())
                        }))
                        .catch_unwind()
                        .await;
                        result
                    })
                })
                .await;
                let mut test_result =
                    TestResult::from_result(&test.props.should_panic, start.elapsed(), result);
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
            TestFunction::SyncBench(_) => {
                let handle = spawn_blocking(move || {
                    let test = test.clone();
                    crate::sync::run_sync_test_function(
                        output,
                        &test,
                        idx,
                        count,
                        ensure_time,
                        dependency_view,
                    )
                });
                handle.await.unwrap_or_else(|join_error| {
                    TestResult::failed(
                        start.elapsed(),
                        FailureCause::HarnessError(format!(
                            "Failed joining test task: {join_error}"
                        )),
                    )
                })
            }
            TestFunction::AsyncBench(bench_fn) => {
                let mut bencher = AsyncBencher::new();
                let test_id = crate::panic_hook::next_test_id();
                crate::panic_hook::set_current_test_id(test_id);
                let result = AssertUnwindSafe(async move {
                    bench_fn(&mut bencher, dependency_view).await;
                    (
                        bencher
                            .summary()
                            .expect("iter() was not called in bench function"),
                        bencher.bytes,
                    )
                })
                .catch_unwind()
                .await;
                let bytes = result.as_ref().map(|(_, bytes)| *bytes).unwrap_or_default();
                let test_result = TestResult::from_summary(
                    &test.props.should_panic,
                    start.elapsed(),
                    result.map(|(summary, _)| summary),
                    bytes,
                );
                crate::panic_hook::clear_current_test_id();
                test_result
            }
        }
    }
}

struct Worker {
    _listener: Listener,
    _process: Child,
    _out_handle: JoinHandle<()>,
    _err_handle: JoinHandle<()>,
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
    /// `IpcCommand::HostedRpcReply` back to the worker subprocess. Mirrors
    /// `sync::Worker::handle_hosted_rpc_call`.
    async fn handle_hosted_rpc_call(
        &mut self,
        dump_on_ipc_failure: &DumpOnFailure,
        request_id: u64,
        dep_id: String,
        method_idx: u32,
        args_bytes: Vec<u8>,
    ) {
        let body = match self.hosted_rpc_owner_cells.get(&dep_id) {
            // Use the async dispatch entry point so an owner that implements
            // `AsyncHostedRpcDep` directly can `.await` inside its dispatcher
            // without blocking the tokio runtime. Sync owners reach this
            // entry point through the blanket bridge and their dispatched
            // future resolves immediately.
            Some(cell) => match cell.dispatch_async(method_idx, &args_bytes).await {
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
        dump_on_ipc_failure
            .run(write_frame_async(&mut self.connection, &msg).await)
            .await;
    }

    pub async fn run_test(&mut self, nocapture: bool, test: &RegisteredTest) -> TestResult {
        let mut capture_enabled = self.capture_enabled.lock().await;
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
        dump_on_ipc_failure
            .run(write_frame_async(&mut self.connection, &msg).await)
            .await;

        let response = loop {
            let response_bytes = dump_on_ipc_failure
                .run(read_frame_async(&mut self.connection).await)
                .await;
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes)).await;
            match response {
                IpcResponse::TestFinished { .. } => break response,
                IpcResponse::CloneableAccepted { .. }
                | IpcResponse::HostedDescriptorAccepted { .. } => continue,
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
                    )
                    .await;
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
                Self::drain_until(self.out_lines.clone(), finish_marker.clone()).await;
            let err_lines: Vec<_> =
                Self::drain_until(self.err_lines.clone(), finish_marker.clone()).await;
            result.into_test_result(out_lines, err_lines)
        } else {
            result.into_test_result(Vec::new(), Vec::new())
        }
    }

    /// Async counterpart to `sync::Worker::provide_cloneable`. `dep_id` is the
    /// dep's fully-qualified id (`{crate}::{module}::{name}`).
    async fn provide_cloneable(&mut self, dep_id: String, wire_bytes: Vec<u8>) {
        let dump_on_ipc_failure = self.dump_on_failure();
        let cmd = IpcCommand::ProvideCloneable {
            dep_id: dep_id.clone(),
            wire_bytes,
        };
        let msg = serialize_to_byte_vec(&cmd).expect("Failed to encode IPC command");
        dump_on_ipc_failure
            .run(write_frame_async(&mut self.connection, &msg).await)
            .await;

        loop {
            let response_bytes = dump_on_ipc_failure
                .run(read_frame_async(&mut self.connection).await)
                .await;
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes)).await;
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
                    dep_id: rpc_dep_id,
                    method_idx,
                    args_bytes,
                } => {
                    // A worker subprocess can issue a HostedRpc call from
                    // inside an in-progress test, even while the parent is
                    // mid-`ProvideCloneable` for a different dep. Dispatch it
                    // so the protocol doesn't desync.
                    self.handle_hosted_rpc_call(
                        &dump_on_ipc_failure,
                        request_id,
                        rpc_dep_id,
                        method_idx,
                        args_bytes,
                    )
                    .await;
                }
            }
        }
    }

    /// Async counterpart to `sync::Worker::provide_hosted_descriptor`.
    async fn provide_hosted_descriptor(&mut self, dep_id: String, wire_bytes: Vec<u8>) {
        let dump_on_ipc_failure = self.dump_on_failure();
        let cmd = IpcCommand::ProvideHostedDescriptor {
            dep_id: dep_id.clone(),
            wire_bytes,
        };
        let msg = serialize_to_byte_vec(&cmd).expect("Failed to encode IPC command");
        dump_on_ipc_failure
            .run(write_frame_async(&mut self.connection, &msg).await)
            .await;

        loop {
            let response_bytes = dump_on_ipc_failure
                .run(read_frame_async(&mut self.connection).await)
                .await;
            let response: IpcResponse = dump_on_ipc_failure.run(deserialize(&response_bytes)).await;
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
                    dep_id: rpc_dep_id,
                    method_idx,
                    args_bytes,
                } => {
                    // See provide_cloneable arm. Dispatch the call inline so
                    // the IPC stream stays in sync.
                    self.handle_hosted_rpc_call(
                        &dump_on_ipc_failure,
                        request_id,
                        rpc_dep_id,
                        method_idx,
                        args_bytes,
                    )
                    .await;
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

    async fn drain_until(
        source: Arc<Mutex<VecDeque<CapturedOutput>>>,
        finish_marker: String,
    ) -> Vec<CapturedOutput> {
        let mut result = Vec::new();
        loop {
            let mut source = source.lock().await;
            while let Some(line) = source.pop_front() {
                if line.line() == finish_marker {
                    return result;
                } else {
                    result.push(line.clone());
                }
            }
            drop(source);

            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

struct DumpOnFailure {
    out_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
    err_lines: Arc<Mutex<VecDeque<CapturedOutput>>>,
}

impl DumpOnFailure {
    pub async fn run<T, E>(&self, result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(_error) => {
                let out_lines: Vec<_> = self.out_lines.lock().await.drain(..).collect();
                let err_lines: Vec<_> = self.err_lines.lock().await.drain(..).collect();
                let mut all_lines = [out_lines, err_lines].concat();
                all_lines.sort();

                // Route the diagnostic through the real terminal even
                // when host capture has fd 2 redirected into its pipe.
                // We `process::exit(1)` immediately after this and
                // never reach `host_capture.finalize()`, so anything we
                // wrote into the host pipe would be lost. The capture
                // pipe and its reader are abandoned on exit; that is
                // acceptable for this fatal-IPC path.
                use std::io::Write;
                let mut err = crate::host_capture::TerminalStderr;
                for line in all_lines {
                    let _ = writeln!(err, "{}", line.line());
                }
                let _ = err.flush();

                std::process::exit(1);
            }
        }
    }
}

async fn spawn_worker_if_needed(args: &Arguments) -> Option<Worker> {
    if args.spawn_workers {
        let id = Uuid::new_v4();
        let name_str = format!("{id}.sock");
        let name = name_str
            .clone()
            .to_ns_name::<GenericNamespaced>()
            .expect("Invalid local socket name");
        let opts = ListenerOptions::new().name(name.clone());
        let listener = opts
            .create_tokio()
            .expect("Failed to create local socket listener");

        let exe = std::env::current_exe().expect("Failed to get current executable path");

        let mut args = args.clone();
        args.ipc = Some(name_str);
        args.spawn_workers = false;
        args.logfile = None;
        let args = args.to_args();

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
        let out_handle = spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Some(line) = lines
                .next_line()
                .await
                .expect("Failed to read from worker stdout")
            {
                if *capture_enabled_clone.lock().await {
                    out_lines_clone
                        .lock()
                        .await
                        .push_back(CapturedOutput::stdout(line));
                } else {
                    // `#[never_capture]` pass-through: write to the real
                    // terminal even when host capture has redirected
                    // fd 1 into its pipe, so the worker line stays
                    // uncaptured live output and is not later
                    // re-labelled `[host]`.
                    use std::io::Write;
                    let mut out = crate::host_capture::TerminalStdout;
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
            }
        });

        let err_lines_clone = err_lines.clone();
        let capture_enabled_clone = capture_enabled.clone();
        let err_handle = spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Some(line) = lines
                .next_line()
                .await
                .expect("Failed to read from worker stderr")
            {
                if *capture_enabled_clone.lock().await {
                    err_lines_clone
                        .lock()
                        .await
                        .push_back(CapturedOutput::stderr(line));
                } else {
                    // Same as the stdout pass-through above: route the
                    // never-captured worker line to the real terminal
                    // stderr, not the host capture pipe.
                    use std::io::Write;
                    let mut err = crate::host_capture::TerminalStderr;
                    let _ = writeln!(err, "{line}");
                    let _ = err.flush();
                }
            }
        });

        let connection = listener
            .accept()
            .await
            .expect("Failed to accept connection");

        Some(Worker {
            _listener: listener,
            _process: process,
            _out_handle: out_handle,
            _err_handle: err_handle,
            out_lines,
            err_lines,
            connection,
            capture_enabled,
            hosted_rpc_owner_cells: Arc::new(HashMap::new()),
        })
    } else {
        None
    }
}

/// Parent-side `--nocapture` / no-spawn-workers helper. Builds one stub per
/// HostedRpc dep using an [`InProcessHostedRpcTransport`] that points at the
/// parent-held owner cells, and stashes it in the execution tree so dependency
/// materialisation skips the owner-only constructor. Mirrors
/// `sync::install_local_hosted_rpc_stubs`.
fn install_local_hosted_rpc_stubs(
    execution: &mut TestSuiteExecution,
    rpc_factories: &HashMap<String, RpcFactory>,
    owner_cells: &HashMap<String, Arc<HostedRpcOwnerCell>>,
) {
    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(InProcessHostedRpcTransport::new(owner_cells.clone()));
    for (dep_id, factory) in rpc_factories.iter() {
        if !owner_cells.contains_key(dep_id) {
            // No owner cell materialised for this dep (e.g. registered
            // globally but not pulled into the current filter). Skip so
            // we don't install a stub nothing routes.
            continue;
        }
        let channel = HostedRpcChannel::new(dep_id.clone(), transport.clone());
        let stub = (factory.build_stub)(channel);
        let applied = execution.provide_cloneable_value(dep_id, stub);
        if !applied {
            // The owner cell can be materialised solely because another
            // parent-side Cloneable/Hosted/HostedRpc dependency needs this
            // HostedRpc dep as a constructor input. In that case the stub is
            // intentionally not present in the worker execution tree; there is
            // nothing to pre-populate for no-spawn test execution.
            continue;
        }
    }
}

/// Worker subprocess helper. Builds one stub per registered HostedRpc dep backed
/// by [`IpcHostedRpcTransport`], and installs it in the execution tree. Mirrors
/// `sync::install_worker_subprocess_hosted_rpc_stubs` but runs on the tokio
/// `Arc<Mutex<Stream>>` connection.
async fn install_worker_subprocess_hosted_rpc_stubs(
    execution: &Arc<Mutex<TestSuiteExecution>>,
    rpc_factories: &HashMap<String, RpcFactory>,
    connection_arc: Arc<Mutex<Stream>>,
) {
    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(IpcHostedRpcTransport::new(connection_arc));
    for (dep_id, factory) in rpc_factories.iter() {
        let channel = HostedRpcChannel::new(dep_id.clone(), transport.clone());
        let stub = (factory.build_stub)(channel);
        let mut execution = execution.lock().await;
        let applied = execution.provide_cloneable_value(dep_id, stub);
        // Not every binary registers a HostedRpc dep that the current
        // execution actually uses; if so, just move on.
        let _ = applied;
    }
}

/// Worker subprocess `HostedRpcTransport` for the tokio runner.
/// Mirrors `sync::IpcHostedRpcTransport` but bridges a sync trait method
/// to the async tokio IPC primitives via
/// `tokio::task::block_in_place` + `Handle::current().block_on(...)`.
///
/// The shared `Arc<Mutex<Stream>>` is the same one used by the
/// worker subprocess's main IPC loop; the lock guarantees that a stub
/// call and the main loop never interleave a half-written frame. Calls
/// serialise across all in-flight stubs by acquiring the mutex for the
/// full request-then-reply round trip.
///
/// This relies on the temporal invariant documented on
/// [`crate::internal::HostedRpcChannel::call`]: stubs are only invoked
/// from inside a running test body, never from `build_stub`, and never
/// from detached background work that outlives the test. Under those
/// rules the main IPC loop is only reading between tests, so it cannot
/// race with a stub call.
struct IpcHostedRpcTransport {
    connection: Arc<Mutex<Stream>>,
    next_request_id: AtomicU64,
}

impl IpcHostedRpcTransport {
    fn new(connection: Arc<Mutex<Stream>>) -> Self {
        Self {
            connection,
            next_request_id: AtomicU64::new(0),
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

        let connection = self.connection.clone();
        let handle = tokio::runtime::Handle::current();

        // Run the async I/O round-trip from inside this sync trait
        // method. `block_in_place` releases this worker thread back to
        // the scheduler so the parent's read loop can continue making
        // progress on other tasks while we wait for the reply.
        tokio::task::block_in_place(move || {
            handle.block_on(async move {
                let mut conn = connection.lock().await;
                write_frame_async(&mut *conn, &msg).await.map_err(|e| {
                    HostedRpcError::Transport(format!("write HostedRpcCall failed: {e:?}"))
                })?;
                let reply_bytes = read_frame_async(&mut *conn).await.map_err(|e| {
                    HostedRpcError::Transport(format!("read HostedRpcReply failed: {e:?}"))
                })?;
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
                            HostedRpcReplyBody::Err { message } => {
                                Err(HostedRpcError::Dispatch(message))
                            }
                        }
                    }
                    other => Err(HostedRpcError::Transport(format!(
                        "unexpected IpcCommand while waiting for HostedRpcReply: {other:?}"
                    ))),
                }
            })
        })
    }
}
