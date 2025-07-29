use crate::args::{Arguments, TimeThreshold};
use crate::bench::Bencher;
use crate::execution::{TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests_sync, get_ensure_time, CapturedOutput, FlakinessControl, RegisteredTest,
    SuiteResult, TestFunction, TestResult,
};
use crate::ipc::{ipc_name, IpcCommand, IpcResponse};
use crate::output::{test_runner_output, TestRunnerOutput};
use bincode::{decode_from_slice, encode_to_vec};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use std::time::Instant;
use uuid::Uuid;

pub fn test_runner() -> ExitCode {
    let mut args = Arguments::from_args();
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
            let threads = args.test_threads().get();
            let mut handles = Vec::with_capacity(threads);
            for _ in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                let args_clone = args.clone();
                handles.push(spawn(move || {
                    test_thread(args_clone, execution_clone, output_clone, count)
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

fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
) -> Vec<(RegisteredTest, TestResult)> {
    let mut worker = spawn_worker_if_needed(&args);
    let mut connection = if let Some(ref name) = args.ipc {
        let name = ipc_name(name.clone());
        let stream = Stream::connect(name).expect("Failed to connect to IPC socket");
        Some(stream)
    } else {
        None
    };

    let mut results = Vec::with_capacity(count);
    let mut expected_test = None;

    while !is_done(&execution) {
        if let Some(connection) = &mut connection {
            if expected_test.is_none() {
                let mut command_size: [u8; 2] = [0, 0];
                connection
                    .read_exact(&mut command_size)
                    .expect("Failed to read IPC command size");
                let mut command = vec![0; u16::from_le_bytes(command_size) as usize];
                connection
                    .read_exact(&mut command)
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

                if let Some(connection) = &mut connection {
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

                    let msg = encode_to_vec(&response, bincode::config::standard())
                        .expect("Failed to encode IPC response");
                    let message_size = (msg.len() as u16).to_le_bytes();
                    connection
                        .write_all(&message_size)
                        .expect("Failed to write IPC response message size");
                    connection
                        .write_all(&msg)
                        .expect("Failed to write response to IPC connection");
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

fn pick_next(execution: &Arc<Mutex<TestSuiteExecution>>) -> Option<TestExecution> {
    let mut execution = execution.lock().unwrap();
    execution.pick_next_sync()
}

fn run_with_flakiness_control<R>(
    output: Arc<dyn TestRunnerOutput>,
    test_description: &RegisteredTest,
    idx: usize,
    count: usize,
    test: impl Fn(Instant) -> Result<(), R>,
) -> Result<(), R> {
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
                test(start)?;
            }
            Ok(())
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
            let result =
                run_with_flakiness_control(output, test_description, idx, count, move |start| {
                    let dependency_view = dependency_view.clone();
                    let test_fn = test_fn.clone();
                    catch_unwind(AssertUnwindSafe(move || {
                        let result = test_fn(dependency_view).as_result();
                        if let Err(failure) = result {
                            panic!("{failure}");
                        }
                        if let Some(ensure_time) = ensure_time {
                            let elapsed = start.elapsed();
                            if ensure_time.is_critical(&elapsed) {
                                panic!("Test run time exceeds critical threshold: {elapsed:?}");
                            }
                        }
                    }))
                });
            TestResult::from_result(
                &test_description.props.should_panic,
                start.elapsed(),
                result,
            )
        }
        TestFunction::SyncBench(bench_fn) => {
            let mut bencher = Bencher::new();
            let result = catch_unwind(AssertUnwindSafe(|| {
                bench_fn(&mut bencher, dependency_view);
                bencher
                    .summary()
                    .expect("iter() was not called in bench function")
            }));
            TestResult::from_summary(
                &test_description.props.should_panic,
                start.elapsed(),
                result,
                bencher.bytes,
            )
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
}

impl Worker {
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

        let msg =
            encode_to_vec(&cmd, bincode::config::standard()).expect("Failed to encode IPC command");
        let message_size = (msg.len() as u16).to_le_bytes();
        dump_on_ipc_failure.run(self.connection.write_all(&message_size));
        dump_on_ipc_failure.run(self.connection.write_all(&msg));

        let mut response_size: [u8; 2] = [0, 0];
        dump_on_ipc_failure.run(self.connection.read_exact(&mut response_size));
        let mut response = vec![0; u16::from_le_bytes(response_size) as usize];
        dump_on_ipc_failure.run(self.connection.read_exact(&mut response));
        let (response, _): (IpcResponse, usize) =
            dump_on_ipc_failure.run(decode_from_slice(&response, bincode::config::standard()));

        let IpcResponse::TestFinished {
            result,
            finish_marker,
        } = response;

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
        })
    } else {
        None
    }
}
