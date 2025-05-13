# Dynamic test generation

Normally the test tree is static, defined compile time using **modules** representing test suites and **functions** annotated with `#[test]` defining test cases. Sometimes however it is useful to generate test cases runtime. `test-r` supports this using the `#[test_gen]` attribute.

Test generators can be either sync or async (if the `tokio` feature is enabled). The generator function must take a single parameter, a mutable reference to `DynamicTestRegistration`. Dependency injection to the generator function is **not supported** currently, but the dynamically generated tests can use shared dependencies.

The following two examples demonstrate generating sync and async tests using the `#[test_gen]` attribute:

```rust
use test_r::core::{DynamicTestRegistration, TestType};
use test_r::{add_test, test_gen};

struct Dep1 {
    value: i32,
}

struct Dep2 {
    value: i32,
}

#[test_gen]
fn gen_sync_tests(r: &mut DynamicTestRegistration) {
    println!("Generating some tests with dependencies in a sync generator");
    for i in 0..10 {
        add_test!(
            r,
            format!("test_{i}"),
            TestType::UnitTest,
            move |dep1: &Dep1| {
                println!("Running test {} using dep {}", i, dep1.value);
                let s = i.to_string();
                let i2 = s.parse::<i32>().unwrap();
                assert_eq!(i, i2);
            }
        );
    }
}

#[test_gen]
async fn gen_async_tests(r: &mut DynamicTestRegistration) {
    println!("Generating some async tests with dependencies in a sync generator");
    for i in 0..10 {
        add_test!(
            r,
            format!("test_{i}"),
            TestType::UnitTest,
            move |dep1: &Dep1, d2: &Dep2| async {
                println!("Running test {} using deps {} {}", i, dep1.value, d2.value);
                let s = i.to_string();
                let i2 = s.parse::<i32>().unwrap();
                assert_eq!(i, i2);
            }
        );
    }
}
```

The generator functions are executed at the startup of the test runner, and all the generated tests are added to the test tree. The **name** of the generated tests must be unique. Each test is added to the **test suite** the generator function is defined in.

<div class="warning">
Test generators are executed in both the main process and in all the child processes spawned for output capturing. For this reason, they must be idempotent, and they should not print any output - as the output would not be captured when the generator runs in the primary process, and it would interfere with output formats such as `json` or `junit`.  
</div>
