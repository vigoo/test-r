# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9
https://github.com/rust-lang/rust/tree/master/library/test
https://github.com/rust-lang/rust/issues/105424

# Todo
- Flaky/non-flaky attributes
- Capture/no-capture controlled by attributes
- Support tests returning `Result<>` 
- Tags
- Make sure some 3rd party golden testing framework works, or add our own (https://crates.io/crates/goldenfile)
- Support property based testing (with an existing library) (https://crates.io/crates/proptest or https://crates.io/crates/quickcheck)
- How does it work together with criterion? Just ignore the test-r bench macro for those?
- Check if we can do some trick to run doctests with test-r (#cfg(doctest) imports?)

Ready to integrate, before release:
- Initial documentation
- Tests for tests
- Provide a nicer assertion macro (or at least recommend a 3rd party that works well)
- Align terse output more with https://github.com/rust-lang/rust/blob/master/library/test/src/formatters/terse.rs
- Code cleanup / remove all TODOs

Later:
- per-test report/ensure-time with attributes
- Support `#[should_panic]` in dynamic tests
- Support timeout, ensure-time, and report-time in dynamic tests
- More detailed benchmark stats output 
- Prettier pretty output
