#[cfg(test)]
mod tests {
    use rand::Rng;
    use std::time::Duration;
    use test_r::{flaky, non_flaky, test};

    #[test]
    fn other_module_test_works() {
        println!("Print from other module's test");
        let result = 2 + 2;
        assert_eq!(result, 4);
    }

    #[test]
    #[flaky(10)]
    fn flaky_test() {
        println!("Print from flaky test");
        let mut rng = rand::thread_rng();
        let result = 2 + rng.gen_range(1..3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(result, 4);
    }

    #[test]
    #[non_flaky(10)]
    fn non_flaky_test() {
        println!("Print from non_flaky test");
        let result = 2 + 2;
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(result, 4);
    }
}
