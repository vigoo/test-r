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
    use test_r::sequential;

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
        async fn sleeping_test_2() {
            let _ = tokio::io::stdout()
                .write(b"Print from sleeping test 2\n")
                .await
                .unwrap();
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let result = 2 + 2;
            assert_eq!(result, 4);
        }
    }
}

mod deps {
    use test_r::{sequential, test, test_dep};

    struct Dep1 {
        value: i32,
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
    mod tests {
        use crate::deps::Dep1;
        use std::sync::Arc;
        use test_r::{sequential, test, test_dep};
        use tokio::io::AsyncWriteExt;

        #[test_dep]
        fn create_dep1() -> Dep1 {
            Dep1::new(10)
        }

        #[test_dep]
        async fn create_dep2() -> Dep2 {
            Dep2::new(20).await
        }

        struct Dep2 {
            value: i32,
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
            use std::sync::Arc;
            use test_r::{inherit_test_dep, test, test_dep};

            inherit_test_dep!(Dep1);

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
            async fn dep_test_inner_works_1(dep1: &Dep1) {
                println!("Print from dep test inner 1");
                assert_eq!(dep1.value, 10);
            }

            #[test]
            async fn dep_test_inner_works_2(dep2: &Dep2) {
                println!("Print from dep test inner 2");
                assert_eq!(dep2.value, 210);
            }

            #[test]
            async fn dep_test_inner_works_3(dep3: &Dep3) {
                println!("Print from dep test inner 3");
                assert_eq!(dep3.value, 240);
            }
        }
    }
}
