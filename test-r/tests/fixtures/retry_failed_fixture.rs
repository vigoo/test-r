//! Fixtures for `--retry-failed`. Every test is ignored so ordinary runs skip
//! these intentional failures. Each one counts its invocations in files under
//! `TEST_R_RETRY_FAILED_DIR`, because capture mode reruns failed tests in
//! fresh worker processes where in-memory counters would restart.

use std::path::PathBuf;
use test_r::{flaky, non_flaky, test, test_dep};

test_r::enable!();

/// Records one more invocation of `name` and returns its 1-based number.
fn invocation(name: &str) -> usize {
    let dir = PathBuf::from(std::env::var("TEST_R_RETRY_FAILED_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let count = std::fs::read_to_string(&path).map_or(0, |s| s.len()) + 1;
    std::fs::write(&path, "x".repeat(count)).unwrap();
    println!("{name} invocation {count}");
    count
}

#[test]
#[ignore]
fn passes() {
    invocation("passes");
}

#[test]
#[ignore]
fn panics_once() {
    assert!(invocation("panics_once") > 1, "failure invocation 1");
}

#[test]
#[ignore]
fn errs_once() -> Result<(), String> {
    match invocation("errs_once") {
        1 => Err("failure invocation 1".to_string()),
        _ => Ok(()),
    }
}

#[test]
#[ignore]
fn fails_twice() {
    let n = invocation("fails_twice");
    assert!(n > 2, "failure invocation {n}");
}

#[test]
#[ignore]
fn always_fails() {
    let n = invocation("always_fails");
    panic!("failure invocation {n}");
}

#[test]
#[ignore]
#[flaky(2)]
fn flaky_exhausts() {
    let n = invocation("flaky_exhausts");
    assert!(n > 2, "failure invocation {n}");
}

#[test]
#[ignore]
#[non_flaky(2)]
fn non_flaky_fails() {
    let n = invocation("non_flaky_fails");
    assert!(n != 2, "failure invocation {n}");
}

/// A harness failure: the first invocation does not panic as required.
#[test]
#[ignore]
#[should_panic]
fn should_panic_once() {
    let n = invocation("should_panic_once");
    assert!(n == 1, "panicked as expected");
}

pub struct Fresh(usize);

#[test_dep]
fn fresh() -> Fresh {
    Fresh(invocation("fresh_dep"))
}

/// Passes only when the retry received a newly constructed dependency.
#[test]
#[ignore]
fn needs_fresh_dep(dep: &Fresh) {
    let n = invocation("needs_fresh_dep");
    assert!(n > 1 && dep.0 == n, "failure invocation {n}");
}

pub struct Broken;

#[test_dep]
fn broken() -> Broken {
    assert!(invocation("broken_dep") > 1, "dependency setup failed");
    Broken
}

#[test]
#[ignore]
fn needs_broken_dep(_dep: &Broken) {
    invocation("needs_broken_dep");
}

#[cfg(feature = "tokio")]
mod asynchronous {
    use super::invocation;
    use test_r::{test, timeout};

    #[test]
    #[ignore]
    async fn async_panics_once() {
        assert!(invocation("async_panics_once") > 1, "failure invocation 1");
    }

    #[test]
    #[ignore]
    #[timeout(200)]
    async fn async_times_out_once() {
        if invocation("async_times_out_once") == 1 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}
