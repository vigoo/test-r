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
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    #[tag(output_capture_test)]
    #[always_report_time]
    #[always_ensure_time]
    fn this_too() {
        println!("Print from 'this_too'");
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
