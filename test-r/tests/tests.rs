use serial_test::serial;
use std::time::{Duration, Instant};

mod cargo_tests {
    use super::*;

    #[test]
    #[serial]
    fn can_run_sync_examples() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("--skip")
            .arg("other::tests::result_based_test_err")
            .arg("--skip")
            .arg("tests::it_does_work")
            .current_dir(root)
            .status()
            .unwrap();

        assert_eq!(process.code(), Some(0));
    }

    #[test]
    #[serial]
    fn can_run_async_examples() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("--skip")
            .arg("inner::tests::sleeping_test_3_timeout")
            .arg("--skip")
            .arg("inner::tests::sleeping_test_3_timeout_hr")
            .arg("--skip")
            .arg("tests::it_does_work")
            .arg("--skip")
            .arg("tests::panic_test_2b")
            .arg("--skip")
            .arg("tests::result_based_test_err")
            .arg("--skip")
            .arg("suite_timeout_tests::suite_timeout_exceeds")
            .arg("--skip")
            .arg("suite_timeout_macro_tests::suite_timeout_macro_exceeds")
            .current_dir(root)
            .status()
            .unwrap();

        assert_eq!(process.code(), Some(0));
    }

    #[test]
    #[serial]
    fn exit_code_is_101_on_failure() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("tests::it_does_work")
            .current_dir(root)
            .status()
            .unwrap();

        assert_eq!(process.code(), Some(101));
    }

    #[test]
    #[serial]
    fn async_output_capturing_works() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:output_capture_test")
            .arg("--")
            .arg("--format")
            .arg("json")
            .arg("--show-output")
            .current_dir(root)
            .output()
            .unwrap();

        let mut output_it_does_work = "".to_string();
        let mut output_this_too = "".to_string();
        let mut output_panic_test_1 = "".to_string();

        let output = String::from_utf8(process.stdout).unwrap();
        for line in output.lines() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                let event = map.get("event").unwrap().as_str().unwrap();
                if (event == "ok" || event == "failed")
                    && let Some(serde_json::Value::String(s)) = map.get("name")
                {
                    if s == "test_r_example_tokio::tests::it_does_work" {
                        output_it_does_work =
                            map.get("stdout").unwrap().as_str().unwrap().to_string();
                    } else if s == "test_r_example_tokio::tests::this_too" {
                        output_this_too = map.get("stdout").unwrap().as_str().unwrap().to_string();
                    } else if s == "test_r_example_tokio::tests::panic_test_1" {
                        output_panic_test_1 =
                            map.get("stdout").unwrap().as_str().unwrap().to_string();
                    }
                }
            }
        }

        assert!(output_it_does_work.contains("Print from 'it_does_work'\n"));
        assert!(output_it_does_work.contains("Stderr from 'it_does_work'\n"));
        assert!(output_this_too.contains("Print from 'this_too'"));
        assert!(output_this_too.contains("Stderr from 'this_too'"));
        assert!(output_panic_test_1.contains("Print from 'panic_test_1'"));
        assert!(output_panic_test_1.contains("Stderr from 'panic_test_1'"));
    }

    #[test]
    #[serial]
    fn sync_output_capturing_works() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:output_capture_test")
            .arg("--")
            .arg("--format")
            .arg("json")
            .arg("--show-output")
            .current_dir(root)
            .output()
            .unwrap();

        let mut output_it_does_work = "".to_string();
        let mut output_this_too = "".to_string();
        let mut output_inner_test_works = "".to_string();

        let output = String::from_utf8(process.stdout).unwrap();
        for line in output.lines() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                let event = map.get("event").unwrap().as_str().unwrap();
                if (event == "ok" || event == "failed")
                    && let Some(serde_json::Value::String(s)) = map.get("name")
                {
                    if s == "test_r_example::tests::it_does_work" {
                        output_it_does_work =
                            map.get("stdout").unwrap().as_str().unwrap().to_string();
                    } else if s == "test_r_example::tests::this_too" {
                        output_this_too = map.get("stdout").unwrap().as_str().unwrap().to_string();
                    } else if s == "test_r_example::inner::tests::inner_test_works" {
                        output_inner_test_works =
                            map.get("stdout").unwrap().as_str().unwrap().to_string();
                    }
                }
            }
        }

        assert!(output_it_does_work.contains("Print from 'it_does_work'\n"));
        assert!(output_it_does_work.contains("Stderr from 'it_does_work'\n"));
        assert!(output_this_too.contains("Print from 'this_too'"));
        assert!(output_this_too.contains("Stderr from 'this_too'"));
        assert!(output_inner_test_works.contains("Print from inner test"));
        assert!(output_inner_test_works.contains("Stderr from inner test"));
    }

    #[test]
    #[serial]
    fn nested_module_can_be_executed_by_suite_tag() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:a")
            .arg("--")
            .arg("--format")
            .arg("json")
            .arg("--show-output")
            .current_dir(root)
            .output()
            .unwrap();

        let output = String::from_utf8(process.stdout).unwrap();

        let mut found = false;
        for line in output.lines() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                let event = map.get("event").unwrap().as_str().unwrap();
                if event == "ok"
                    && let Some(serde_json::Value::String(name)) = map.get("name")
                    && name == "test_r_example::other::tests::nested::nested_module_test_works"
                {
                    found = true;
                    break;
                }
            }
        }
        assert!(found)
    }

    #[test]
    #[serial]
    fn nested_module_can_be_executed_by_standalone_suite_tag() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:b")
            .arg("--")
            .arg("--format")
            .arg("json")
            .arg("--show-output")
            .current_dir(root)
            .output()
            .unwrap();

        let output = String::from_utf8(process.stdout).unwrap();
        let mut found = false;
        for line in output.lines() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                let event = map.get("event").unwrap().as_str().unwrap();
                if event == "ok"
                    && let Some(serde_json::Value::String(name)) = map.get("name")
                    && name == "test_r_example::other::tests::nested::nested_module_test_works"
                {
                    found = true;
                    break;
                }
            }
        }
        assert!(found)
    }

    #[test]
    #[serial]
    fn nested_module_can_be_executed_by_nested_suite_tag() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:c")
            .arg("--")
            .arg("--format")
            .arg("json")
            .arg("--show-output")
            .current_dir(root)
            .output()
            .unwrap();

        let output = String::from_utf8(process.stdout).unwrap();
        let mut found = false;
        for line in output.lines() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                let event = map.get("event").unwrap().as_str().unwrap();
                if event == "ok"
                    && let Some(serde_json::Value::String(name)) = map.get("name")
                    && name == "test_r_example::other::tests::nested::nested_module_test_works"
                {
                    found = true;
                    break;
                }
            }
        }
        assert!(found)
    }
}

