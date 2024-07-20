# Links
https://rustc-dev-guide.rust-lang.org/test-implementation.html
https://github.com/LukasKalbertodt/libtest-mimic/
https://github.com/LukasKalbertodt/libtest-mimic/issues/9

# Todo
- Dynamic test generation (annotation for generator function)
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
- Provide a nicer assertion macro
- Flaky/non-flaky attributes
- Capture/no-capture controlled by attributes
- Prettier pretty output
- Support tests returning `Result<>` 
- Make sure `#[tracing::instrument]` works
- Tags
- 