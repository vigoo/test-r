//! Exercise the public macros without running the registered async tests.
#![cfg(feature = "tokio")]

use std::time::Duration;
use test_r::core::internal::{
    REGISTERED_TESTS, REGISTERED_TESTSUITE_PROPS, RegisteredTestSuiteProperty,
};

macro_rules! timeout_case {
    ($name:ident, $duration:literal) => {
        #[test_r::test]
        #[test_r::timeout($duration)]
        async fn $name() {}
    };
}

timeout_case!(compound, "2h37min4.125s");
timeout_case!(whitespace, " 12 min 3.5 sec ");
timeout_case!(submillisecond, "1millis 500usec 163µs 17nanos");
timeout_case!(minute, "1m");
timeout_case!(month, "1M");
timeout_case!(fractional_month, "0.1months");
timeout_case!(fractional_year, "0.25yrs");
timeout_case!(zero, "0");
timeout_case!(integer, 1234);
timeout_case!(leading_fraction, ".5s");
timeout_case!(exponent, "1e3ms");
timeout_case!(bare_seconds, "2");
timeout_case!(millisecond_limit, "18446744073709551615ms");

#[test_r::timeout("1wk 2days 3hrs 4mins 5secs 6msec 7us 8ns")]
mod inline_suite {}

mod external_suite {}
test_r::timeout_suite!(external_suite, "0.5year 0.5month");

#[test]
fn timeout_values_use_configured_fundu_syntax() {
    let tests = REGISTERED_TESTS.lock().unwrap();
    for (name, expected) in [
        ("compound", 9_424_125),
        ("whitespace", 723_500),
        ("submillisecond", 1),
        ("minute", 60_000),
        ("month", 2_630_016_000),
        ("fractional_month", 263_001_600),
        ("fractional_year", 7_889_400_000),
        ("zero", 0),
        ("integer", 1234),
        ("leading_fraction", 500),
        ("exponent", 1000),
        ("bare_seconds", 2000),
        ("millisecond_limit", u64::MAX),
    ] {
        let registered = tests.iter().find(|test| test.name == name).unwrap();
        assert_eq!(
            registered.props.timeout,
            Some(Duration::from_millis(expected)),
            "{name}"
        );
    }
    let suites = REGISTERED_TESTSUITE_PROPS.lock().unwrap();
    for (name, expected) in [
        ("inline_suite", 788_645_006),
        ("external_suite", 17_093_808_000),
    ] {
        assert!(
            suites.iter().any(|prop| matches!(prop,
                RegisteredTestSuiteProperty::Timeout { name: suite, timeout, .. }
                    if suite == name && *timeout == Duration::from_millis(expected)
            )),
            "{name}"
        );
    }
}

#[test]
fn malformed_and_overflowing_timeouts_are_rejected() {
    let invalid = [
        "",
        "-1s",
        "1?s",
        "inf",
        "1fortnight",
        "18446744073709551616ms",
        "18446744073709551616s",
    ];
    let fixture =
        std::env::temp_dir().join(format!("test-r-timeout-syntax-{}", std::process::id()));
    std::fs::create_dir_all(fixture.join("src")).unwrap();
    std::fs::write(
        fixture.join("Cargo.toml"),
        format!(
            "[package]\nname = \"timeout-syntax-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             [workspace]\n[dependencies]\ntest-r = {{ path = {:?} }}\n",
            env!("CARGO_MANIFEST_DIR")
        ),
    ).unwrap();
    let mut source = String::new();
    for (index, input) in invalid.iter().enumerate() {
        source.push_str(&format!(
            "#[test_r::test]\n#[test_r::timeout({input:?})]\nasync fn case_{index}() {{}}\n\
             #[test_r::timeout({input:?})]\nmod inline_{index} {{}}\n\
             mod external_{index} {{}}\ntest_r::timeout_suite!(external_{index}, {input:?});\n"
        ));
    }
    std::fs::write(fixture.join("src/lib.rs"), source).unwrap();
    let output = std::process::Command::new("cargo")
        .args([
            "check",
            "--tests",
            "--offline",
            "--message-format=json",
            "--manifest-path",
        ])
        .arg(fixture.join("Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            format!("{}/../target", env!("CARGO_MANIFEST_DIR")),
        )
        .output()
        .unwrap();
    std::fs::remove_dir_all(fixture).unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let rejections = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-message"
                && message["message"]["level"] == "error"
                && message["message"]["rendered"].as_str().is_some_and(|text| {
                    text.contains("timeout") && text.contains("human-readable duration string")
                })
        })
        .count();
    assert_eq!(
        rejections,
        invalid.len() * 3,
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
