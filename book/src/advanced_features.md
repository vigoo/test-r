# Advanced features

This chapter covers the advanced features of `test-r`, which are either not available at all using the built-in test harness,
or at least being unstable.

- [Dependency injection](./advanced_features/dependency_injection.md) allows sharing dependencies between tests.
- [Tags](./advanced_features/tags.md) allow grouping tests and running only a subset of them.
- [Benches](./advanced_features/benches.md) are used to measure the performance of functions.
- [Per-test configuration](./advanced_features/per_test_configuration.md) allows customizing the test execution from the code, instead of using command line options.
- [Flaky tests](./advanced_features/flaky_tests.md) can be either retried, or executed multiple times to verify they aren't flaky
- [Dynamic test generation](./advanced_features/dynamic_test_generation.md) allows creating new tests from code
