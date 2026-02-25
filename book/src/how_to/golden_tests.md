# Golden tests

Golden tests are comparing a previously saved output for a given test with the current output. This can be very useful to verify backward compatibility, for example.
There are several golden testing libraries available in the Rust ecosystem. 

The `test-r` crate does not provide a built-in support for golden tests, but it should work with most of these libraries.

## Golden tests with the goldenfile crate

The [goldenfile](https://crates.io/crates/goldenfile) crate is proven to work well with `test-r`. For example the following helper function can be used to check the backward compatibility of reading serialized binary data with some custom serialize/deserialize functions requiring [desert_rust](https://crates.io/crates/desert_rust) codecs:

```rust
use desert_rust::{BinaryCodec, BinarySerializer, BinaryDeserializer, serialize_to_byte_vec, deserialize};
use goldenfile::Mint;
use test_r::test;

fn is_deserializable<T: BinaryCodec + PartialEq + Debug>(old: &Path, new: &Path) {
    let old = std::fs::read(old).unwrap();
    let new = std::fs::read(new).unwrap();

    // Both the old and the latest binary can be deserialized
    let old_decoded: T = deserialize(&old).unwrap();
    let new_decoded: T = deserialize(&new).unwrap();

    // And they represent the same value
    assert_eq!(old_decoded, new_decoded);
}

pub(crate) fn backward_compatible<T: BinaryCodec + PartialEq + Debug + 'static>(
    name: impl AsRef<str>,
    mint: &mut Mint,
    value: T,
) {
    let mut file = mint
        .new_goldenfile_with_differ(
            format!("{}.bin", name.as_ref()),
            Box::new(is_deserializable::<T>),
        )
        .unwrap();
    let encoded = serialize_to_byte_vec(&value).unwrap();
    file.write_all(&encoded).unwrap();
    file.flush().unwrap();
}

#[derive(Debug, PartialEq, BinaryCodec)]
struct Example {
    value: i32,
}

#[test]
pub fn example() {
    let mut mint = Mint::new("tests/goldenfiles");
    backward_compatible("example1", &mut mint, Example { value: 42 });
}
```
