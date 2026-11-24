//! Типы всплывающих окон, выхода и слоты ошибок отправки. Вынесено из
//! `root.rs` (RF3j).

use super::*;

/// M16/F29b: какое всплывающее окно рисуем: оболочка берёт
/// геометрию/флаги из `PopupState` по этому виду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopupKind {
    Character,
    Backpack,
    /// M18/F36: окно квестов (2 вкладки: текущие/выполненные).
    Quests,
    /// M18/F38: окно диалога NPC — как окно персонажа (попап).
    Npc,
    /// M18: описание предмета — как окно персонажа (попап).
    Item,
    /// M18/F53: окно заточки — попап (карточка с шапкой, без затемнения).
    Upgrade,
    /// M18: окно сокетов (инкрустация через меню оружия) — popup.
    Socketing,
    /// M20: окно выбора количества (переиспользуемое: выброс…).
    Quantity,
    /// M20: окно игрового диалога (заготовка: страницы/ввод).
    Dialog,
    /// M21: окно профессий (отдельное, подобно квестам).
    Professions,
    /// M23: окно подятия предмета — лут трупа слева, рюкзак справа.
    LootPickup,
    /// M24: админ-панель — листы по кнопкам шапки («Предметы»).
    AdminPanel,
    /// M26: окно карты мира (весь мир, зум/панорама) — заказчик.
    Map,
}

impl PopupKind {
    /// Суффикс id виджетов карточки (уникальность gpui-виджетов).
    pub(crate) fn id(self) -> &'static str {
        match self {
            PopupKind::Character => "ld2-char",
            PopupKind::Backpack => "ld2-bp",
            PopupKind::Quests => "ld2-quests",
            PopupKind::Npc => "ld2-npc",
            PopupKind::Item => "ld2-item",
            PopupKind::Upgrade => "ld2-upgrade",
            PopupKind::Socketing => "ld2-socket",
            PopupKind::Quantity => "ld2-quantity",
            PopupKind::Dialog => "ld2-dialog",
            PopupKind::Professions => "ld2-professions",
            PopupKind::LootPickup => "ld2-pickup",
            PopupKind::AdminPanel => "ld2-admin",
            PopupKind::Map => "ld2-map",
        }
    }
}

/// M20: text-поля execute-диалога — (id поля, поле ввода).
pub(crate) type DialogTextFields = Vec<(String, Entity<TextInput>)>;

/// M16/F29b: состояние всплывающего окна — геометрия drag/resize и
/// флаг «жест начат на scrim» (M15/F25e: закрыть может только
/// полный клик, целиком прошедший по scrim).
#[derive(Debug)]
pub(crate) struct PopupState {
    /// Окно открыто (рендер дополнительно гейтится `Screen::Game`).
    pub(crate) open: bool,
    /// Размер карточки (px): (ширина, высота).
    pub(crate) size: (f32, f32),
    /// Смещение от центра (px) — drag.
    pub(crate) delta: (f32, f32),
    /// Смещение указателя в шапке в момент захвата (drag).
    pub(crate) drag_offset: Option<(f32, f32)>,
    /// Якорь resize — (курсор_x, курсор_y, w, h, delta_x, delta_y).
    pub(crate) resize_anchor: Option<(f32, f32, f32, f32, f32, f32)>,
}

impl PopupState {
    pub(crate) fn new(size: (f32, f32)) -> Self {
        Self {
            open: false,
            size,
            delta: (0.0, 0.0),
            drag_offset: None,
            resize_anchor: None,
        }
    }
}

/// M14/F16: какое действие выполнит таймер выхода.
#[derive(Clone, Copy)]
pub(crate) enum ExitKind {
    /// Из мира — на экран персонажей.
    SwitchChar,
    /// На логин-экран (дисконнект).
    Logout,
}

/// M14/F16: длина таймера выхода, мс.
pub(crate) const EXIT_DELAY_MS: i64 = 20_000;

/// Куда положить ошибку отправки команды.
#[derive(Clone, Copy)]
pub(crate) enum SendSlot {
    Notice,
    World,
    Login,
    Create,
    Select,
}
