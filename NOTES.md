# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9
https://github.com/rust-lang/rust/tree/master/library/test

# Todo
- Support `--logfile PATH`
- Support `--report-time`
- Support `--report-time-format`
- Support `#[timeout(duration)]`
- Support `--shuffle`
- Support `--shuffle-seed`
- Support property based testing (with an existing library) (https://crates.io/crates/proptest or https://crates.io/crates/quickcheck)
- Flaky/non-flaky attributes
- Capture/no-capture controlled by attributes
- Support tests returning `Result<>` 
- Tags
- Make sure `#[tracing::instrument]` works
- Provide a nicer assertion macro (or at least recommend a 3rd party that works well)
- Make sure some 3rd party golden testing framework works, or add our own (https://crates.io/crates/goldenfile)
- CI
- Initial documentation
- Tests for tests
- Align terse output more with https://github.com/rust-lang/rust/blob/master/library/test/src/formatters/terse.rs
- How does it work together with criterion? Just ignore the test-r bench macro for those?

Later:
- Support `#[should_panic]` in dynamic tests
- More detailed benchmark stats output 
- Prettier pretty output
- 