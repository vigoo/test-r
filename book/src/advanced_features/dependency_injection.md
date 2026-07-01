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
    use test_r::{inherit_test_dep, test};
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
fn test4(#[tagged_as("tag1")] shared: &SharedDependency) {
    assert_eq!(shared.value, 1);
}
```

Tagged dependencies can also be used as parameters of **dependency constructors**. This allows a `#[test_dep]` to depend on a specific tagged instance of another dependency:

```rust
struct DerivedDependency {
    value: i32,
}

#[test_dep]
fn derived_dependency(#[tagged_as("tag1")] shared: &SharedDependency) -> DerivedDependency {
    DerivedDependency { value: shared.value * 2 }
}
```

It is also possible to **inherit** tagged dependencies from an outer suite:

```rust
mod inner {
    use test_r::{inherit_test_dep, test};
    use super::SharedDependency;
    
    inherit_test_dep!(#[tagged_as("tag1")] SharedDependency);
    inherit_test_dep!(#[tagged_as("tag2")] SharedDependency);
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
fn test5(#[dimension(shd)] shared: &SharedDependency) {
    // ...
}
```

The library will generate two separate test functions (named `test5::test5_tag1` and `test5::test5_tag2`) from this definition, and each will use a different instance of `SharedDependency`.

### Auto-derived case tags

Every matrix-generated test case additionally carries an **auto-derived tag** of the form `<dimension>_<case>`. For a dimension named `db` with cases `postgres` and `sqlite`, the generated cases get the tags `db_postgres` and `db_sqlite` respectively (alongside any explicit `#[tag(...)]` already on the test).

These auto-tags are ordinary tags — they are selectable with the existing [`:tag:` filter](./tags.md). So after:

```rust
#[test_dep(tagged_as = "postgres")]
fn db_postgres() -> DbDep { DbDep::postgres() }

#[test_dep(tagged_as = "sqlite")]
fn db_sqlite() -> DbDep { DbDep::sqlite() }

define_matrix_dimension!(db: DbDep -> "postgres", "sqlite");

#[test]
fn my_test(#[dimension(db)] dep: &DbDep) {
    // ...
}
```

you can run only the `sqlite` case with:

```sh
cargo test ':tag:db_sqlite'
```

The auto-derived tag is added to each generated case's tags; it never replaces an explicit `#[tag(...)]`. With multiple dimensions (the Cartesian product), each generated case carries the relevant subset of auto-tags. For example, a test with dimensions `db` (postgres/sqlite) and `lang` (ts/rust) produces a case for `db_postgres` + `lang_ts` that carries `["db_postgres", "lang_ts"]` plus any explicit tags, and you can select it with `cargo test ':tag:db_postgres&lang_ts'`.

### Applying a dimension to every test in a module: `matrix_suite!`

When a whole module of tests shares the same matrix dimension, annotating every test function's parameter with `#[dimension(...)]` is repetitive. The function-like `matrix_suite!(<module>, <dim>, <DepType>)` macro applies a matrix dimension to every `#[test]` in `<module>` whose dependency list contains the untagged `<DepType>` dep, without any per-test `#[dimension]` annotations. It is invoked in the **parent** of the target module — like `tag_suite!` / `sequential_suite!` — after the module is declared, and it works with file-based modules (`mod my_suite;`) as well as inline ones:

```rust
// The dimension helper must be in scope where `matrix_suite!` is invoked,
// so define the dimension in the same parent module.
define_matrix_dimension!(db: EnvDeps -> "postgres", "sqlite");

// An untagged `EnvDeps` constructor: its body is never used for the matrix'd
// tests below (their dependency is rewritten to the tagged variant at
// runtime), but the *getter symbol* it emits must exist so the `#[test]`
// expansion of `deps: &EnvDeps` (untagged) compiles.
#[test_dep]
fn create_env_deps() -> EnvDeps { /* postgres-by-default, or any value */ }

mod my_suite {
    use test_r::test;

    // Inherit the tagged (postgres/sqlite) getters AND the untagged getter
    // into this child module. The untagged getter is required for the
    // `&EnvDeps` parameter to compile; at runtime the aliasing dependency
    // view redirects it to the per-case tagged dep.
    test_r::inherit_test_dep!(#[tagged_as("postgres")] EnvDeps);
    test_r::inherit_test_dep!(#[tagged_as("sqlite")] EnvDeps);
    test_r::inherit_test_dep!(EnvDeps);

    // tests here take `deps: &EnvDeps` with NO #[dimension] attribute
    #[test]
    fn thing_one(deps: &EnvDeps) { /* ... */ }

    #[test]
    fn thing_two(deps: &EnvDeps) { /* ... */ }

    // A test that does NOT take a `&EnvDeps` parameter is left untouched:
    // it runs exactly once, not matrix-expanded.
    #[test]
    fn helper_without_dep() { /* ... */ }
}

// Register the runtime Matrix suite property for `my_suite`.
test_r::matrix_suite!(my_suite, db, EnvDeps);
```

`thing_one` and `thing_two` are each multiplied into one test per case (`thing_one_postgres` / `thing_one_sqlite` / `thing_two_postgres` / `thing_two_sqlite`) at test-collection time, carrying the `db_postgres` / `db_sqlite` auto-tags. `helper_without_dep` is not multiplied because it does not depend on the `EnvDeps` dep, so it runs exactly once.

#### How `matrix_suite!` works, and why it is a runtime (post-hoc) macro

`matrix_suite!` does **not** rewrite the module at compile time. Instead it emits a `#[cfg(test)]` constructor that calls the dimension's `test_r_get_dep_tags_<dim>()` helper (emitted by `define_matrix_dimension!`) and registers a suite-level `Matrix` property keyed by `<module>` and the untagged dep name derived from `<DepType>`. At test-collection time the runner applies suite properties to the already-registered tests: for every test under `<module>` whose `dependencies` contain that untagged dep name, it produces one clone per case — named `<test>_<case>`, tagged `<dim>_<case>`, with the dependency entry rewritten to the case-specific tagged dep, and the test closure wrapped in an aliasing `DependencyView` that redirects the compiled (untagged) getter lookup to that tagged dep.

Because the closure is *already compiled* against the untagged getter, the untagged getter symbol must still exist at compile time (hence the untagged `#[test_dep]` / `inherit_test_dep!` shown above) — but the untagged dep is never materialized for a matrix'd test, since its dependency list is rewritten to the tagged variant. This is why `matrix_suite!` is a **function-like macro invoked in the parent** (`test_r::matrix_suite!(my_suite, db, EnvDeps)`), just like `tag_suite!` and `sequential_suite!`: it only needs the module's *name*, so the target module may be file-based, and no compile-time introspection of the module body is required.

The `<DepType>` match is by the untagged dep name derived from the type as written (the same lowercased, segment-joined spelling `#[test_dep]` / `#[dimension]` use). Generic types are supported: `matrix_suite!(suite, kind, Wrapped<Primary>)` matches a test taking `deps: &Wrapped<Primary>` (untagged dep name `wrappedprimary`); a test taking `&Wrapped<Secondary>` is left untouched because its dep name differs.

`matrix_suite!` only multiplies tests whose dependency list contains the untagged dep name. A test that already carries an explicit `#[dimension]` or `#[tagged_as]` for that dep is also left untouched (its dependency is already a tagged variant, not the untagged name), so you can mix per-test overrides inside a matrix suite. Benches (`#[bench]`) are not multiplied (the matrix mechanism is not defined for benchmarks).
