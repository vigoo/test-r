test_r::enable!();

mod other;
mod sharing;

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

#[cfg(test)]
mod matrix_suite_generic_type_matching_repro {
    use std::marker::PhantomData;
    use test_r::test_dep;

    pub struct Primary;
    pub struct Secondary;

    pub struct Wrapped<T> {
        value: &'static str,
        _marker: PhantomData<T>,
    }

    #[test_dep(tagged_as = "primary")]
    fn create_primary() -> Wrapped<Primary> {
        Wrapped {
            value: "primary",
            _marker: PhantomData,
        }
    }

    #[test_r::matrix_suite(kind, Wrapped<Primary>)]
    mod suite {
        use super::{Primary, Secondary, Wrapped};
        use std::marker::PhantomData;
        use test_r::{define_matrix_dimension, test, test_dep};

        test_r::inherit_test_dep!(
            #[tagged_as("primary")]
            Wrapped<Primary>
        );

        define_matrix_dimension!(kind: Wrapped<Primary> -> "primary");

        #[test_dep]
        fn create_secondary() -> Wrapped<Secondary> {
            Wrapped {
                value: "secondary",
                _marker: PhantomData,
            }
        }

        #[test]
        fn secondary_dependency_is_not_part_of_primary_matrix(dep: &Wrapped<Secondary>) {
            assert_eq!(dep.value, "secondary");
        }
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

// ---------------------------------------------------------------------------
// Feature 1 + Feature 2 end-to-end checks
// ---------------------------------------------------------------------------
//
// The matrix auto-tag feature (Feature 1) makes every matrix-generated test
// case carry a `<dim>_<case>` tag derived at `define_matrix_dimension!` time.
// `matrix_suite` (Feature 2) applies a dimension to every matching `#[test]`
// in a module without per-test `#[dimension]` annotations.
//
// We verify both by invoking the generated test-generator function (which the
// `#[test]` macro rewrites into `fn <name>() -> Vec<GeneratedTest>`) and
// inspecting the returned `GeneratedTest` entries' names and `props.tags`.
// This does not execute the test closures, so no dependency resolution runs.

#[cfg(test)]
mod matrix_features_e2e {
    use test_r::core::GeneratedTest;
    use test_r::{define_matrix_dimension, tag, test, test_dep};

    // The shared matrix dimension: a `DbDep` value per case.
    pub struct DbDep {
        pub flavor: &'static str,
    }

    #[test_dep(tagged_as = "postgres")]
    fn create_db_postgres() -> DbDep {
        DbDep { flavor: "postgres" }
    }

    #[test_dep(tagged_as = "sqlite")]
    fn create_db_sqlite() -> DbDep {
        DbDep { flavor: "sqlite" }
    }

    define_matrix_dimension!(db: DbDep -> "postgres", "sqlite");

    // ----- Feature 1: per-test `#[dimension]` carries the auto-tag -----

    /// A matrix test that also carries an explicit `#[tag(...)]`. The generated
    /// cases must keep the explicit tag AND gain the `db_<case>` auto-tags.
    #[test]
    #[tag(matrix_suite_e2e)]
    fn matrix_dep_test(#[dimension(db)] dep: &DbDep) {
        assert!(dep.flavor == "postgres" || dep.flavor == "sqlite");
    }

    /// Returns the generated cases for `matrix_dep_test` without running them.
    fn generated_cases() -> Vec<GeneratedTest> {
        // `matrix_dep_test()` is the test-generator function produced by the
        // `#[test]` + `#[dimension]` expansion; calling it returns the
        // per-case `GeneratedTest` entries.
        matrix_dep_test()
    }

    #[test]
    fn matrix_cases_have_dim_case_auto_tags() {
        let cases = generated_cases();
        assert_eq!(
            cases.len(),
            2,
            "expected exactly 2 matrix cases (postgres, sqlite)"
        );

        let by_name: std::collections::HashMap<String, GeneratedTest> =
            cases.into_iter().map(|t| (t.name.clone(), t)).collect();
        assert_eq!(by_name.len(), 2, "case names must be distinct");

        let pg = by_name
            .get("matrix_dep_test_postgres")
            .expect("postgres case name should be matrix_dep_test_postgres");
        let sql = by_name
            .get("matrix_dep_test_sqlite")
            .expect("sqlite case name should be matrix_dep_test_sqlite");

        // Feature 1: each case carries the `<dim>_<case>` auto-tag, alongside
        // the explicit `#[tag(...)]` already on the test.
        assert!(
            pg.props.tags.contains(&"db_postgres".to_string()),
            "postgres case must carry the db_postgres auto-tag, got {:?}",
            pg.props.tags
        );
        assert!(
            sql.props.tags.contains(&"db_sqlite".to_string()),
            "sqlite case must carry the db_sqlite auto-tag, got {:?}",
            sql.props.tags
        );
        // Explicit tag is preserved (not replaced) on every case.
        for case in [pg, sql] {
            assert!(
                case.props.tags.contains(&"matrix_suite_e2e".to_string()),
                "explicit #[tag(matrix_suite_e2e)] must be preserved, got {:?}",
                case.props.tags
            );
        }
        // The non-matching auto-tag is NOT smeared onto the wrong case.
        assert!(
            !pg.props.tags.contains(&"db_sqlite".to_string()),
            "postgres case must not carry db_sqlite, got {:?}",
            pg.props.tags
        );
    }

