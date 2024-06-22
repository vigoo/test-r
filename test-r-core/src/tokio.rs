use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use tokio::task::spawn_blocking;

use crate::internal::{filter_registered_tests, RegisteredTest, TestFunction};
use crate::output::test_runner_output;
use crate::{args, internal};

pub fn test_runner() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_test_runner());
}

async fn async_test_runner() {
    let args = args::Arguments::from_args();
    let mut output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let filtered = filter_registered_tests(&args, &registered_tests);
    let count = filtered.len();
    let mut results = Vec::with_capacity(count);

    if args.list {
        output.test_list(&*registered_tests);
    } else {
        output.start_suite(count);

        for (idx, registered_test) in filtered.into_iter().enumerate() {
            output.start_running_test(registered_test, idx, count);
            let result = run_test(args.include_ignored, registered_test).await;
            output.finished_running_test(registered_test, idx, count, &result);

            results.push((registered_test, result));
        }

        output.finished_suite(&*registered_tests, &results);
    }
}

async fn run_test(include_ignored: bool, test: &RegisteredTest) -> internal::TestResult {
    if test.is_ignored && !include_ignored {
        internal::TestResult::Ignored
    } else {
        match &test.run {
            TestFunction::Sync(_) => {
                let test_fn = test.run.clone();
                let handle = spawn_blocking(move || crate::sync::run_sync_test_function(&test_fn));
                handle
                    .await
                    .unwrap_or_else(|join_error| internal::TestResult::Failed {
                        panic: Box::new(join_error),
                    })
            }
            TestFunction::Async(test_fn) => {
                match AssertUnwindSafe(test_fn()).catch_unwind().await {
                    Ok(_) => internal::TestResult::Passed,
                    Err(panic) => internal::TestResult::Failed { panic },
                }
            }
        }
    }
}
