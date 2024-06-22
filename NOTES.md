# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9

# Todo
- Support quiet (same as `format==terse`)
- Support color (with `anstream`)
- Support test-threads setting
- Shared, type based dependency injection
- Sequential/parallel constraints on modules - generating static initializer registering info
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
- Support property based testing
- Provide a nicer assertion macro
- Flaky/non-flaky attributes
- Capture/no-capture controlled by attributes
- Prettier pretty output
