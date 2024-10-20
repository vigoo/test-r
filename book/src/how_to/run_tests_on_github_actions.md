# GitHub Actions with JUnit

With `test-r` it is easy to generate JUnit test reports when running the tests on CI. Then the generated XMLs can be parsed by another GitHub Action step to provide a nicer test report in the GitHub UI.

The following example shows how to run the tests with `test-r` and generate JUnit XMLs:

```shell
cargo test -- --format junit --logfile target/report.xml
```

This will generate one or more JUnit XML files in the `target` directory.

The [action-junit-report](https://github.com/mikepenz/action-junit-report) action can be used to parse the generated XMLs and show the results in the GitHub UI. The following example shows how to use it:

```yaml
  - name: Publish Test Report
    uses: mikepenz/action-junit-report@v4
    if: success() || failure() # always run even if the previous step fails
    with:
      report_paths: '**/target/report-*.xml'
      detailed_summary: true
      include_passed: true
```
