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
        // M2: login
        LoginTitle => "Account login",
        FieldLogin => "Login",
        FieldPassword => "Password",
        BtnLogin => "Sign in",
        BtnLogout => "Log out",
        StatusConnecting => "Connecting…",
        StatusOnline => "Online",
        StatusReconnecting => "Reconnecting…",
        StatusAuthExpired => "Session expired",
        StatusStopped => "Disconnected",
        ErrFillFields => "Enter login and password",
        // M2: characters
        CharactersTitle => "Characters",
        SlotsUsed => "{used} of {limit} slots",
        SlotsFull => "All slots are used",
        BtnEnter => "Enter world",
        BtnCreate => "Create character",
        BtnDelete => "Delete",
        BtnSwitchChar => "Switch character",
        CreateTitle => "New character",
        FieldCharName => "Name",
        NameTooShort => "Name: at least 2 characters",
        ProfileLabel => "Profile",
        ProfileWarrior => "Warrior",
        ProfileRogue => "Rogue",
        ProfileMage => "Mage",
        ProfileHint => "Starting skills (server-defined)",
        DeleteTitle => "Delete character",
        DeleteConfirm => "Delete \"{name}\" forever? This cannot be undone.",
        Loading => "Loading…",
        GamePlaceholder => "Game screen — world panels arrive in M3",
        LocationLabel => "Location",
        HpLabel => "Health",
        MpLabel => "Mana",
        XpPoolLabel => "Experience points",
    }
}
