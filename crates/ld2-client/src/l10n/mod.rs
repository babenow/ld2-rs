//! Локализация UI: таблицы `ru`/`en` с первого коммита (решение D8, ADR-003).
//!
//! Согласованность языков гарантируется компилятором: `t()` — полный `match`
//! по `Key` в каждом языковом модуле, пропуск ключа = ошибка сборки.
//! Игровые тексты (бойлог, описания локаций) приходят с сервера и не локализуются.

pub mod en;
pub mod ru;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub fn from_code(code: &str) -> Self {
        match code {
            "en" => Self::En,
            _ => Self::Ru,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Ru => Self::En,
            Self::En => Self::Ru,
        }
    }
}

/// Строковые ключи UI (M0-шелл + M2: аккаунт/персонажи).
#[derive(Debug, Clone, Copy)]
pub enum Key {
    AppTitle,
    Subtitle,
    Exit,
    Language,
    Host,
    Version,
    ConfigPath,
    DataPath,
    M0Title,
    M0Hint,
    // M2: вход (ФР-01)
    LoginTitle,
    FieldLogin,
    FieldPassword,
    BtnLogin,
    BtnLogout,
    StatusConnecting,
    StatusOnline,
    StatusReconnecting,
    StatusAuthExpired,
    StatusStopped,
    ErrFillFields,
    // M2: персонажи (ФР-02)
    CharactersTitle,
    SlotsUsed,
    SlotsFull,
    BtnEnter,
    BtnCreate,
    BtnDelete,
    BtnSwitchChar,
    CreateTitle,
    FieldCharName,
    NameTooShort,
    ProfileLabel,
    ProfileWarrior,
    ProfileRogue,
    ProfileMage,
    ProfileHint,
    DeleteTitle,
    DeleteConfirm,
    Loading,
    GamePlaceholder,
    LocationLabel,
    HpLabel,
    MpLabel,
    XpPoolLabel,
    // M3: мир (ФР-03, ФР-17)
    ExitsLabel,
    PlayersLabel,
    MonstersLabel,
    NpcsLabel,
    CorpsesLabel,
    HistoryLabel,
    LogLabel,
    Examine,
    Attack,
    EntityDetailTitle,
    AltNavHint,
    LootLabel,
    NoExits,
    CorpsePlayerHint,
    // M4: лист/инвентарь/смерть (ФР-04, ФР-06, ФР-07)
    SheetSkills,
    SheetAttrs,
    SheetInventory,
    MaxLabel,
    EquipLabel,
    EmptySlot,
    MenuUse,
    MenuEquip,
    MenuUnequip,
    MenuDrop,
    MenuDesc,
    DeathTitle,
    DeathLost,
    DeathConfirm,
    ItemDescTitle,
    ReqLabel,
}

pub fn t(lang: Lang, key: Key) -> &'static str {
    match lang {
        Lang::Ru => ru::t(key),
        Lang::En => en::t(key),
    }
}

/// Строка с подстановкой плейсхолдеров `{name}`:
/// `tf(lang, Key::SlotsUsed, &[("used", "2"), ("limit", "8")])`.
pub fn tf(lang: Lang, key: Key, subs: &[(&str, &str)]) -> String {
    let mut s = t(lang, key).to_string();
    for (name, value) in subs {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KEYS: &[Key] = &[
        Key::AppTitle,
        Key::Subtitle,
        Key::Exit,
        Key::Language,
        Key::Host,
        Key::Version,
        Key::ConfigPath,
        Key::DataPath,
        Key::M0Title,
        Key::M0Hint,
        Key::LoginTitle,
        Key::FieldLogin,
        Key::FieldPassword,
        Key::BtnLogin,
        Key::BtnLogout,
        Key::StatusConnecting,
        Key::StatusOnline,
        Key::StatusReconnecting,
        Key::StatusAuthExpired,
        Key::StatusStopped,
        Key::ErrFillFields,
        Key::CharactersTitle,
        Key::SlotsUsed,
        Key::SlotsFull,
        Key::BtnEnter,
        Key::BtnCreate,
        Key::BtnDelete,
        Key::BtnSwitchChar,
        Key::CreateTitle,
        Key::FieldCharName,
        Key::NameTooShort,
        Key::ProfileLabel,
        Key::ProfileWarrior,
        Key::ProfileRogue,
        Key::ProfileMage,
        Key::ProfileHint,
        Key::DeleteTitle,
        Key::DeleteConfirm,
        Key::Loading,
        Key::GamePlaceholder,
        Key::LocationLabel,
        Key::HpLabel,
        Key::MpLabel,
        Key::XpPoolLabel,
        Key::ExitsLabel,
        Key::PlayersLabel,
        Key::MonstersLabel,
        Key::NpcsLabel,
        Key::CorpsesLabel,
        Key::HistoryLabel,
        Key::LogLabel,
        Key::Examine,
        Key::Attack,
        Key::EntityDetailTitle,
        Key::AltNavHint,
        Key::LootLabel,
        Key::NoExits,
        Key::CorpsePlayerHint,
        Key::SheetSkills,
        Key::SheetAttrs,
        Key::SheetInventory,
        Key::MaxLabel,
        Key::EquipLabel,
        Key::EmptySlot,
        Key::MenuUse,
        Key::MenuEquip,
        Key::MenuUnequip,
        Key::MenuDrop,
        Key::MenuDesc,
        Key::DeathTitle,
        Key::DeathLost,
        Key::DeathConfirm,
        Key::ItemDescTitle,
        Key::ReqLabel,
    ];

    #[test]
    fn every_key_translated_on_every_lang() {
        for &key in ALL_KEYS {
            assert!(!t(Lang::Ru, key).is_empty(), "пустой RU: {key:?}");
            assert!(!t(Lang::En, key).is_empty(), "пустой EN: {key:?}");
        }
    }

    #[test]
    fn placeholders_consistent_between_langs() {
        // Плейсхолдеры должны совпадать в обоих языках — иначе format сломается.
        fn braces(s: &str) -> Vec<&str> {
            let mut found: Vec<&str> = s
                .split('{')
                .skip(1)
                .filter_map(|part| part.split('}').next())
                .collect();
            found.sort_unstable();
            found
        }
        for &key in ALL_KEYS {
            assert_eq!(
                braces(t(Lang::Ru, key)),
                braces(t(Lang::En, key)),
                "плейсхолдеры {key:?} расходятся"
            );
        }
    }

    #[test]
    fn tf_substitutes_placeholders() {
        let s = tf(Lang::Ru, Key::SlotsUsed, &[("used", "2"), ("limit", "8")]);
        assert_eq!(s, "2 из 8 слотов");
        let s = tf(Lang::Ru, Key::DeleteConfirm, &[("name", "Ким")]);
        assert!(
            s.contains("Ким") && !s.contains('{'),
            "плейсхолдер подставлен: {s}"
        );
    }

    #[test]
    fn lang_codes_round_trip() {
        assert_eq!(Lang::from_code("ru"), Lang::Ru);
        assert_eq!(Lang::from_code("en"), Lang::En);
        assert_eq!(Lang::from_code("de"), Lang::Ru, "неизвестный код → ru");
        assert_eq!(Lang::Ru.toggle(), Lang::En);
        assert_eq!(Lang::En.code(), "en");
    }
}
