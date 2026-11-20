# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- [**breaking**] Prebuilt runs stop processes left behind by each test binary; pass `--keep-leftovers` to keep them.
- [**breaking**] `-p`/`--exclude` match Cargo package names instead of directory names, and `--exclude` also applies without `-p`.
- [**breaking**] Prebuilt binaries run in their own process group: a test that reads from the terminal is stopped (SIGTTIN), and Ctrl+Z or Ctrl+\ reach cargo-test-r instead of the test.

## [2.2.11](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.10...cargo-test-r-v2.2.11) - 2026-11-17

### Other

- Add --retry-failed and cargo test-r run --binaries-metadata; support panic-only bodies on Rust 1.100 ([#213](https://github.com/vigoo/test-r/pull/213))

## [2.2.10](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.9...cargo-test-r-v2.2.10) - 2026-11-05

### Other

- update Cargo.lock dependencies

## [2.2.9](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.8...cargo-test-r-v2.2.9) - 2026-10-13

### Other

- update Cargo.lock dependencies

## [2.2.8](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.7...cargo-test-r-v2.2.8) - 2026-07-01

### Other

- Auto-derived case tags and matrix-suite support ([#181](https://github.com/vigoo/test-r/pull/181))

## [2.2.7](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.6...cargo-test-r-v2.2.7) - 2026-05-27

### Other

- update Cargo.lock dependencies

## [2.2.6](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.5...cargo-test-r-v2.2.6) - 2026-05-26

### Other

- update Cargo.lock dependencies

## [2.2.5](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.4...cargo-test-r-v2.2.5) - 2026-04-21

### Other

- update Cargo.lock dependencies

## [2.2.4](https://github.com/vigoo/test-r/compare/cargo-test-r-v2.2.3...cargo-test-r-v2.2.4) - 2026-03-04

### Other

- cargo-test-r binary release ([#161](https://github.com/vigoo/test-r/pull/161))
