use std::fmt;
use std::io::{IsTerminal, Write};

/// Tracks whether any supported terminal is detected for OSC 9;4 progress reporting.
///
/// When enabled, emits ConEmu-style OSC 9;4 escape sequences to stderr to show
/// progress in the terminal's title/tab bar. Supported by Ghostty, WezTerm,
/// Windows Terminal, ConEmu, and iTerm2 (3.6.6+).
///
/// When disabled (unsupported terminal or non-interactive stderr), all methods
/// are silent no-ops.
pub(crate) struct TermProgress {
    enabled: bool,
    has_failure: bool,
}

enum ProgressState {
    Remove,
    Value(f64),
    Error(f64),
}

impl fmt::Display for ProgressState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (state, progress) = match self {
            Self::Remove => (0, 0.0),
            Self::Value(v) => (1, *v),
            Self::Error(v) => (2, *v),
        };
        write!(f, "\x1b]9;4;{state};{progress:.0}\x1b\\")
    }
}

impl TermProgress {
    pub fn new() -> Self {
        Self {
            enabled: supports_osc_9_4(),
            has_failure: false,
        }
    }

    pub fn update(&self, completed: usize, total: usize) {
        if !self.enabled {
            return;
        }
        let pct = if total == 0 {
            0.0
        } else {
            (completed as f64 / total as f64) * 100.0
        };
        let state = if self.has_failure {
            ProgressState::Error(pct)
        } else {
            ProgressState::Value(pct)
        };
        let _ = write!(std::io::stderr(), "{state}");
    }

    pub fn mark_failure(&mut self) {
        self.has_failure = true;
    }

    pub fn clear(&self) {
        if !self.enabled {
            return;
        }
        let _ = write!(std::io::stderr(), "{}", ProgressState::Remove);
    }
}

impl Drop for TermProgress {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Detect terminals known to support OSC 9;4 progress sequences.
fn supports_osc_9_4() -> bool {
    if !std::io::stderr().is_terminal() {
        return false;
    }

    let windows_terminal = std::env::var("WT_SESSION").is_ok();
    let conemu = std::env::var("ConEmuANSI").ok() == Some("ON".into());
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let wezterm = term_program.as_deref() == Some("WezTerm");
    let ghostty = term_program.as_deref() == Some("ghostty");
    let iterm = term_program.as_deref() == Some("iTerm.app")
        && std::env::var("TERM_FEATURES")
            .ok()
            .is_some_and(|v| term_features_has_progress(&v));

    windows_terminal || conemu || wezterm || ghostty || iterm
}

/// Check if iTerm2's TERM_FEATURES contains the "P" (progress) capability.
fn term_features_has_progress(value: &str) -> bool {
    let mut current = String::new();
    for ch in value.chars() {
        if !ch.is_ascii_alphanumeric() {
            break;
        }
        if ch.is_ascii_uppercase() {
            if current == "P" {
                return true;
            }
            current.clear();
            current.push(ch);
        } else {
            current.push(ch);
        }
    }
    current == "P"
}
