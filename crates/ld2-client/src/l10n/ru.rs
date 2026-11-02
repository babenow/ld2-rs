//! Русские строки UI (основной язык, P0).

use super::Key;

pub(super) fn t(key: Key) -> &'static str {
    use Key::*;
    match key {
        AppTitle => "ld2",
        Subtitle => "текстовая онлайн-MMORPG",
        Exit => "Выход",
        Language => "Язык",
        Host => "Сервер",
        Version => "Версия",
        ConfigPath => "Конфиг",
        DataPath => "Данные",
        M0Title => "Каркас M0 — окно поднято",
        M0Hint => "Дальше: M1 — контракт протокола, транспорт и мок-сервер (PLAN-M0.md)",
    }
}