mod timing_tests {
    use super::*;

    #[test]
    #[serial]
    fn timeout_works() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let start = Instant::now();
        let _process = std::process::Command::new("cargo")
            .arg("test")
            .arg("inner::tests::sleeping_test_3_timeout")
            .arg("--")
            .arg("--exact")
            .current_dir(root)
            .status()
            .unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed < Duration::from_secs(15));
    }

    #[test]
    #[serial]
    fn suite_timeout_attribute_works() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let start = Instant::now();
        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("suite_timeout_tests::suite_timeout_exceeds")
            .arg("--")
            .arg("--exact")
            .current_dir(&root)
            .status()
            .unwrap();
        let elapsed = start.elapsed();

        // The test should fail due to timeout, and it should complete quickly (not wait 30s)
        assert_ne!(process.code(), Some(0));
        assert!(elapsed < Duration::from_secs(15));

        // A short test in the same suite should pass
        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("suite_timeout_tests::suite_timeout_short_test")
            .arg("--")
            .arg("--exact")
            .current_dir(&root)
            .status()
            .unwrap();
        assert_eq!(process.code(), Some(0));

        // A test with its own per-test timeout should use the per-test timeout (override)
        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("suite_timeout_tests::suite_timeout_overridden")
            .arg("--")
            .arg("--exact")
            .current_dir(&root)
            .status()
            .unwrap();
        assert_eq!(process.code(), Some(0));
    }

    #[test]
    #[serial]
    fn suite_timeout_macro_works() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let start = Instant::now();
        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("suite_timeout_macro_tests::suite_timeout_macro_exceeds")
            .arg("--")
            .arg("--exact")
            .current_dir(&root)
            .status()
            .unwrap();
        let elapsed = start.elapsed();

        assert_ne!(process.code(), Some(0));
        assert!(elapsed < Duration::from_secs(15));

        // A short test in the same suite should pass
        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("suite_timeout_macro_tests::suite_timeout_macro_short_test")
            .arg("--")
            .arg("--exact")
            .current_dir(&root)
            .status()
            .unwrap();
        assert_eq!(process.code(), Some(0));
    }
}

