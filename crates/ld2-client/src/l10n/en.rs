//! English UI strings (P1 — полный перевод на M9, ADR-003 D8).

use super::Key;

pub(super) fn t(key: Key) -> &'static str {
    use Key::*;
    match key {
        AppTitle => "ld2",
        Subtitle => "text-based online MMORPG",
        Exit => "Exit",
        Language => "Language",
        Host => "Server",
        Version => "Version",
        ConfigPath => "Config",
        DataPath => "Data",
        M0Title => "M0 skeleton — window is up",
        M0Hint => "Next: M1 — protocol contract, transport and mock server (PLAN-M0.md)",
    }
}
