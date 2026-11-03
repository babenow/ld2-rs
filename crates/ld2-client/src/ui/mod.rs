//! UI-слой M2–M3 (ФР-01…ФР-03, ФР-17): экраны входа/персонажей, игровой
//! мир (локация/сущности/история) и журнал.
//!
//! Экраны — проекция [`AppStore`] (см. `store.rs`): они читают состояние,
//! отправляют команды через [`TransportHandle`] и возвращают [`Effect`]'ы
//! для слоя приложения (keyring/транспорт).

use crate::l10n::Key;
use crate::store::{AppStore, Effect};
use crate::transport::TransportHandle;

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