mod lazy_dep_pruning_tests {
    use super::*;

    fn run_example_test(filter: &str) -> String {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(filter)
            .arg("--")
            .arg("--exact")
            .arg("--nocapture")
            .current_dir(root)
            .output()
            .unwrap();

        assert_eq!(
            process.status.code(),
            Some(0),
            "Test failed: {}",
            String::from_utf8_lossy(&process.stderr)
        );

        let stdout = String::from_utf8(process.stdout).unwrap();
        let stderr = String::from_utf8(process.stderr).unwrap();
        format!("{stdout}{stderr}")
    }

    #[test]
    #[serial]
    fn unused_deps_are_not_created() {
        let output = run_example_test("lazy_dep_pruning::test_uses_dep_a");

        assert!(
            output.contains("LAZY_DEPS_MARKER: Creating DepA"),
            "DepA should be created"
        );
        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepB"),
            "DepB should NOT be created"
        );
        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepC"),
            "DepC should NOT be created"
        );
    }

    #[test]
    #[serial]
    fn no_deps_created_when_test_uses_none() {
        let output = run_example_test("lazy_dep_pruning::test_uses_none");

        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepA"),
            "DepA should NOT be created"
        );
        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepB"),
            "DepB should NOT be created"
        );
        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepC"),
            "DepC should NOT be created"
        );
    }

    #[test]
    #[serial]
    fn transitive_deps_are_kept() {
        let output = run_example_test("lazy_dep_pruning::test_uses_dep_c");

        assert!(
            output.contains("LAZY_DEPS_MARKER: Creating DepA"),
            "DepA should be created (transitive dep of DepC)"
        );
        assert!(
            !output.contains("LAZY_DEPS_MARKER: Creating DepB"),
            "DepB should NOT be created"
        );
        assert!(
            output.contains("LAZY_DEPS_MARKER: Creating DepC"),
            "DepC should be created"
        );
    }

    #[test]
    #[serial]
    fn all_deps_created_when_both_used() {
        let output = run_example_test("lazy_dep_pruning::test_uses_both");

        assert!(
            output.contains("LAZY_DEPS_MARKER: Creating DepA"),
            "DepA should be created"
        );
        assert!(
            output.contains("LAZY_DEPS_MARKER: Creating DepB"),
            "DepB should be created"
        );
    }

    /// `matrix_suite!` multiplies tests at runtime (Strategy B). Verify via
    /// `--list` that each `&DbDep`-taking test in `matrix_suite_example`
    /// appears once per case with the `<test>_<case>` naming, that the
    /// non-`DbDep`-taking `no_dep_test` is NOT multiplied, and that the
    /// `:tag:db_postgres` / `:tag:db_sqlite` selectors resolve to the right
    /// subsets.
    #[test]
    #[serial]
    fn matrix_suite_list_multiplies_tests() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("--list")
            .current_dir(&root)
            .output()
            .unwrap();

        // `--list` always exits 0 regardless of whether some tests would fail.
        assert_eq!(
            process.status.code(),
            Some(0),
            "cargo test -- --list should exit 0; stderr: {}",
            String::from_utf8_lossy(&process.stderr)
        );

        let stdout = String::from_utf8_lossy(&process.stdout);

        // Each DbDep-taking test is multiplied into a postgres + sqlite case.
        for needle in [
            "matrix_features_e2e::matrix_suite_example::thing_one_postgres",
            "matrix_features_e2e::matrix_suite_example::thing_one_sqlite",
            "matrix_features_e2e::matrix_suite_example::thing_two_postgres",
            "matrix_features_e2e::matrix_suite_example::thing_two_sqlite",
        ] {
            assert!(
                stdout.contains(needle),
                "`--list` should contain `{needle}`, got:\n{stdout}"
            );
        }

        // The non-DbDep test runs exactly once — it must NOT be multiplied.
        assert!(
            stdout.contains("matrix_features_e2e::matrix_suite_example::no_dep_test"),
            "`--list` should contain the unmultiplied `no_dep_test`, got:\n{stdout}"
        );
        assert!(
            !stdout.contains("matrix_features_e2e::matrix_suite_example::no_dep_test_postgres"),
            "`no_dep_test` must not be multiplied, but a `_postgres` variant appeared:\n{stdout}"
        );
        assert!(
            !stdout.contains("matrix_features_e2e::matrix_suite_example::no_dep_test_sqlite"),
            "`no_dep_test` must not be multiplied, but a `_sqlite` variant appeared:\n{stdout}"
        );
    }

    /// Running the example with `:tag:db_sqlite` selects exactly the
    /// `_sqlite`-suffixed matrix cases (plus any other sqlite-tagged tests)
    /// and they all pass.
    #[test]
    #[serial]
    fn matrix_suite_tag_selector_runs_only_sqlite_cases() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg(":tag:db_sqlite")
            .current_dir(&root)
            .status()
            .unwrap();

        assert_eq!(
            process.code(),
            Some(0),
            "selecting :tag:db_sqlite should run only sqlite matrix cases and pass"
        );
    }
}

