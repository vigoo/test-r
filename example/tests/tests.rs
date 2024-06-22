use test_r::test;

test_r::enable!();

#[test]
fn it_works() {
    let result = 2 + 2;
    assert_eq!(result, 4);
}