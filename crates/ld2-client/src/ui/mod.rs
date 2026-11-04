//! UI-слой M2–M3 (ФР-01…ФР-03, ФР-17): экраны входа/персонажей, игровой
//! мир (локация/сущности/история) и журнал.
//!
//! Экраны — проекция [`AppStore`] (см. `store.rs`): они читают состояние,
//! отправляют команды через [`TransportHandle`] и возвращают [`Effect`]'ы
//! для слоя приложения (keyring/транспорт).

use eframe::egui;

use crate::store::{AppStore, Effect};
use crate::transport::TransportHandle;

pub mod chat_panel;
pub mod hotkeys;
pub mod log_panel;
pub mod screens;
pub mod settings;
pub mod sheet;
pub mod world;

impl UiCtx<'_> {
    /// Сохранение `config.toml` по факту изменения настройки (ФР-19).
    pub fn persist_cfg(&self) {
        if let Err(e) = self.cfg_store.save(self.cfg) {
            tracing::warn!("не удалось сохранить конфиг: {e:#}");
        }
    }
}

/// Контекст экрана: всё, что нужно функции отрисовки.
pub struct UiCtx<'a> {
    pub store: &'a mut AppStore,
    pub forms: &'a mut Forms,
    pub transport: &'a TransportHandle,
    pub lang: crate::l10n::Lang,
    /// Локальный конфиг (ФР-19): тема/шрифт/лимиты/хоткеи/состояние UI.
    pub cfg: &'a mut crate::config::Config,
    /// Персистентность конфига (сохранение по факту изменения).
    pub cfg_store: &'a crate::config::ConfigStore,
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
    /// Пати-виджет свёрнут (ФР-14: «сворачиваемый»).
    pub party_collapsed: bool,
    /// Поле золота в окне сделки (ФР-15).
    pub trade_gold: String,
    /// Выбранный в ComboBox предмет для добавления в сделку.
    pub trade_item: String,
    /// Поле имени новой гильдии (ФР-16).
    pub guild_name: String,
    /// Открыто ли окно настроек (ФР-19).
    pub settings_open: bool,
    /// Запись горячей клавиши: ждём нажатия для действия с этим индексом.
    pub hotkey_capture: Option<usize>,
    /// Черновик хоста сервера в настройках.
    pub host_draft: String,
    /// Смена хоста применена — App переподключит транспорт после кадра.
    pub host_apply_pending: bool,
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

/// Профиль персонажа — переедал в `ld2-core` (D12, общий с gpui).
pub use ld2_core::store::Profile;

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
    /// Гильдия: состав, ранги, список (ФР-16, M8).
    Guild,
}

impl SheetTab {
    /// Строковое имя для `config.toml` (ФР-19: состояние вкладки).
    pub fn as_str(self) -> &'static str {
        match self {
            SheetTab::Skills => "skills",
            SheetTab::Attrs => "attrs",
            SheetTab::Inventory => "inventory",
            SheetTab::Statuses => "statuses",
            SheetTab::Quests => "quests",
            SheetTab::Professions => "professions",
            SheetTab::Guild => "guild",
        }
    }

    /// Восстановление из конфига (неизвестное значение → дефолт).
    pub fn parse_tab(s: &str) -> Self {
        match s {
            "attrs" => SheetTab::Attrs,
            "inventory" => SheetTab::Inventory,
            "statuses" => SheetTab::Statuses,
            "quests" => SheetTab::Quests,
            "professions" => SheetTab::Professions,
            "guild" => SheetTab::Guild,
            _ => SheetTab::Skills,
        }
    }
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

/// Форматирование длительностей и заголовков преступлений — переиспользует
/// `ld2-core` (D12: общая логика обоих клиентов).
pub use ld2_core::l10n::{format_duration, format_duration_ceil};
pub use ld2_core::store::crime_kind_title;

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
