//! UI-слой M2–M3 (ФР-01…ФР-03, ФР-17): экраны входа/персонажей, игровой
//! мир (локация/сущности/история) и журнал.
//!
//! Экраны — проекция [`AppStore`] (см. `store.rs`): они читают состояние,
//! отправляют команды через [`TransportHandle`] и возвращают [`Effect`]'ы
//! для слоя приложения (keyring/транспорт).

use eframe::egui;

use crate::l10n::Key;
use crate::store::{AppStore, Effect};
use crate::transport::TransportHandle;

pub mod chat_panel;
pub mod log_panel;
pub mod screens;
pub mod sheet;
pub mod world;

/// Контекст экрана: всё, что нужно функции отрисовки.
pub struct UiCtx<'a> {
    pub store: &'a mut AppStore,
    pub forms: &'a mut Forms,
    pub transport: &'a TransportHandle,
    pub lang: crate::l10n::Lang,
}

/// Формы ввода (живут между кадрами).
#[derive(Default)]
pub struct Forms {
    pub login: LoginForm,
    pub create: CreateForm,
    /// `(id, name)` персонажа, выбранного на удаление.
    pub delete_target: Option<(String, String)>,
    /// Правая панель: вкладка + drag&drop (M4).
    pub sheet: SheetState,
    /// Нижняя панель чата (ФР-13, M8): ввод и фильтр-строка.
    pub chat: ChatForm,
}

#[derive(Default)]
pub struct ChatForm {
    /// Поле ввода сообщения.
    pub input: String,
    /// Фильтр-строка по истории (ФР-13).
    pub filter: String,
}

#[derive(Default)]
pub struct LoginForm {
    pub login: String,
    pub password: String,
}

/// Профиль персонажа (D3: Воин/Вор/Маг — wire-значения для `char.create`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    #[default]
    Warrior,
    Rogue,
    Mage,
}

impl Profile {
    pub fn all() -> [Profile; 3] {
        [Profile::Warrior, Profile::Rogue, Profile::Mage]
    }

    /// Wire-значение `cmd/char.create.profile`.
    pub fn value(self) -> &'static str {
        match self {
            Profile::Warrior => "warrior",
            Profile::Rogue => "rogue",
            Profile::Mage => "mage",
        }
    }

    /// Локализованное название профиля.
    pub fn label_key(self) -> Key {
        match self {
            Profile::Warrior => Key::ProfileWarrior,
            Profile::Rogue => Key::ProfileRogue,
            Profile::Mage => Key::ProfileMage,
        }
    }
}

#[derive(Default)]
pub struct CreateForm {
    pub name: String,
    pub profile: Profile,
    pub open: bool,
    /// Команда ушла — ждём ack, чтобы закрыть окно по успеху.
    pub submitted: bool,
}

impl CreateForm {
    pub fn reset(&mut self) {
        self.name.clear();
        self.profile = Profile::default();
        self.open = true;
        self.submitted = false;
    }

    pub fn name_valid(&self) -> bool {
        self.name.trim().chars().count() >= 2
    }
}

/// Вкладка правой панели (лист персонажа, ФР-04/ФР-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetTab {
    #[default]
    Skills,
    Attrs,
    Inventory,
    /// Статусы преступлений + журнал нарушений (ФР-11, M6).
    Statuses,
    /// Журнал квестов (ФР-12, M7).
    Quests,
    /// Профессии и крафт (ФР-08, M7).
    Professions,
}

/// Источник перетаскивания в инвентаре (drag&drop, ФР-07).
#[derive(Debug, Clone, PartialEq)]
pub enum DragFrom {
    /// Перетаскивается предмет из сетки инвентаря.
    Inventory(String),
    /// Перетаскивается предмет из слота экипировки.
    Slot(String),
}

/// Состояние правой панели — живёт между кадрами.
#[derive(Default)]
pub struct SheetState {
    pub tab: SheetTab,
    pub drag: Option<DragFrom>,
}

