test_r::enable!();

#[cfg(test)]
mod tests {
    use test_r::test;
    use tokio::io::AsyncWriteExt;

    #[test]
    async fn it_does_work() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'it_does_work'\n")
            .await
            .unwrap();
        let result = 2 + 2;
        assert_eq!(result, 5);
    }

    #[test]
    async fn this_too() {
        let _ = tokio::io::stdout()
            .write(b"Print from 'this_too'\n")
            .await
            .unwrap();
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}

mod inner {
    #[cfg(test)]
    mod tests {
        use test_r::test;
        use tokio::io::AsyncWriteExt;

        #[test]
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
    }
}
