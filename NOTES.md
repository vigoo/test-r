# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9
https://github.com/rust-lang/rust/tree/master/library/test
https://github.com/rust-lang/rust/issues/105424

# Todo

Ready to integrate, before release:
- Initial documentation
- How does it work together with criterion? Just ignore the test-r bench macro for those?
- Tests for tests
- Provide a nicer assertion macro (or at least recommend a 3rd party that works well)
- Align terse output more with https://github.com/rust-lang/rust/blob/master/library/test/src/formatters/terse.rs
- Code cleanup / remove all TODOs
- Check if we can do some trick to run doctests with test-r (#cfg(doctest) imports?)
- Pretty output: align passed / reported time columns
- Dump worker stdout/err in case of ipc panic

Later:
- per-test report/ensure-time with attributes
- Support `#[should_panic]` in dynamic tests
- Support tags, always_capture, never_capture, flaky, non_flaky, timeout, ensure-time, and report-time in dynamic tests
- More detailed benchmark stats output 
- Prettier pretty output
