test_r::enable!();

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt::{Debug, Display, Formatter};
    use test_r::{never_report_time, tag, test};
    use tokio::io::AsyncWriteExt;

    #[test]
    #[tag(output_capture_test)]
    async fn it_does_work() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'it_does_work'\n")
            .await
            .unwrap();
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    #[tag(output_capture_test)]
    async fn this_too() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'this_too'\n")
            .await
            .unwrap();
        let result = 2 + 2;
        assert_eq!(result, 4);
    }

    #[test]
    #[should_panic]
    #[tag(output_capture_test)]
    async fn panic_test_1() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'panic_test_1'\n")
            .await
            .unwrap();
        panic!("This test should panic");
    }

    #[test]
    #[should_panic(expected = "hello world")]
    async fn panic_test_2a() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'panic_test_2a'\n")
            .await
            .unwrap();
        panic!("hello world");
    }

    #[test]
    #[should_panic(expected = "hello world")]
    #[never_report_time]
    async fn panic_test_2b() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'panic_test_2b'\n")
            .await
            .unwrap();
        panic!("something else");
    }

    struct CustomError;

    impl Debug for CustomError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "CustomError")
        }
    }

    impl Display for CustomError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "Failed with custom error")
        }
    }

    impl Error for CustomError {}

    #[test]
    async fn result_based_test_ok() -> Result<String, std::io::Error> {
        println!("Print from succeeding result based test");
        Ok("Success".to_string())
    }

    #[test]
    async fn result_based_test_err() -> Result<String, CustomError> {
        println!("Print from failing result based test");
        Err(CustomError)
    }
}

mod inner {

    #[cfg(test)]
    mod tests {
        use test_r::{
            always_report_time, never_ensure_time, never_report_time, tag, test, timeout,
        };
        use tokio::io::AsyncWriteExt;