    #[test]
    fn matrix_cases_have_tagged_dependency_names() {
        let cases = generated_cases();
        for case in &cases {
            let deps = case
                .dependencies
                .as_ref()
                .expect("matrix case should declare its dependency");
            assert!(
                deps.iter()
                    .any(|d| d == "dbdep_postgres" || d == "dbdep_sqlite"),
                "case `{}` should depend on a tagged dbdep variant, got {:?}",
                case.name,
                deps
            );
        }
    }

    // ----- Feature 2: `#[matrix_suite]` rewrites the whole module -----

    /// `#[matrix_suite(db, DbDep)]` injects `#[dimension(db)]` onto every
    /// `&DbDep` parameter of each `#[test]` in this module before the inner
    /// `#[test]` macro expands. Tests that do not take `&DbDep` are left alone.
    #[cfg(test)]
    #[test_r::matrix_suite(db, DbDep)]
    mod matrix_suite_example {
        use super::DbDep;
        use test_r::{define_matrix_dimension, tag, test};

        // The dimension's tagged deps live in the parent module; inherit them
        // so the per-case getters resolve in this child module.
        test_r::inherit_test_dep!(
            #[tagged_as("postgres")]
            DbDep
        );
        test_r::inherit_test_dep!(
            #[tagged_as("sqlite")]
            DbDep
        );

        // `define_matrix_dimension!` emits a module-local `test_r_get_dep_tags_db`
        // helper. The injected `#[dimension(db)]` (added by `matrix_suite`)
        // expands to a call to that helper, so it must be defined in the same
        // module as the rewritten tests — not just in a parent.
        define_matrix_dimension!(db: DbDep -> "postgres", "sqlite");

        #[test]
        fn thing_one(deps: &DbDep) {
            assert!(deps.flavor == "postgres" || deps.flavor == "sqlite");
        }

        #[test]
        #[tag(suite_explicit)]
        fn thing_two(deps: &DbDep) {
            assert!(deps.flavor == "postgres" || deps.flavor == "sqlite");
        }

        /// A helper that is NOT a `#[test]` — must be left untouched.
        fn helper_not_a_test(deps: &DbDep) -> bool {
            deps.flavor == "postgres"
        }

        /// A `#[test]` that takes no `&DbDep` parameter — runs exactly once,
        /// not matrix-expanded.
        #[test]
        fn no_dep_test() {
            // Use a runtime value so clippy's `eq_op` does not flag this as a
            // constant-equal assertion. The point of this test is just that it
            // runs exactly once (not matrix-expanded).
            let expected = 4;
            assert_eq!(2 + 2, expected);
        }

        #[test]
        fn matrix_suite_expands_every_matching_test() {
            // `thing_one` and `thing_two` were rewritten by `matrix_suite` to
            // carry `#[dimension(db)]`, so their generator functions produce 2
            // cases each. `no_dep_test` was not rewritten; it stays a single
            // plain test (no `no_dep_test()` generator fn exists).
            let thing_one_cases = thing_one();
            let thing_two_cases = thing_two();

            assert_eq!(
                thing_one_cases.len(),
                2,
                "thing_one should be matrix-expanded into 2 cases"
            );
            assert_eq!(
                thing_two_cases.len(),
                2,
                "thing_two should be matrix-expanded into 2 cases"
            );

            let mut pg_count = 0;
            let mut sqlite_count = 0;
            for case in thing_one_cases.iter().chain(thing_two_cases.iter()) {
                assert!(
                    case.props.tags.contains(&"db_postgres".to_string())
                        || case.props.tags.contains(&"db_sqlite".to_string()),
                    "case `{}` should carry a db_<case> auto-tag, got {:?}",
                    case.name,
                    case.props.tags
                );
                if case.props.tags.contains(&"db_postgres".to_string()) {
                    pg_count += 1;
                }
                if case.props.tags.contains(&"db_sqlite".to_string()) {
                    sqlite_count += 1;
                }
            }
            assert_eq!(pg_count, 2, "two postgres cases expected (one per test)");
            assert_eq!(sqlite_count, 2, "two sqlite cases expected (one per test)");

            // Explicit `#[tag(suite_explicit)]` on thing_two is preserved.
            for case in &thing_two_cases {
                assert!(
                    case.props.tags.contains(&"suite_explicit".to_string()),
                    "explicit tag on thing_two must be preserved, got {:?}",
                    case.props.tags
                );
            }

            // The helper is a plain function callable directly (untouched).
            assert!(helper_not_a_test(&DbDep { flavor: "postgres" }));
        }
    }
}
