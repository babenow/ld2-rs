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

/// Строковые ключи UI (только те, что есть в M0-шелле).
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
}

pub fn t(lang: Lang, key: Key) -> &'static str {
    match lang {
        Lang::Ru => ru::t(key),
        Lang::En => en::t(key),
    }
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
    ];

    #[test]
    fn every_key_translated_on_every_lang() {
        for &key in ALL_KEYS {
            assert!(!t(Lang::Ru, key).is_empty(), "пустой RU: {key:?}");
            assert!(!t(Lang::En, key).is_empty(), "пустой EN: {key:?}");
        }
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
