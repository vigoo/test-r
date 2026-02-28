# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.2](https://github.com/vigoo/test-r/compare/test-r-v3.0.1...test-r-v3.0.2) - 2026-02-28

### Added

- support multiple filter arguments ([#156](https://github.com/vigoo/test-r/pull/156))

## [3.0.1](https://github.com/vigoo/test-r/compare/test-r-v3.0.0...test-r-v3.0.1) - 2026-02-26

### Added

- initial version of cargo-test-r cargo extensions ([#154](https://github.com/vigoo/test-r/pull/154))

## [3.0.0](https://github.com/vigoo/test-r/compare/test-r-v2.3.1...test-r-v3.0.0) - 2026-02-25

### Added

- not initializing unused test dependencies ([#152](https://github.com/vigoo/test-r/pull/152))
- suite level timeout ([#140](https://github.com/vigoo/test-r/pull/140))
- progress bar support ([#146](https://github.com/vigoo/test-r/pull/146))
- tagged dependency support in test_dep inputs ([#145](https://github.com/vigoo/test-r/pull/145))

### Fixed

- stderr capturing in json output mode ([#143](https://github.com/vigoo/test-r/pull/143))
- properly handle nested sequential suites ([#150](https://github.com/vigoo/test-r/pull/150))

### Other

- Adjust error formatting & hide internal panic traces ([#135](https://github.com/vigoo/test-r/pull/135))
- migrated to desert-rust ([#142](https://github.com/vigoo/test-r/pull/142))

## [2.3.1](https://github.com/vigoo/test-r/compare/test-r-v2.3.0...test-r-v2.3.1) - 2025-10-21

### Fixed

- CTRF related fixes ([#132](https://github.com/vigoo/test-r/pull/132))

## [2.3.0](https://github.com/vigoo/test-r/compare/test-r-v2.2.2...test-r-v2.3.0) - 2025-10-21

### Added

- Support CTRF output ([#130](https://github.com/vigoo/test-r/pull/130))

## [2.2.2](https://github.com/vigoo/test-r/compare/test-r-v2.2.1...test-r-v2.2.2) - 2025-08-27

### Other

- Use :# format for converting the error to panic ([#124](https://github.com/vigoo/test-r/pull/124))

## [2.2.1](https://github.com/vigoo/test-r/compare/test-r-v2.2.0...test-r-v2.2.1) - 2025-07-29

### Other

- Adds a new command line argument to retry whole runs a number of times ([#121](https://github.com/vigoo/test-r/pull/121))

## [2.2.0](https://github.com/vigoo/test-r/compare/test-r-v2.1.0...test-r-v2.2.0) - 2025-06-03

### Added

- Support for human-readable duration strings in #[timeout] ([#116](https://github.com/vigoo/test-r/pull/116))

### Other

- More robust test modifiers ([#114](https://github.com/vigoo/test-r/pull/114))
- Updated dependencies ([#110](https://github.com/vigoo/test-r/pull/110))
- Explicitly dropping the test execution before printing the test results ([#118](https://github.com/vigoo/test-r/pull/118))
- Rust Edition 2024 ([#98](https://github.com/vigoo/test-r/pull/98))
- Splitted the macro code into submodules ([#112](https://github.com/vigoo/test-r/pull/112))

## [2.1.0](https://github.com/vigoo/test-r/compare/test-r-v2.0.1...test-r-v2.1.0) - 2025-01-30

### Added

- writing intermediate junit reports after each test (#95)

### Fixed

- name collision in macro-generated code (#94)

## [2.0.1](https://github.com/vigoo/test-r/compare/test-r-v2.0.0...test-r-v2.0.1) - 2025-01-08

### Other

- Fixes ([#89](https://github.com/vigoo/test-r/pull/89))

## [2.0.0](https://github.com/vigoo/test-r/compare/test-r-v1.2.0...test-r-v2.0.0) - 2025-01-01

### Added

- Dependency tagging and test matrix (#86)
- Support for all test properties in dynamic test generators (#82)
- Per-test report/ensure time attributes (#80)
- --show-stats option (#84)

### Other

- Updated dependencies ([#78](https://github.com/vigoo/test-r/pull/78))

## [1.2.0](https://github.com/vigoo/test-r/compare/test-r-v1.1.0...test-r-v1.2.0) - 2024-12-13

### Added

- Print flakyness-related retries (#76)

## [1.1.0](https://github.com/vigoo/test-r/compare/test-r-v1.0.5...test-r-v1.1.0) - 2024-11-28

### Added

- Ability to mark a non-inline suite as sequential ([#71](https://github.com/vigoo/test-r/pull/71))

## [1.0.5](https://github.com/vigoo/test-r/compare/test-r-v1.0.4...test-r-v1.0.5) - 2024-11-27

### Fixed

- Fix an issue with dropping and recreating dependencies in some c… ([#69](https://github.com/vigoo/test-r/pull/69))
