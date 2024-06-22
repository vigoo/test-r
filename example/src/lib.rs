test_r::enable!();

#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    fn it_does_work() {
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    fn this_too() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}

mod inner {
    #[cfg(test)]
    mod tests {
        use test_r::test;

        #[test]
        fn inner_test_works() {
            let result = 2 + 2;
            assert_eq!(result, 4);
        }
    }
}