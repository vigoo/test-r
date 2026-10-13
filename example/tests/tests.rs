use test_r::test;

test_r::enable!();

#[test]
fn it_works() {
    println!("Print from 'it_works'");
    let result = 2 + 2;
    assert_eq!(result, 4);
}

#[path = "common/retry.rs"]
mod retry_fixture;

#[test]
#[ignore]
#[test_r::flaky(2)]
fn retry_evidence() {
    retry_fixture::retry_body();
}

#[test]
#[ignore]
fn retry_suite_evidence() -> Result<(), String> {
    retry_fixture::suite_body()
}

#[test]
#[ignore]
#[test_r::non_flaky(2)]
fn retry_non_flaky_evidence() {
    retry_fixture::retry_body();
}

#[test]
#[ignore]
#[test_r::always_capture]
#[test_r::flaky(2)]
fn retry_forced_capture() {
    retry_fixture::forced_capture_body();
}
