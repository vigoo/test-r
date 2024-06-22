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
