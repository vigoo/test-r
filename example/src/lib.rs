test_r::enable!();

#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    fn it_does_work() {
        println!("Print from 'it_does_work'");
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    fn this_too() {
        println!("Print from 'this_too'");
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
}
