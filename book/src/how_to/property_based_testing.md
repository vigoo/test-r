# Property based testing

## Property based testing using the proptest crate

The [proptest library](https://crates.io/crates/proptest) works well together with `test-r`. There is no special requirements, just make sure to import `test-r`'s `test` attribute before using the `proptest!` macro to define the property based tests.

For example:

```rust
use test_r::test;
use proptest::prelude::*;

fn parse_date(s: &str) -> Option<(u32, u32, u32)> {
    todo!()
}

proptest! {
    #[test]
    fn parses_all_valid_dates(s in "[0-9]{4}-[0-9]{2}-[0-9]{2}") {
        parse_date(&s);
    }
}
```
