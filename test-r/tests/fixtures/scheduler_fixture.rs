test_r::enable!();

// Explicitly enabled by the scheduler regression test, not ordinary test runs.
#[test_r::sequential]
mod sequential {
    use std::time::Duration;
    use test_r::test;

    #[test]
    #[ignore]
    fn s1() {
        std::thread::sleep(Duration::from_millis(200));
    }

    #[test]
    #[ignore]
    fn s2() {
        std::thread::sleep(Duration::from_millis(200));
    }

    mod nested {
        use super::*;
        use test_r::test;

        #[test]
        #[ignore]
        fn s3() {
            std::thread::sleep(Duration::from_millis(200));
        }

        #[test]
        #[ignore]
        fn s4() {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

mod parallel {
    use test_r::test;

    #[test]
    #[ignore]
    fn p1() {}

    #[test]
    #[ignore]
    fn p2() {}

    #[test]
    #[ignore]
    fn p3() {}
}
