# Dependency injection

Tests can share dependencies in `test-r`. This is especially useful for integration tests where setting up the integration environment is expensive.

## Using shared dependencies
To **use** a shared dependency from a test, we simply need to add a reference parameter to the test function:

```rust
use test_r::test;

struct SharedDependency {
    value: i32,
}

struct OtherDependency {
    value: i32,
}

#[test]
fn test1(shared: &SharedDependency) {
    assert_eq!(shared.value, 42);
}

#[test]
async fn test2(shared: &SharedDependency, other: &OtherDependency) {
    assert_eq!(shared.value, other.value);
}
```

The name of the parameters does not matter - test dependencies are indexed by their **type**. If a test needs multiple instances of the same type, a newtype wrapper can be used to distinguish them.

## Providing shared dependencies

Shared dependencies need to be provided for **each test suite**. A test suite in `test-r` is the enclosing **module** where the test functions are defined. It is possible to provide different values for the same dependency in different suites, but it is also possible to "import" provided dependencies from an outer suite. This flexibility allows for a wide range of uses cases, from defining singleton dependencies for a whole crate to detailed customization for specific tests. 

Test dependencies are provided by **constructor functions** annotated with `#[test_dep]`. The constructor function can be sync or async (if the `tokio` feature is enabled):

```rust
use test_r::test_dep;

#[test_dep]
async fn shared_dependency() -> SharedDependency {
    SharedDependency { value: 42 }
}

#[test_dep]
fn other_dependency() -> OtherDependency {
    OtherDependency { value: 42 }
}
```

Whether the dependency was created by a sync or async function does not matter - they can be used in both sync and async tests.

### Using dependencies provided for an outer test suite

As explained above, test dependencies must be provided in **each test module**. So if we want to use the same instances in an inner test suite, it has to be **inherited**:

```rust
mod inner {
    use test_r::{inher_test_dep, test};
    use super::SharedDependency;
    
    inherit_test_dep!(SharedDependency);
    
    #[test]
    fn test3(shared: &SharedDependency) {
        assert_eq!(shared.value, 42);
    }
}
```

## Dependency graph

Test dependency constructors can depend on other dependencies just like tests are. This allows defining a complex **dependency graph**, where each shared dependency is created in the correct order, and only when needed, and they got dropped as soon as no other test needs them.

The following example defines a third dependency (based on the above examples) which requires the other two to get constructed:

```rust
struct ThirdDependency {
    value: i32,
}

#[test_dep]
fn third_dependency(shared: &SharedDependency, other: &OtherDependency) -> ThirdDependency {
    ThirdDependency { value: shared.value + other.value }
}
```


