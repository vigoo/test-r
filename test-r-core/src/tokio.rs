use crate::args::{Arguments, TimeThreshold};
use crate::bench::AsyncBencher;
use crate::execution::{DepWireBytes, HostedOwner, TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests, get_ensure_time, CapturedOutput, CloneableCodec, FailureCause,
    FlakinessControl, RegisteredTest, SuiteResult, TestFunction, TestResult, WorkerReconstructor,
};
use crate::ipc::{ipc_name, read_frame_async, write_frame_async, IpcCommand, IpcResponse};
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
            // Phase 1A.5: materialise Cloneable deps in the parent and
            // ship the wire bytes to each worker via IPC (see sync.rs for
            // the equivalent code path). We use the async collector here so
            // `async fn` Cloneable constructors are awaited on the parent.
            // Only the top-level parent (no `--ipc` set) does this; IPC
            // worker subprocesses wait for `ProvideCloneable` from the
            // parent and never run the original constructor.
            let is_top_level_parent = args.is_top_level_parent();
            let cloneable_wire_bytes: Vec<DepWireBytes> =
                if is_top_level_parent && args.spawn_workers {
                    execution.collect_cloneable_wire_bytes_async().await
                } else {
                    Vec::new()
                };
            // Phase 1B: only the top-level parent materialises Hosted
            // owners and keeps them alive in `_hosted_owners` for the
            // entire suite (see sync.rs for the rationale). IPC worker
            // subprocesses must NOT run Hosted constructors: they would
            // duplicate singleton resources (TCP listeners, containers, …)
            // and could even hang worker startup if the constructor
            // panics before we accept the parent's IPC connection.
            let (hosted_descriptor_bytes, _hosted_owners): (Vec<DepWireBytes>, Vec<HostedOwner>) =
                if is_top_level_parent && execution.has_hosted_dependencies() {
                    execution.collect_hosted_descriptor_bytes_async().await
                } else {
                    (Vec::new(), Vec::new())
                };
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

            let start = Instant::now();
            output.start_suite(&filtered_tests);

            let execution = Arc::new(Mutex::new(execution));
            let cloneable_wire_bytes = Arc::new(cloneable_wire_bytes);
            let hosted_descriptor_bytes = Arc::new(hosted_descriptor_bytes);
            let cloneable_codecs = Arc::new(cloneable_codecs);
            let mut join_set = JoinSet::new();
            let threads = args.test_threads().get();

            for _ in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                let args_clone = args.clone();
                let results_clone = results.clone();
                let wire_bytes_clone = cloneable_wire_bytes.clone();
                let hosted_bytes_clone = hosted_descriptor_bytes.clone();
                let codecs_clone = cloneable_codecs.clone();
                let handle = tokio::runtime::Handle::current();
                join_set.spawn_blocking(move || {
                    handle.block_on(test_thread(
                        args_clone,
                        execution_clone,
                        output_clone,
                        count,
                        results_clone,
                        wire_bytes_clone,
                        hosted_bytes_clone,
                        codecs_clone,
                    ))
                });
            }

            while let Some(res) = join_set.join_next().await {
                res.expect("Failed to join task");
            }

            drop(execution);

            let results = results.lock().await;
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
    cloneable_wire_bytes: Arc<Vec<DepWireBytes>>,
    hosted_descriptor_bytes: Arc<Vec<DepWireBytes>>,
    cloneable_codecs: Arc<HashMap<String, (CloneableCodec, WorkerReconstructor)>>,
) {
    let mut worker = spawn_worker_if_needed(&args).await;
    let mut connection = if let Some(ref name) = args.ipc {
        let name = ipc_name(name.clone());
        let stream = Stream::connect(name)
            .await
            .expect("Failed to connect to IPC socket");
        Some(stream)
    } else {
        None
    };

    if let Some(worker) = worker.as_mut() {
        for (dep_id, wire_bytes) in cloneable_wire_bytes.iter() {
            worker
                .provide_cloneable(dep_id.clone(), wire_bytes.clone())
                .await;
        }
        // Phase 1B: ship every Hosted dep's descriptor bytes too.
        for (dep_id, descriptor_bytes) in hosted_descriptor_bytes.iter() {
            worker
                .provide_hosted_descriptor(dep_id.clone(), descriptor_bytes.clone())
                .await;
        }
    }

    let mut expected_test = None;

    while !is_done(&execution).await {
        if let Some(connection) = &mut connection {
            while expected_test.is_none() {
                let command_bytes = read_frame_async(connection)
                    .await
                    .expect("Failed to read IPC command frame");
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
                        write_frame_async(connection, &msg)
                            .await
                            .expect("Failed to write IPC response frame");
                    }
                    IpcCommand::ProvideHostedDescriptor { dep_id, wire_bytes } => {
                        // Phase 1B worker-side reconstruction: same shape as
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
                        write_frame_async(connection, &msg)
                            .await
                            .expect("Failed to write IPC response frame");
                    }
                    IpcCommand::HostedRpcReply { .. } => {
                        // Phase 1C: HostedRpc is sync-runner only in MVP.
                        // The tokio worker should never see a reply because
                        // it never originates a HostedRpcCall in this MVP.
                        panic!(
                            "HostedRpc is not supported by the tokio runner in the Phase 1C MVP"
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

                if let Some(connection) = &mut connection {
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
                    write_frame_async(connection, &msg)
                        .await
                        .expect("Failed to write IPC response frame");
                }

                results.lock().await.push((next.test.clone(), result));
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

/// Mode-consistent Hosted semantics for the no-spawn-workers path on the
/// tokio runner. Mirrors `sync::apply_hosted_descriptors_locally`: takes
/// the parent-collected descriptor bytes and reconstructs each Hosted
/// dep's worker-side handle (via the registered codec + worker_fn)
/// directly in the parent's `TestSuiteExecution`. The worker_fn for the
/// Hosted scope is always `Sync` (`HostedDep::from_descriptor` returns
/// `Self`, not a future), so we don't need a runtime await here.
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
                    "Async WorkerReconstructor for Hosted dep '{dep_id}' is not supported (HostedDep::from_descriptor is sync)"
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
}

impl Worker {
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
                IpcResponse::HostedRpcCall { .. } => {
                    panic!("HostedRpc is not supported by the tokio runner in the Phase 1C MVP");
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
                IpcResponse::HostedRpcCall { .. } => {
                    panic!("HostedRpc is not supported by the tokio runner in the Phase 1C MVP");
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
                IpcResponse::HostedRpcCall { .. } => {
                    panic!("HostedRpc is not supported by the tokio runner in the Phase 1C MVP");
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

                for line in all_lines {
                    eprintln!("{}", line.line());
                }

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
                    println!("{line}");
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
                    eprintln!("{line}");
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
        })
    } else {
        None
    }
}
