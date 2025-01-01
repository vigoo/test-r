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

## Dependency tagging
It is possible to have multiple dependency constructors of the same type, distinguished by a string **tag**. This is an alternative to using newtype wrappers, and it enables the **dependency matrix** feature explained in the next section.

To tag a dependency, use the `#[test_dep]` attribute with the following argument:

```rust
#[test_dep(tagged_as = "tag1")]
fn shared_dependency_tag1() -> SharedDependency {
    SharedDependency { value: 1 }
}

#[test_dep(tagged_as = "tag2")]
fn shared_dependency_tag2() -> SharedDependency {
    SharedDependency { value: 2 }
}
``` 

Tagged dependencies are not injected automatically for parameters of the same type, they need to have a matching `tagged_as` attribute:

```rust
#[test]
fn test4(shared: #[tagged_as("tag1")] &SharedDependency) {
    assert_eq!(shared.value, 1);
}
```

It is also possible to **inherit** tagged dependencies from an outer suite:

```rust
mod inner {
    use test_r::{inher_test_dep, test};
    use super::SharedDependency;
    
    inherit_test_dep!(#[tagged_as("tag1") SharedDependency);
    inherit_test_dep!(#[tagged_as("tag2") SharedDependency);
}
```

## Dependency matrix
`test-r` combines the above described **dependency tagging** feature with its [generated tests feature](./dynamic_test_generation.md) to provide an easy way to test a matrix of configurations, represented by different values of test dependencies.

This can be used for example to test a table of different inputs, or to run tests with multiple implementations of the same interface.

The first step is to define a **tagged test dependency** for each value used in the matrix.
Take the previous section as an example where two different `SharedDependency` was created with tags `tag1` and `tag2`.

The second step is to define a **matrix dimension** with the `define_matrix_dimension!` macro:

```rust
define_matrix_dimension!(shd: SharedDependency -> "tag1", "tag2");
```

In this example:
- `shd` is the name of the dimension - there can be an arbitrary number of dimensions defined, and they can be used in any combination in test functions
- `SharedDependency` is the type of the dependency
- `"tag1", "tag2"` are the tags used in the dependency matrix for this dependency

The third step is to mark one or more parameters of a test function to match one of the defined dimensions:

```rust
#[test]
fn test5(#[dimension(shd) shared: &SharedDependency) {
    // ...
}
```

The library will generate two separate test functions (named `test5::test5_tag1` and `test5::test5_tag2`) from this definition, and each will use a different instance of `SharedDependency`.

