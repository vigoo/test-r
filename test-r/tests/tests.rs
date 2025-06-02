use std::time::{Duration, Instant};

#[test]
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
        .current_dir(root)
        .status()
        .unwrap();

    assert_eq!(process.code(), Some(0));
}

#[test]
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
fn timeout_works() {
    let cwd = std::env::current_dir().unwrap();
    let root = cwd.parent().unwrap().join("example-tokio");

    let start = Instant::now();
    let _process = std::process::Command::new("cargo")
        .arg("test")
        .arg("inner::tests::sleeping_test_3_timeout")
        .current_dir(root)
        .status()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_secs(5));
}

#[test]
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
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(line)
        {
            let event = map.get("event").unwrap().as_str().unwrap();
            if event == "ok" || event == "failed" {
                if let Some(serde_json::Value::String(s)) = map.get("name") {
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
    }

    assert!(output_it_does_work.contains("Print from 'it_does_work'\n"));
    assert_eq!(output_this_too, "Print from 'this_too'");
    assert_eq!(output_panic_test_1, "Print from 'panic_test_1'");
}

#[test]
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
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(line)
        {
            let event = map.get("event").unwrap().as_str().unwrap();
            if event == "ok" || event == "failed" {
                if let Some(serde_json::Value::String(s)) = map.get("name") {
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
    }

    assert!(output_it_does_work.contains("Print from 'it_does_work'\n"));
    assert_eq!(output_this_too, "Print from 'this_too'");
    assert_eq!(output_inner_test_works, "Print from inner test");
}
