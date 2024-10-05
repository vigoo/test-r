use crate::args::Arguments;
use crate::bench::Bencher;
use crate::execution::{TestExecution, TestSuiteExecution};
use crate::internal;
use crate::internal::{
    generate_tests_sync, CapturedOutput, RegisteredTest, ShouldPanic, TestFunction, TestResult,
};
use crate::ipc::{ipc_name, IpcCommand, IpcResponse};
use crate::output::{test_runner_output, TestRunnerOutput};
use bincode::{decode_from_slice, encode_to_vec};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use std::time::Instant;
use uuid::Uuid;

pub fn test_runner() {
    let mut args = Arguments::from_args();
    let output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let registered_dependency_constructors =
        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS.lock().unwrap();
    let registered_testsuite_props = internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap();
    let registered_test_generators = internal::REGISTERED_TEST_GENERATORS.lock().unwrap();

    let generated_tests = generate_tests_sync(&registered_test_generators);

    let all_tests: Vec<&RegisteredTest> = registered_tests
        .iter()
        .chain(generated_tests.as_slice())
        .collect();

    if args.list {
        output.test_list(&all_tests);
    } else {
        let mut execution = TestSuiteExecution::construct(
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

        std::thread::scope(|s| {
            let start = Instant::now();
            output.start_suite(count);

            let execution = Arc::new(Mutex::new(execution));
            let threads = args.test_threads().get();
            let mut handles = Vec::with_capacity(threads);
            for _ in 0..threads {
                let execution_clone = execution.clone();
                let output_clone = output.clone();
                let args_clone = args.clone();
                handles.push(
                    s.spawn(move || test_thread(args_clone, execution_clone, output_clone, count)),
                );
            }

            for handle in handles {
                results.extend(handle.join().unwrap());
            }

            output.finished_suite(&all_tests, &results, start.elapsed());
        });
    }
}

fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
) -> Vec<(RegisteredTest, TestResult)> {
    let mut worker = spawn_worker_if_needed(&args);
    let mut connection = if let Some(name) = args.ipc {
        let name = ipc_name(name);
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

                output.start_running_test(next.test, next.index, count);

                let result = if next.test.is_ignored && !args.include_ignored {
                    TestResult::Ignored {
                        captured: Vec::new(),
                    }
                } else if let Some(worker) = worker.as_mut() {
                    worker.run_test(next.test)
                } else {
                    run_sync_test_function(&next.test.should_panic, &next.test.run, next.deps)
                };

                output.finished_running_test(next.test, next.index, count, &result);

                if let Some(connection) = &mut connection {
                    let response = IpcResponse::TestFinished {
                        result: (&result).into(),
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

fn is_done(execution: &Arc<Mutex<TestSuiteExecution<'_>>>) -> bool {
    let execution = execution.lock().unwrap();
    execution.is_done()
}

fn pick_next<'a>(execution: &Arc<Mutex<TestSuiteExecution<'a>>>) -> Option<TestExecution<'a>> {
    let mut execution = execution.lock().unwrap();
    execution.pick_next_sync()
}

#[allow(unreachable_patterns)]
pub(crate) fn run_sync_test_function(
    should_panic: &ShouldPanic,
    test_fn: &TestFunction,
    dependency_view: Box<dyn internal::DependencyView + Send + Sync>,
) -> TestResult {
    let start = Instant::now();
    match test_fn {
        TestFunction::Sync(test_fn) => {
            let result = catch_unwind(AssertUnwindSafe(move || test_fn(dependency_view)));
            TestResult::from_result(should_panic, start.elapsed(), result)
        }
        TestFunction::SyncBench(bench_fn) => {
            let mut bencher = Bencher::new();
            let result = catch_unwind(AssertUnwindSafe(|| {
                bench_fn(&mut bencher, dependency_view);
                bencher
                    .summary()
                    .expect("iter() was not called in bench function")
            }));
            TestResult::from_summary(should_panic, start.elapsed(), result, bencher.bytes)
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
    out_lines: Arc<Mutex<Vec<CapturedOutput>>>,
    err_lines: Arc<Mutex<Vec<CapturedOutput>>>,
    connection: Stream,
}

impl Worker {
    pub fn run_test(&mut self, test: &RegisteredTest) -> TestResult {
        // Send IPC command and wait for IPC response, and in the meantime read from the stdout/stderr channels
        let cmd = IpcCommand::RunTest {
            name: test.name.clone(),
            crate_name: test.crate_name.clone(),
            module_path: test.module_path.clone(),
        };

        let msg =
            encode_to_vec(&cmd, bincode::config::standard()).expect("Failed to encode IPC command");
        let message_size = (msg.len() as u16).to_le_bytes();
        self.connection
            .write_all(&message_size)
            .expect("Failed to write IPC message size");
        self.connection
            .write_all(&msg)
            .expect("Failed to write to IPC connection");

        let mut response_size: [u8; 2] = [0, 0];
        self.connection
            .read_exact(&mut response_size)
            .expect("Failed to read IPC response size");
        let mut response = vec![0; u16::from_le_bytes(response_size) as usize];
        self.connection
            .read_exact(&mut response)
            .expect("Failed to read IPC response");
        let (response, _): (IpcResponse, usize) =
            decode_from_slice(&response, bincode::config::standard())
                .expect("Failed to decode IPC response");

        let IpcResponse::TestFinished { result } = response;

        let out_lines: Vec<_> = self.out_lines.lock().unwrap().drain(..).collect();
        let err_lines: Vec<_> = self.err_lines.lock().unwrap().drain(..).collect();
        result.into_test_result(out_lines, err_lines)
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

        let mut process = Command::new(exe)
            .args(args)
            .stdin(Stdio::inherit())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to spawn worker process");

        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let out_lines = Arc::new(Mutex::new(Vec::new()));
        let err_lines = Arc::new(Mutex::new(Vec::new()));

        let out_lines_clone = out_lines.clone();
        let out_handle = spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        //eprintln!("[WORKER OUT] {line}");
                        out_lines_clone
                            .lock()
                            .unwrap()
                            .push(CapturedOutput::stdout(line));
                    }
                    Err(error) => {
                        eprintln!("Failed to read from worker stdout: {error}");
                        return;
                    }
                }
            }
        });

        let err_lines_clone = err_lines.clone();
        let err_handle = spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        //eprintln!("[WORKER ERR] {line}");
                        err_lines_clone
                            .lock()
                            .unwrap()
                            .push(CapturedOutput::stderr(line));
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
            connection,
        })
    } else {
        None
    }
}