        #[test]
        #[tag(a)]
        async fn inner_test_works() {
            let _ = tokio::io::stdout()
                .write(b"Print from inner test\n")
                .await
                .unwrap();
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        #[ignore]
        async fn ignored_inner_test_works() {
            let _ = tokio::io::stdout()
                .write(b"Print from ignored inner test\n")
                .await
                .unwrap();
            let result = 2 + 2;
            assert_eq!(result, 5);
        }

        #[test]
        async fn sleeping_test_1() {
            let _ = tokio::io::stdout()
                .write(b"Print from sleeping test 1\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        #[never_report_time]
        #[never_ensure_time]
        async fn sleeping_test_2() {
            let _ = tokio::io::stdout()
                .write(b"Print from sleeping test 2\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        #[timeout(3000)]
        #[always_report_time]
        async fn sleeping_test_3_timeout() {
            let _ = tokio::io::stdout()
                .write(b"Start sleeping in sleeping test 3\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            let _ = tokio::io::stdout()
                .write(b"Finished sleeping in sleeping test 3\n")
                .await
                .unwrap();
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        #[timeout("3s")]
        #[always_report_time]
        async fn sleeping_test_3_timeout_hr() {
            let _ = tokio::io::stdout()
                .write(b"Start sleeping in sleeping test 3\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            let _ = tokio::io::stdout()
                .write(b"Finished sleeping in sleeping test 3\n")
                .await
                .unwrap();
            let result = 2 + 2;
            assert_eq!(result, 4);
        }
    }
}

#[cfg(test)]
pub mod flakiness {
    use rand::Rng;
    use std::time::Duration;
    use test_r::{flaky, non_flaky, tag, test};

    #[test]
    #[flaky(10)]
    #[tag(a)]
    #[tag(b)]
    fn flaky_test() {
        println!("Print from flaky test");
        let mut rng = rand::rng();
        let result = 2 + rng.random_range(1..3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(result, 4);
    }

    #[test]
    #[test_r::flaky(10)]
    #[tag(a)]
    #[tag(b)]
    fn flaky_test_ex1() {
        println!("Print from flaky test");
        let mut rng = rand::rng();
        let result = 2 + rng.random_range(1..3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(result, 4);
    }

    #[test]
    #[::test_r::flaky(10)]
    #[tag(a)]
    #[tag(b)]
    fn flaky_test_ex2() {
        println!("Print from flaky test");
        let mut rng = rand::rng();
        let result = 2 + rng.random_range(1..3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(result, 4);
    }

    #[test]
    #[non_flaky(10)]
    #[tag(a)]
    fn non_flaky_test() {
        println!("Print from non_flaky test");
        let result = 2 + 2;
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(result, 4);
    }
}

#[cfg(test)]
pub mod benches {
    use std::sync::Arc;
    use test_r::AsyncBencher;
    use test_r::{bench, test_dep};

    #[bench]
    async fn bench1(b: &mut AsyncBencher) {
        b.iter(|| Box::pin(async { 10 + 11 })).await;
    }

    pub struct Dep1 {
        pub value: i32,
    }

    #[test_dep]
    fn create_dep1() -> Arc<Dep1> {
        println!("Creating Dep1 for bench2");
        Arc::new(Dep1 { value: 10 })
    }

    #[bench]
    async fn bench2(b: &mut AsyncBencher, dep1: &Arc<Dep1>) {
        let dep1 = dep1.clone();
        b.iter(move || {
            let dep1 = dep1.clone();
            Box::pin(async move { dep1.value + 11 })
        })
        .await;
    }
}

pub mod deps {
    #[cfg(test)]
    use test_r::sequential;

    #[derive(Debug)]
    pub struct Dep1 {
        pub value: i32,
    }

    impl Dep1 {
        pub fn new(value: i32) -> Self {
            println!("Creating Dep1 {value}");
            Self { value }
        }
    }

    impl Drop for Dep1 {
        fn drop(&mut self) {
            println!("Dropping Dep1 {}", self.value);
        }
    }

    #[cfg(test)]
    #[sequential]
    pub mod tests {
        use crate::deps::Dep1;
        use test_r::{test, test_dep};
        use tokio::io::AsyncWriteExt;
        use tracing::info;

        #[derive(Debug)]
        struct InitializedTracing;

        #[test_dep]
        fn initialized_tracing() -> InitializedTracing {
            tracing_subscriber::fmt::init();
            info!("Initialized tracing");
            InitializedTracing
        }

        #[test_dep]
        fn create_dep1() -> Dep1 {
            Dep1::new(10)
        }

        #[test_dep]
        async fn create_dep2() -> Dep2 {
            Dep2::new(20).await
        }

        #[derive(Debug)]
        pub struct Dep2 {
            pub value: i32,
        }

        impl Dep2 {
            pub async fn new(value: i32) -> Self {
                println!("Creating Dep2 {value}");
                Self { value }
            }
        }

        impl Drop for Dep2 {
            fn drop(&mut self) {
                println!("Dropping Dep2 {}", self.value);
            }
        }

        #[test]
        async fn sleeping_test_3() {
            let _ = tokio::io::stdout()
                .write(b"Print from sleeping test 3\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        async fn dep_test_works(dep1: &Dep1, dep2: &Dep2) {
            println!("Print from dep test");
            assert_eq!(dep1.value, 10);
            assert_eq!(dep2.value, 20);
        }

        mod inner {
            use crate::deps::tests::Dep2;
            use crate::deps::Dep1;
            use test_r::{inherit_test_dep, tag, test, test_dep};
            use tracing::info;

            inherit_test_dep!(Dep1);

            #[derive(Debug)]
            struct Dep3 {
                value: i32,
            }

            #[test_dep]
            fn create_dep3(dep2: &Dep2) -> Dep3 {
                println!("Creating Dep3 based on {}", dep2.value);
                Dep3 {
                    value: 30 + dep2.value,
                }
            }

            #[test_dep]
            async fn create_inner_dep2(dep1: &Dep1) -> Dep2 {
                println!("Creating inner Dep2 based on {}", dep1.value);
                Dep2::new(200 + dep1.value).await
            }

            #[test]
            #[tracing::instrument]
            #[tag(b)]
            async fn dep_test_inner_works_1(dep1: &Dep1) {
                info!("Print from dep test inner 1");
                assert_eq!(dep1.value, 10);
            }

            #[test]
            #[tracing::instrument]
            async fn dep_test_inner_works_2(dep2: &Dep2) {
                info!("Print from dep test inner 2");
                assert_eq!(dep2.value, 210);
            }

            #[test]
            #[tracing::instrument]
            async fn dep_test_inner_works_3(dep3: &Dep3) {
                info!("Print from dep test inner 3");
                assert_eq!(dep3.value, 240);
            }
        }
    }
}

#[cfg(test)]
mod generated {
    use crate::deps::tests::Dep2;
    use crate::deps::Dep1;
    use std::time::Duration;
    use test_r::core::internal::TestProperties;
    use test_r::core::{
        CaptureControl, DynamicTestRegistration, FlakinessControl, ReportTimeControl, ShouldPanic,
        TestType,
    };
    use test_r::{add_test, define_matrix_dimension, test, test_dep, test_gen};

    #[test_gen]
    fn generate_tests_1(r: &mut DynamicTestRegistration) {
        println!("Generating some tests in a sync generator");
        for i in 0..10 {
            r.add_sync_test(
                format!("test_{i}"),
                TestProperties::unit_test(),
                move |_| {
                    println!("Running test {}", i);
                    let s = i.to_string();
                    let i2 = s.parse::<i32>().unwrap();
                    assert_eq!(i, i2);
                },
            );
        }
    }

    #[test_gen]
    async fn generate_tests_2(r: &mut DynamicTestRegistration) {
        println!("Generating some tests in an async generator");
        for i in 0..10 {
            r.add_async_test(
                format!("test_{i}"),
                TestProperties::unit_test(),
                move |_| {
                    Box::pin(async move {
                        println!("Running test {}", i);
                        let s = i.to_string();
                        let i2 = s.parse::<i32>().unwrap();
                        assert_eq!(i, i2);
                    })
                },
            );
        }
    }

    #[test_dep]
    fn create_dep1() -> Dep1 {
        Dep1::new(10)
    }

    #[test_dep]
    async fn create_dep2() -> Dep2 {
        Dep2::new(10).await
    }

    #[test_dep(tagged_as = "primary")]
    async fn create_primary_dep2() -> Dep2 {
        create_dep2().await
    }

    #[test_dep(tagged_as = "secondary")]
    async fn create_secondary_dep2() -> Dep2 {
        Dep2::new(20).await
    }

    #[test]
    fn dep2_primary(dep2: &Dep2) {
        assert_eq!(dep2.value, 10);
    }

    #[test]
    fn dep2_secondary(#[tagged_as("secondary")] dep2_sec: &Dep2) {
        assert_eq!(dep2_sec.value, 20);
    }

    mod inner {
        use crate::deps::tests::Dep2;
        use test_r::inherit_test_dep;

        inherit_test_dep!(Dep2);
        inherit_test_dep!(
            #[tagged_as("primary")]
            Dep2
        );
        inherit_test_dep!(
            #[tagged_as("secondary")]
            Dep2
        );

        #[test]
        fn dep2_primary_inner(dep2: &Dep2) {
            assert_eq!(dep2.value, 10);
        }

        #[test]
        fn dep2_secondary_inner(#[tagged_as("secondary")] dep2_sec: &Dep2) {
            assert_eq!(dep2_sec.value, 20);
        }
    }

    #[test_gen]
    fn generate_tests_3(r: &mut DynamicTestRegistration) {
        println!("Generating some tests with dependencies in a sync generator");
        for i in 0..10 {
            add_test!(
                r,
                format!("test_{i}"),
                TestProperties::unit_test(),
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
    async fn generate_tests_4(r: &mut DynamicTestRegistration) {
        println!("Generating some async tests with dependencies in a sync generator");
        for i in 0..10 {
            add_test!(
                r,
                format!("test_{i}"),
                TestProperties::unit_test(),
                move |dep1: &Dep1, d2: &Dep2| async {
                    println!("Running test {} using deps {} {}", i, dep1.value, d2.value);
                    let s = i.to_string();
                    let i2 = s.parse::<i32>().unwrap();
                    assert_eq!(i, i2);
                }
            );
        }
    }

    #[test_gen]
    async fn generate_tests_4_s(r: &mut DynamicTestRegistration) {
        println!("Generating some async tests with dependencies in a sync generator");
        for i in 0..10 {
            add_test!(
                r,
                format!("test_{i}"),
                TestProperties::unit_test(),
                move |dep1: &Dep1, #[tagged_as("secondary")] d2: &Dep2| async {
                    println!("Running test {} using deps {} {}", i, dep1.value, d2.value);
                    let s = i.to_string();
                    let i2 = s.parse::<i32>().unwrap();
                    assert_eq!(i, i2);
                }
            );
        }
    }

    #[test_gen]
    #[allow(dependency_on_unit_never_type_fallback)]
    async fn generate_tests_5(r: &mut DynamicTestRegistration) {
        println!("Generating some async tests with custom properties");
        for i in 0..5 {
            r.add_async_test(
                format!("test_{i}"),
                TestProperties {
                    should_panic: ShouldPanic::WithMessage(format!("panic {i}")),
                    test_type: TestType::UnitTest,
                    timeout: Some(Duration::from_secs(2)),
                    flakiness_control: FlakinessControl::RetryKnownFlaky(2),
                    capture_control: CaptureControl::NeverCapture,
                    report_time_control: ReportTimeControl::Enabled,
                    ensure_time_control: ReportTimeControl::Disabled,
                    tags: vec!["example".to_string()],
                    is_ignored: false,
                },
                move |_| {
                    Box::pin(async move {
                        println!("Running test {}", i);
                        panic!("panic {i}");
                    })
                },
            );
        }
    }

    #[test_gen]
    #[allow(dependency_on_unit_never_type_fallback)]
    async fn generate_tests_6(r: &mut DynamicTestRegistration) {
        println!("Generating some async tests with custom properties using the add_test! macro");
        for i in 0..5 {
            add_test!(
                r,
                format!("test_{i}"),
                TestProperties {
                    should_panic: ShouldPanic::WithMessage(format!("panic {i}")),
                    test_type: TestType::UnitTest,
                    timeout: Some(Duration::from_secs(2)),
                    flakiness_control: FlakinessControl::RetryKnownFlaky(2),
                    capture_control: CaptureControl::NeverCapture,
                    report_time_control: ReportTimeControl::Enabled,
                    ensure_time_control: ReportTimeControl::Disabled,
                    tags: vec!["example".to_string()],
                    is_ignored: false,
                },
                move || async {
                    println!("Running test {}", i);
                    panic!("panic {i}");
                },
            );
        }
    }

    define_matrix_dimension!(d2: Dep2 -> "primary", "secondary");

    #[test]
    async fn autogenerated_tests(#[dimension(d2)] dep2: &Dep2) {
        println!("Running autogenerated tests with {}", dep2.value);
        assert_eq!(dep2.value % 2, 0);
    }

    #[test]
    async fn autogenerated_tests_2(dep1: &Dep1, #[dimension(d2)] dep2: &Dep2) {
        println!(
            "Running autogenerated tests with {} {}",
            dep1.value, dep2.value
        );
        assert_eq!((dep1.value + dep2.value) % 2, 0);
    }

    struct Dep3 {
        value: i32,
    }

    #[test_dep(tagged_as = "a")]
    fn create_dep3_a() -> Dep3 {
        Dep3 { value: 100 }
    }

    #[test_dep(tagged_as = "b")]
    fn create_dep3_b() -> Dep3 {
        Dep3 { value: 200 }
    }

    define_matrix_dimension!(d3: Dep3 -> "a", "b");

    #[test]
    async fn autogenerated_tests_3(
        dep1: &Dep1,
        #[dimension(d2)] dep2: &Dep2,
        #[dimension(d3)] dep3: &Dep3,
    ) {
        println!(
            "Running autogenerated tests with {} {} {}",
            dep1.value, dep2.value, dep3.value
        );
        assert_eq!((dep1.value + dep2.value + dep3.value) % 2, 0);
    }

    #[test]
    fn autogenerated_tests_3_s(
        dep1: &Dep1,
        #[dimension(d2)] dep2: &Dep2,
        #[dimension(d3)] dep3: &Dep3,
    ) {
        println!(
            "Running autogenerated tests with {} {} {}",
            dep1.value, dep2.value, dep3.value
        );
        assert_eq!((dep1.value + dep2.value + dep3.value) % 2, 0);
    }

    #[test_gen]
    fn generate_tests_name_collision(r: &mut DynamicTestRegistration) {
        println!("Generating some tests with dependencies in a sync generator");
        for i in 0..10 {
            add_test!(
                r,
                format!("test_{i}"),
                TestProperties::unit_test(),
                move |deps: &Dep1, other: &Dep2| async {
                    println!(
                        "Running test {} using dep {} and {}",
                        i, deps.value, other.value
                    );
                    let s = i.to_string();
                    let i2 = s.parse::<i32>().unwrap();
                    assert_eq!(i, i2);
                }
            );
        }
    }
}
