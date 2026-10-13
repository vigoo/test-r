use std::sync::atomic::{AtomicUsize, Ordering};

pub fn forced_capture_body() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let attempt = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    println!("captured sibling invocation {attempt}");
    assert!(attempt > 1, "captured sibling failure");
}

pub fn suite_body() -> Result<(), String> {
    let marker = std::env::var("TEST_R_RETRY_MARKER").unwrap();
    if std::path::Path::new(&marker).exists() {
        Ok(())
    } else {
        std::fs::write(marker, b"first suite").unwrap();
        eprintln!("failed suite diagnostic");
        Err("first suite failure".into())
    }
}

pub fn retry_body() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let attempt = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    let scenario = std::env::var("TEST_R_RETRY_SCENARIO").unwrap();
    if scenario == "partial" {
        print!("stdout invocation {attempt}");
        eprint!("stderr invocation {attempt}");
    } else {
        println!("stdout invocation {attempt}");
        eprintln!("stderr invocation {attempt}");
    }
    match scenario.as_str() {
        "pass" => {}
        "fail" => panic!("failure invocation {attempt}"),
        "prove" => assert_eq!(attempt, 1, "failure invocation 2"),
        "detached" => {
            if attempt == 1 {
                std::thread::sleep(std::time::Duration::from_millis(400));
                panic!("failure invocation 1");
            }
            let _ = test_r::spawn_thread(|| panic!("failure invocation 2")).join();
        }
        "nested" => {
            if attempt == 1 {
                panic!("failure invocation 1");
            }
            let marker = std::env::var("TEST_R_RETRY_MARKER").unwrap();
            if !std::path::Path::new(&marker).exists() {
                std::fs::write(marker, b"first suite").unwrap();
                panic!("failure invocation {attempt}");
            }
        }
        "recover" | "partial" | "mixed" => assert!(attempt > 1, "failure invocation 1"),
        _ => panic!("unknown scenario"),
    }
}
