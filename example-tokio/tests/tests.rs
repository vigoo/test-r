use test_r::test;
use tokio::io::AsyncWriteExt;

test_r::enable!();

#[test]
async fn it_works() {
    let _ = tokio::io::stdout()
        .write(b"Print from 'it_works'\n")
        .await
        .unwrap();
    let result = 2 + 2;
    assert_eq!(result, 4);
}

#[path = "../../example/tests/common/retry.rs"]
mod retry_fixture;

#[test]
#[ignore]
#[test_r::flaky(2)]
async fn retry_evidence() {
    tokio::task::yield_now().await;
    retry_fixture::retry_body();
}

#[test]
#[ignore]
async fn retry_suite_evidence() -> Result<(), String> {
    tokio::task::yield_now().await;
    retry_fixture::suite_body()
}

#[test]
#[ignore]
#[test_r::non_flaky(2)]
async fn retry_non_flaky_evidence() {
    tokio::task::yield_now().await;
    retry_fixture::retry_body();
}

#[test]
#[ignore]
#[test_r::always_capture]
#[test_r::flaky(2)]
async fn retry_forced_capture() {
    retry_fixture::forced_capture_body();
}
