#[cfg(test)]
#[test_r::tag(a)]
mod tests {
    use rand::Rng;
    use std::error::Error;
    use std::fmt::{Debug, Display, Formatter};
    use std::time::Duration;
    use test_r::{always_capture, flaky, never_capture, non_flaky, test};

    #[test]
    fn other_module_test_works() {
        println!("Print from other module's test");
        let result = 2 + 2;
        assert_eq!(result, 4);
    }

    #[test]
    #[flaky(10)]
    #[always_capture]
    fn flaky_test() {
        println!("Print from flaky test");
        let mut rng = rand::thread_rng();
        let result = 2 + rng.gen_range(1..3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(result, 4);
    }

    #[test]
    #[non_flaky(10)]
    #[never_capture]
    fn non_flaky_test() {
        println!("Print from non_flaky test");
        let result = 2 + 2;
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(result, 4);
    }

    #[test]
    fn result_based_test_ok() -> Result<String, std::io::Error> {
        println!("Print from succeeding result based test");
        Ok("Success".to_string())
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
    fn result_based_test_err() -> Result<String, CustomError> {
        println!("Print from failing result based test");
        Err(CustomError)
    }
}
