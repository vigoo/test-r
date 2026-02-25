test_r::enable!();

mod other;

#[cfg(test)]
mod tests {
    use test_r::core::bench::Bencher;
    use test_r::{always_ensure_time, always_report_time, bench, tag, test, test_dep};

    #[test]
    #[tag(output_capture_test)]
    fn it_does_work() {
        println!("Print from 'it_does_work'");
        eprintln!("Stderr from 'it_does_work'");
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    #[tag(output_capture_test)]
    #[always_report_time]
    #[always_ensure_time]
    fn this_too() {
        println!("Print from 'this_too'");
        eprintln!("Stderr from 'this_too'");
        let result = 2 + 2;
        assert_eq!(result, 4);
    }

    #[bench]
    fn bench1(b: &mut Bencher) {
        b.iter(|| 10 + 11);
    }

    pub struct Dep1 {
        pub value: i32,
    }

    #[test_dep]
    fn create_dep1() -> Dep1 {
        println!("Creating Dep1 for bench2");
        Dep1 { value: 10 }
    }

    #[bench]
    fn bench2(b: &mut Bencher, dep1: &Dep1) {
        b.iter(|| dep1.value + 11);
    }
}

mod inner {
    #[cfg(test)]
    mod tests {
        use test_r::{never_ensure_time, tag, test};

        #[test]
        #[tag(output_capture_test)]
        #[never_ensure_time]
        fn inner_test_works() {
            println!("Print from inner test");
            eprintln!("Stderr from inner test");
            let result = 2 + 2;
            assert_eq!(result, 4);
        }

        #[test]
        #[ignore]
        fn ignored_inner_test_works() {
            println!("Print from ignored inner test");
            let result = 2 + 2;
            assert_eq!(result, 5);
        }
    }

    mod slow {
        #[cfg(test)]
        mod tests {
            use test_r::{never_report_time, test};

            #[test]
            #[never_report_time]
            fn sleeping_test_1() {
                println!("Print from sleeping test 1");
                std::thread::sleep(std::time::Duration::from_secs(10));
                let result = 2 + 2;
                assert_eq!(result, 4);
            }

            #[test]
            fn sleeping_test_2() {
                println!("Print from sleeping test 2");
                std::thread::sleep(std::time::Duration::from_secs(10));
                let result = 2 + 2;
                assert_eq!(result, 4);
            }

            #[test]
            fn sleeping_test_3() {
                println!("Print from sleeping test 3");
                std::thread::sleep(std::time::Duration::from_secs(10));
                let result = 2 + 2;
                assert_eq!(result, 4);
            }

            #[test]
            fn sleeping_test_4() {
                println!("Print from sleeping test 4");
                std::thread::sleep(std::time::Duration::from_secs(5));
                let result = 2 + 2;
                assert_eq!(result, 4);
            }

            #[test]
            fn sleeping_test_5() {
                println!("Print from sleeping test 5");
                std::thread::sleep(std::time::Duration::from_secs(5));
                let result = 2 + 2;
                assert_eq!(result, 4);
            }
        }
    }
}

#[cfg(test)]
mod generic_deps {
    use std::sync::Arc;
    use test_r::{test, test_dep};

    pub struct Dep1 {
        pub value: i32,
    }

    pub struct Dep2 {
        pub value: i32,
    }

    #[test_dep]
    pub fn create_dep1() -> Arc<Dep1> {
        println!("Creating Dep1");
        Arc::new(Dep1 { value: 10 })
    }

    #[test_dep]
    pub fn create_dep2() -> Arc<Dep2> {
        println!("Creating Dep2");
        Arc::new(Dep2 { value: 20 })
    }

    #[test]
    pub fn test_with_deps(dep1: &Arc<Dep1>, dep2: &Arc<Dep2>) {
        println!("Test with deps");
        assert_eq!(dep1.value + dep2.value, 30);
    }
}

#[cfg(test)]
mod lazy_dep_pruning {
    use test_r::{test, test_dep};

    pub struct DepA {
        pub value: i32,
    }

    pub struct DepB {
        pub value: i32,
    }

    pub struct DepC {
        pub value: i32,
    }

    #[test_dep]
    fn create_dep_a() -> DepA {
        println!("LAZY_DEPS_MARKER: Creating DepA");
        DepA { value: 1 }
    }

    #[test_dep]
    fn create_dep_b() -> DepB {
        println!("LAZY_DEPS_MARKER: Creating DepB");
        DepB { value: 2 }
    }

    #[test_dep]
    fn create_dep_c(dep_a: &DepA) -> DepC {
        println!("LAZY_DEPS_MARKER: Creating DepC");
        DepC {
            value: dep_a.value + 10,
        }
    }

    #[test]
    fn test_uses_dep_a(dep_a: &DepA) {
        assert_eq!(dep_a.value, 1);
    }

    #[test]
    fn test_uses_dep_b(dep_b: &DepB) {
        assert_eq!(dep_b.value, 2);
    }

    #[test]
    fn test_uses_both(dep_a: &DepA, dep_b: &DepB) {
        assert_eq!(dep_a.value + dep_b.value, 3);
    }

    #[test]
    fn test_uses_none() {
        let x = 4;
        assert_eq!(x, 4);
    }

    #[test]
    fn test_uses_dep_c(dep_c: &DepC) {
        assert_eq!(dep_c.value, 11);
    }
}

#[cfg(test)]
mod nested_sequential {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use test_r::sequential;

    static CONCURRENT_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn assert_no_concurrency() {
        let prev = CONCURRENT_COUNT.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            prev, 0,
            "Tests are running concurrently in a sequential subtree!"
        );
        std::thread::sleep(Duration::from_millis(50));
        CONCURRENT_COUNT.fetch_sub(1, Ordering::SeqCst);
    }

    #[sequential]
    mod parent {
        use super::assert_no_concurrency;
        use test_r::test;

        #[test]
        fn parent_test_1() {
            assert_no_concurrency();
        }

        mod child_a {
            use super::assert_no_concurrency;
            use test_r::test;

            #[test]
            fn child_a_test_1() {
                assert_no_concurrency();
            }

            #[test]
            fn child_a_test_2() {
                assert_no_concurrency();
            }
        }

        mod child_b {
            use super::assert_no_concurrency;
            use test_r::test;

            #[test]
            fn child_b_test_1() {
                assert_no_concurrency();
            }

            #[test]
            fn child_b_test_2() {
                assert_no_concurrency();
            }
        }
    }
}
