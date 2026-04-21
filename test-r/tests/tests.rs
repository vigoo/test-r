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
}