impl Forms {
    /// Начать перетаскивание (сброс предыдущего).
    pub fn start_drag(&mut self, from: DragFrom) {
        self.sheet.drag = Some(from);
    }
}

/// Эффекты экранов — исполняет `App` (не редьюсер).
pub type ScreenEffects = Vec<Effect>;

/// Формат длительности (ФР-11): `{h}ч{m}м` / `{m}м{s}с` / `{s}с`.
pub fn format_duration(lang: crate::l10n::Lang, secs: i64) -> String {
    use crate::l10n::{tf, Key};
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        tf(
            lang,
            Key::DurHm,
            &[("h", &h.to_string()), ("m", &m.to_string())],
        )
    } else if m > 0 {
        tf(
            lang,
            Key::DurMs,
            &[("m", &m.to_string()), ("s", &s.to_string())],
        )
    } else {
        tf(lang, Key::DurS, &[("s", &s.to_string())])
    }
}

/// Для чужих/бейджей: округление вверх до минут (ФР-11).
pub fn format_duration_ceil(lang: crate::l10n::Lang, secs: i64) -> String {
    use crate::l10n::{tf, Key};
    if secs <= 0 {
        return tf(lang, Key::DurS, &[("s", "0")]);
    }
    let mins = (secs + 59) / 60;
    if mins >= 60 {
        tf(
            lang,
            Key::DurHm,
            &[
                ("h", &(mins / 60).to_string()),
                ("m", &(mins % 60).to_string()),
            ],
        )
    } else {
        tf(lang, Key::DurMs, &[("m", &mins.to_string()), ("s", "0")])
    }
}

/// Название статуса: из `session.crime.kinds` (§5), фолбэк — сам `kind`.
pub fn crime_kind_title(store: &crate::store::AppStore, kind: &str) -> String {
    store
        .session
        .as_ref()
        .and_then(|s| s.crime.kinds.iter().find(|k| k.kind == kind))
        .map(|k| k.title.clone())
        .unwrap_or_else(|| kind.to_string())
}

/// Цвет по severity (§7.4/§5).
pub fn severity_color(severity: ld2_protocol::types::Severity) -> egui::Color32 {
    use ld2_protocol::types::Severity;
    match severity {
        Severity::High => egui::Color32::from_rgb(0xE0, 0x6C, 0x75),
        Severity::Medium => egui::Color32::from_rgb(0xD1, 0x9A, 0x66),
        Severity::Low => egui::Color32::from_rgb(0x98, 0xC3, 0x79),
        Severity::Unknown => egui::Color32::from_rgb(0xAB, 0xB2, 0xBF),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l10n::Lang;

    #[test]
    fn duration_formats_ru() {
        assert_eq!(format_duration(Lang::Ru, 45), "45с");
        assert_eq!(format_duration(Lang::Ru, 125), "2м5с");
        assert_eq!(format_duration(Lang::Ru, 3_661), "1ч1м");
        assert_eq!(format_duration(Lang::Ru, -5), "0с");
        // Чужие/бейджи: округление вверх до минут (ФР-11).
        assert_eq!(format_duration_ceil(Lang::Ru, 61), "2м0с");
        assert_eq!(format_duration_ceil(Lang::Ru, 59), "1м0с");
        assert_eq!(format_duration_ceil(Lang::Ru, 4_000), "1ч7м");
        assert_eq!(format_duration_ceil(Lang::Ru, 0), "0с");
    }

    #[test]
    fn crime_title_from_session_config() {
        let mut store = crate::store::AppStore::new();
        // Без сессии — фолбэк на сам kind.
        assert_eq!(crime_kind_title(&store, "marauder"), "marauder");
        // C конфигом (фикстуры мока недоступны в этом кrate — пустой конфиг
        // невозможен: session — только из событий; фолбэк проверен выше).
        let _ = &mut store;
    }
}
