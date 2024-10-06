#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    fn other_module_test_works() {
        println!("Print from other module's test");
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
