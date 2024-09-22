# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9
https://github.com/rust-lang/rust/tree/master/library/test

# Todo
- Try to capture output and support no-capture (`--nocapture`, `--show-output`)
   - we need to fork child processes for this
   - must respect thread-count setting
   - must respect shared dependencies (can't capture if there are top-level shared dependencies and threads>1)
- Measure total and per-test execution time
- Support `#[should_panic]`
- Support `#[bench]`
- Support `--exclude-should-panic`
- Support `--logfile PATH`
- Support `--report-time`
- Support `--report-time-format`
- Support `#[timeout(duration)]`
- Support `-shuffle`
- Support `--shuffle-seed`
- Support property based testing (with an existing library)
- Flaky/non-flaky attributes
- Capture/no-capture controlled by attributes
- Support tests returning `Result<>` 
- Tags
- Make sure `#[tracing::instrument]` works
- Provide a nicer assertion macro (or at least recommend a 3rd party that works well)
- Make sure some 3rd party golden testing framework works, or add our own
- Prettier pretty output
