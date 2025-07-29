use crate::args::{Arguments, TimeThreshold};
use crate::bench::AsyncBencher;
use crate::execution::{TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests, get_ensure_time, CapturedOutput, FlakinessControl, RegisteredTest, SuiteResult,
    TestFunction, TestResult,
};
use crate::ipc::{ipc_name, IpcCommand, IpcResponse};
use crate::output::{test_runner_output, TestRunnerOutput};
use bincode::{decode_from_slice, encode_to_vec};
use futures::FutureExt;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions};
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
            let mut join_set = JoinSet::new();
            let threads = args.test_threads().get();

            for _ in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                let args_clone = args.clone();
                let results_clone = results.clone();
                let handle = tokio::runtime::Handle::current();
                join_set.spawn_blocking(move || {
                    handle.block_on(test_thread(
                        args_clone,
                        execution_clone,
                        output_clone,
                        count,
                        results_clone,
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

async fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
    results: Arc<Mutex<Vec<(RegisteredTest, TestResult)>>>,
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

    let mut expected_test = None;

    while !is_done(&execution).await {
        if let Some(connection) = &mut connection {
            if expected_test.is_none() {
                let mut command_size: [u8; 2] = [0, 0];
                connection
                    .read_exact(&mut command_size)
                    .await
                    .expect("Failed to read IPC command size");
                let mut command = vec![0; u16::from_le_bytes(command_size) as usize];
                connection
                    .read_exact(&mut command)
                    .await
                    .expect("Failed to read IPC command");
                let (command, _): (IpcCommand, usize) =
                    decode_from_slice(&command, bincode::config::standard())
                        .expect("Failed to decode IPC command");

                let IpcCommand::RunTest {
                    name,
                    crate_name,
                    module_path,
                } = command;
                expected_test = Some((name, crate_name, module_path));
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
                    let msg = encode_to_vec(&response, bincode::config::standard())
                        .expect("Failed to encode IPC response");
                    let message_size = (msg.len() as u16).to_le_bytes();
                    connection
                        .write_all(&message_size)
                        .await
                        .expect("Failed to write IPC response message size");
                    connection
                        .write_all(&msg)
                        .await
                        .expect("Failed to write response to IPC connection");
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

async fn pick_next(execution: &Arc<Mutex<TestSuiteExecution>>) -> Option<TestExecution> {
    let mut execution = execution.lock().await;
    execution.pick_next().await
}

async fn run_with_flakiness_control<F, R>(
    output: Arc<dyn TestRunnerOutput>,
    test_description: &RegisteredTest,
    idx: usize,
    count: usize,
    test: F,
) -> Result<(), R>
where
    F: Fn(Instant) -> Pin<Box<dyn Future<Output = Result<(), R>>>> + Send + Sync,
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
                test(start).await?;
            }
            Ok(())
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
                    TestResult::failed(start.elapsed(), Box::new(join_error))
                })
            }
            TestFunction::Async(test_fn) => {
                let timeout = test.props.timeout;
                let test_fn = test_fn.clone();
                let result = run_with_flakiness_control(output, &test, idx, count, |start| {
                    let dependency_view = dependency_view.clone();
                    let test_fn = test_fn.clone();
                    Box::pin(async move {
                        AssertUnwindSafe(Box::pin(async move {
                            let result = match timeout {
                                None => test_fn(dependency_view).await,
                                Some(duration) => {
                                    let result =
                                        tokio::time::timeout(duration, test_fn(dependency_view))
                                            .await;
                                    match result {
                                        Ok(result) => result,
                                        Err(_) => panic!("Test timed out"),
                                    }
                                }
                            };
                            match result.as_result() {
                                Ok(_) => (),
                                Err(message) => panic!("{message}"),
                            };
                            if let Some(ensure_time) = ensure_time {
                                let elapsed = start.elapsed();
                                if ensure_time.is_critical(&elapsed) {
                                    panic!(
                                        "Test run time exceeds critical threshold: {elapsed:?}"
                                    );
                                }
                            }
                        }))
                        .catch_unwind()
                        .await
                    })
                })
                .await;
                TestResult::from_result(&test.props.should_panic, start.elapsed(), result)
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
                    TestResult::failed(start.elapsed(), Box::new(join_error))
                })
            }
            TestFunction::AsyncBench(bench_fn) => {
                let mut bencher = AsyncBencher::new();
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
                TestResult::from_summary(
                    &test.props.should_panic,
                    start.elapsed(),
                    result.map(|(summary, _)| summary),
                    bytes,
                )
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

        let msg =
            encode_to_vec(&cmd, bincode::config::standard()).expect("Failed to encode IPC command");
        let message_size = (msg.len() as u16).to_le_bytes();
        dump_on_ipc_failure
            .run(self.connection.write_all(&message_size).await)
            .await;
        dump_on_ipc_failure
            .run(self.connection.write_all(&msg).await)
            .await;

        let mut response_size: [u8; 2] = [0, 0];
        dump_on_ipc_failure
            .run(self.connection.read_exact(&mut response_size).await)
            .await;
        let mut response = vec![0; u16::from_le_bytes(response_size) as usize];
        dump_on_ipc_failure
            .run(self.connection.read_exact(&mut response).await)
            .await;
        let (response, _): (IpcResponse, usize) = dump_on_ipc_failure
            .run(decode_from_slice(&response, bincode::config::standard()))
            .await;

        let IpcResponse::TestFinished {
            result,
            finish_marker,
        } = response;

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
