# Tags

## Assigning tags
Tests can be associated with an arbitrary number of **tags**. Each tag is global, and must be a valid Rust identifier. 
Tags can be assigned to tests using the `#[tag]` attribute:

```rust
use test_r::{tag, test};

#[tag(tag1)]
#[tag(tag2)]
#[test]
fn tagged_test() {
    assert!(true);
}
```

## Tagging entire test suites

It is possible to tag an entire **test suite**. This can be done by using the `#[tags]` attribute on the module containing the tests, 
or alternatively using the `tag_suite!` macro:

```rust
use test_r::{tag, tag_suite, test};

mod inner1;

tag_suite!(inner1, tag1);

#[tags(tag2)]
mod inner2 {
    // ...
}
```

The `tag_suite!` macro is necessary because currently it is not possible to put attributes on non-inlined modules.

## Running tagged tests
The purpose of tagging tests is to run a subset of the crate's tests selected by tags. To select tests by tags, use the
`:tag:` prefix when passing the **test name** to `cargo test`:

```sh
cargo test :tag:tag1
``` 

This example will run every test tagged as `tag1`, but no others.

### Selecting untagged tests
Sometimes it is useful to select all tests **without a tag**. This can be done by using the `:tag:` prefix with no tag name:

```sh
cargo test :tag:
```

### Selecting tests by multiple tags

Multiple tags can be combined with the `|` (or) and `&` (and) operators. The `&` operator has higher precedence than `|`. So the following example:

```sh
cargo test ':tag:tag1|tag2&tag3'
```

is going to run tests tagged as either `tag1` or both `tag2` and `tag3`.
