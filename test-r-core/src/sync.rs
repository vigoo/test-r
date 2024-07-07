use crate::args;
use crate::args::Arguments;
use crate::execution::TestSuiteExecution;
use crate::internal;
use crate::internal::{DependencyView, RegisteredTest, TestFunction, TestResult};
use crate::output::{test_runner_output, TestRunnerOutput};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

pub fn test_runner() {
    let args = args::Arguments::from_args();
    let output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let registered_dependency_constructors =
        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS.lock().unwrap();
    let registered_testsuite_props = internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap();

    if args.list {
        output.test_list(&registered_tests);
    } else {
        let execution = TestSuiteExecution::construct(
            &args,
            registered_dependency_constructors.as_slice(),
            registered_tests.as_slice(),
            registered_testsuite_props.as_slice(),
        );
        // println!("Execution plan: {execution:?}");

        let count = execution.remaining();
        let mut results = Vec::with_capacity(count);

        std::thread::scope(|s| {
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

            output.finished_suite(&registered_tests, &results);
        });
    }
}

fn test_thread(
    args: Arguments,
    execution: Arc<Mutex<TestSuiteExecution>>,
    output: Arc<dyn TestRunnerOutput>,
    count: usize,
) -> Vec<(RegisteredTest, TestResult)> {
    let mut results = Vec::with_capacity(count);
    while let Some((registered_test, deps, idx)) = pick_next(&execution) {
        output.start_running_test(&registered_test, idx, count);
        let result = run_test(args.include_ignored, deps, &registered_test);
        output.finished_running_test(&registered_test, idx, count, &result);

        results.push((registered_test.clone(), result));
    }
    results
}

fn pick_next<'a>(
    execution: &Arc<Mutex<TestSuiteExecution<'a>>>,
) -> Option<(
    &'a RegisteredTest,
    Box<dyn DependencyView + Send + Sync>,
    usize,
)> {
    let mut execution = execution.lock().unwrap();
    execution.pick_next_sync()
}

fn run_test(
    include_ignored: bool,
    dependency_view: Box<dyn internal::DependencyView + Send + Sync>,
    test: &RegisteredTest,
) -> internal::TestResult {
    if test.is_ignored && !include_ignored {
        internal::TestResult::Ignored
    } else {
        let test_fn = &test.run;
        run_sync_test_function(test_fn, dependency_view)
    }
}

#[allow(unreachable_patterns)]
pub(crate) fn run_sync_test_function(
    test_fn: &TestFunction,
    dependency_view: Box<dyn internal::DependencyView + Send + Sync>,
) -> internal::TestResult {
    let result = catch_unwind(AssertUnwindSafe(move || match test_fn {
        TestFunction::Sync(test_fn) => test_fn(dependency_view),
        _ => {
            panic!("Async tests are not supported in sync mode, enable the 'tokio' feature")
        }
    }));
    match result {
        Ok(_) => internal::TestResult::Passed,
        Err(panic) => internal::TestResult::Failed { panic },
    }
}
