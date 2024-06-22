# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/

# Todo
- Try to capture output and support no-capture (`--nocapture`, `--show-output`)
- Support quiet (same as `format==terse`)
- Support color
- Support test-threads setting
- Shared, type based dependency injection
- Sequential/parallel constraints on modules - generating static initializer registering info
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