mod nocapture_no_spawn_workers_tests {
    use super::*;

    /// Regression: when the tokio runner runs in `--nocapture` mode (which
    /// forces `spawn_workers = false`), every `Cloneable` dep constructor
    /// must run exactly once end-to-end. Historically the runner would
    /// compute the Cloneable wire bytes in
    /// `collect_parent_shared_dependencies_async` (running the constructor
    /// once), then discard those bytes — so `materialize_deps` re-ran the
    /// constructor a second time inside the execution tree.
    ///
    /// The example-tokio fixture
    /// `sharing::cloneable_no_double_init::tests::cloneable_no_double_init_test`
    /// prints a unique marker every time its Cloneable constructor runs.
    /// We invoke that fixture under `--nocapture` and grep-count the marker
    /// on stdout: it must appear exactly once.
    /// End-to-end regression for the parent-side host-capture path
    /// (see `test-r-core/src/host_capture.rs`). The fixture in
    /// `example/src/sharing/host_capture_demo.rs` defines a
    /// `HostedRpc` dep whose owner:
    ///   * spawns a background thread that `println!`s
    ///     `HOST_BG_THREAD_TICK` every 20ms, and
    ///   * `println!`s `HOST_DISPATCH_HIT` from inside `dispatch`.
    ///
    /// Both lines originate in the **parent** process (not in any
    /// worker subprocess), so before host capture they were either
    /// silently dropped by `cargo test`'s outer capture or, worse,
    /// corrupting the structured `--format=json`/`junit` output.
    ///
    /// We run the fixture with `--show-output` (pretty mode) so the
    /// per-test captured output is printed. The host-capture pipeline
    /// must:
    ///   1. collect lines emitted in the parent during the test's
    ///      window;
    ///   2. surface them tagged `[host]` inside the test's
    ///      `---- … stdout/err ----` block.
    ///
    /// Both markers must appear at least once.
    #[test]
    #[serial]
    fn host_capture_surfaces_parent_side_output_under_pretty_format() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("sharing::host_capture_demo::tests::host_capture_demo_emits_both_markers")
            .arg("--")
            .arg("--exact")
            .arg("--show-output")
            .current_dir(&root)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&process.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&process.stderr).into_owned();

        assert_eq!(
            process.status.code(),
            Some(0),
            "fixture test must pass; stderr:\n{stderr}\nstdout:\n{stdout}"
        );

        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("[host] HOST_DISPATCH_HIT"),
            "host-capture must surface the dispatcher print as `[host] HOST_DISPATCH_HIT` \
             under the test's captured output block.\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            combined.contains("[host] HOST_BG_THREAD_TICK"),
            "host-capture must surface the background-thread print as \
             `[host] HOST_BG_THREAD_TICK` under the test's captured output block.\n\
             stdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    #[test]
    #[serial]
    fn cloneable_constructor_runs_once_under_nocapture() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.parent().unwrap().join("example-tokio");

        let process = std::process::Command::new("cargo")
            .arg("test")
            .arg("sharing::cloneable_no_double_init::tests::cloneable_no_double_init_test")
            .arg("--")
            .arg("--exact")
            .arg("--nocapture")
            .current_dir(&root)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&process.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&process.stderr).into_owned();

        assert_eq!(
            process.status.code(),
            Some(0),
            "fixture test must pass under --nocapture; stderr:\n{stderr}\nstdout:\n{stdout}"
        );

        let marker = "CLONEABLE_NO_DOUBLE_INIT_MARKER: build_probe()";
        let combined = format!("{stdout}{stderr}");
        let marker_count = combined.matches(marker).count();

        assert_eq!(
            marker_count, 1,
            "Cloneable constructor must run exactly once in --nocapture mode, \
             but the marker `{marker}` appeared {marker_count} time(s) on the \
             test runner's output.\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
