//! Корневой view gpui-клиента (M10.3): шапка, экраны входа/персонажей/мира,
//! окно настроек и `tick` — опрос ingress-канала транспорта + исполнение
//! [`Effect`] (аналог `App::ui` egui-клиента, D12: логика в `ld2-core`).

use std::cell::RefCell;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, App, Context, Div, EmptyView, Entity, Focusable,
    Hsla, MouseButton, Render, ScrollHandle, SharedString, UniformListScrollHandle, Window,
    WindowControlArea,
};
use ld2_core::config::{host_allowed, Config, ConfigStore, Hotkeys};
use ld2_core::l10n::{self, t, tf, Key, Lang};
use ld2_core::store::{
    filter_active_buffs, local_epoch, local_epoch_ms, Act, AppStore, Effect, Screen,
};
use ld2_core::transport::{self, IngressEvent, TransportConfig, TransportHandle, TransportStatus};
// R11/R16: StateResync (дельта неприменима), TimeSync (5 мин).
use ld2_protocol::cmd::{AuthLogin, CharDelete, LocationMove, StateResync, TimeSync};

mod admin;
mod backpack;
mod chars;
mod chat;
mod dialog;
mod game;
mod pickup;
mod professions;
mod quantity;
mod quests;
mod register;
mod sheet;
mod social;

use crate::palette::{palette, Palette};
use ely::buttons::IconButton;
use ely::feedback::{Toast, Toaster};
use ely::forms::{
    hex, parse_hex, Checkbox, Choice, ColorPicker, HotkeyInput, Input, InputEvent, NumberInput,
    PasswordInput, Slider, Switch, TextInput,
};
use ely::layout::ScrollArea;
use ely::navigation::Tabs;
use ely::overlays::Dialog;
use ely::primitives::IconName;
use ely::shell::WindowControls;
/// Автор клиента (статус-бар логина/выбора персонажа, заказчик).
pub(crate) const AUTHOR: &str = "M. Babenko";

/// R-dnd (заказчик 2026-11-21): payload перетаскивания предмета.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemDrag {
    pub uid: String,
    pub from: DragSource,
    /// Родной слот предмета (для подсветки/проверки зон).
    pub slot_req: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DragSource {
    /// Из рюкзака.
    Bp,
    /// Из слота экипировки (ид слота).
    Equip(String),
}

/// Подтверждение выброса (перетащили вне зон).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DropPending {
    pub uid: String,
    pub name: String,
    pub from: DragSource,
    /// Размер стака (1 — одиночный предмет).
    pub max: i64,
    /// Сколько выбрасываем (1..=max; ввод/ползунок/«Все»).
    pub qty: i64,
}

/// R-dnd: «карточка» предмета под курсором при перетаскивании
/// (ghost-вид on_drag — следует за мышью).
pub(crate) struct ItemDragGhost {
    pub name: String,
    /// Цвет рамки/текста — цвет редкости.
    pub color: u32,
    pub theme: String,
}

impl gpui::Render for ItemDragGhost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let pal = crate::palette::palette(&self.theme);
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(self.color))
            .text_size(px(13.0))
            .text_color(rgb(self.color))
            .child(self.name.clone())
            .opacity(0.9)
    }
}

/// R-dnd: состояние удерживаемой строки списка рюкзака.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BpHold {
    pub uid: String,
    /// Индекс удерживаемой строки в исходном порядке.
    pub index: usize,
    /// Целевой индекс (landing).
    pub target: usize,
    /// Смещение строки под курсором, px.
    pub top: f32,
    /// Захват внутри строки (offset.y при старте), px.
    pub grab: f32,
}

use ely::theme::{Mode, Theme};

/// Release-сборка: внешний `ws://` запрещён (НФР безопасность, guard M10.1).
const RELEASE: bool = !cfg!(debug_assertions);

/// M14/F15: перетаскивание окна за TitleBar (payload drag gpui).
#[derive(Debug)]
struct TitlebarDrag;

/// M16/F29b: перетаскивание карточки всплывающего окна (персонаж/
/// рюкзак) за шапку — общий payload drag оболочки `popup_window`.
#[derive(Debug)]
struct PopupDrag;

/// M16/F29b: изменение размера карточки всплывающего окна (уголок).
#[derive(Debug)]
struct PopupResize;

/// M16/F29b: какое всплывающее окно рисуем: оболочка берёт
/// геометрию/флаги из `PopupState` по этому виду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
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
}

impl PopupKind {
    /// Суффикс id виджетов карточки (уникальность gpui-виджетов).
    fn id(self) -> &'static str {
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
        }
    }
}

/// M18/F33/M24: открытие окна сбрасывает контекстные меню **обеих**
/// сторон (`bp_menu` + `equip_menu`). Иначе закрытие окна с открытым
/// меню оставляло `Some(...)`: keyed state меню и `asked`-счётчик
/// гаснут вместе с элементом (кадр без рендера), и stale-запрос
/// `.manual(Some((n, at)))` автооткрывал меню при следующем рендере.
/// M24 (заказчик): окно подятия/админки рисует общий `backpack_grid`
/// — чистить «своё» меню по kind было мало: в луте самооткрывалось
/// меню рюкзака. `open_popup` — единственная точка, где окно становится
/// открытым, — инвариант: любое открытое окно стартует с пустыми
/// ОБОИМИ меню.
fn clear_popup_menu<T>(bp_menu: &mut Option<T>, equip_menu: &mut Option<T>) {
    *bp_menu = None;
    *equip_menu = None;
}

/// M20: text-поля execute-диалога — (id поля, поле ввода).
type DialogTextFields = Vec<(String, Entity<TextInput>)>;

/// M16/F29b: состояние всплывающего окна — геометрия drag/resize и
/// флаг «жест начат на scrim» (M15/F25e: закрыть может только
/// полный клик, целиком прошедший по scrim).
#[derive(Debug)]
struct PopupState {
    /// Окно открыто (рендер дополнительно гейтится `Screen::Game`).
    open: bool,
    /// Размер карточки (px): (ширина, высота).
    size: (f32, f32),
    /// Смещение от центра (px) — drag.
    delta: (f32, f32),
    /// Смещение указателя в шапке в момент захвата (drag).
    drag_offset: Option<(f32, f32)>,
    /// Якорь resize — (курсор_x, курсор_y, w, h, delta_x, delta_y).
    resize_anchor: Option<(f32, f32, f32, f32, f32, f32)>,
}

impl PopupState {
    fn new(size: (f32, f32)) -> Self {
        Self {
            open: false,
            size,
            delta: (0.0, 0.0),
            drag_offset: None,
            resize_anchor: None,
        }
    }
}

/// M14/F9: вкладки экрана настроек (состояние сессии, конфиг не трогаем).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    /// Тема, язык, шрифты, история чата, уведомления, путь конфига.
    General,
    /// Шрифты: глобальный масштаб, шрифт чата и лога (M14/F14).
    Fonts,
    /// Адрес сервера: read-only + «подключиться к другому» (M14/F11).
    Connection,
    /// Ремаппинг 11 действий (навигация/цикл + боевые 1–6).
    Hotkeys,
}

/// Что делать по клику (замыкания в listener должны быть `Fn` — действуем
/// по копируемому перечислению).
#[derive(Clone)]
pub(crate) enum Click {
    Login,
    /// M13.3: открыть диалог регистрации (ФР-01).
    OpenRegister,
    Logout,
    /// M14/F8: выбор языка в настройках («ru»/«en»).
    Lang(&'static str),
    ToggleSettings,
    /// M15/F25: открыть/закрыть окно персонажа (кнопка в шапке).
    ToggleCharacter,
    /// M16/F29b: открыть/закрыть окно рюкзака (кнопка в шапке).
    ToggleInventory,
    /// M18/F36: открыть/закрыть окно квестов (кнопка в шапке).
    ToggleQuests,
    /// M21: окно профессий (кнопка в шапке/хоткей).
    ToggleProfessions,
    Theme(&'static str),
    ChatLimit(i64),
    /// M14/F14: флаг «системные уведомления» (слайдер-переключатель).
    SetNotify(bool),
    /// Мат-фильтр исходящих (заказчик 2026-11-20).
    SetMuteFilter(bool),
    ApplyHost,
    /// M14/F11: раскрыть ввод нового адреса сервера.
    EditHost,
    /// M14/F13: жирный шрифт строк урона в журнале.
    LogDamageBold(bool),
    /// M14/F14: открыть/закрыть ColorPicker строки (id свотча).
    TogglePicker(&'static str),
    MoveExit(String),
    OpenReleases,
    // M10.4: игровой экран
    Attack(String),
    Flee,
    Examine(String),
    /// M18/F35: диалог NPC (поговорить/квесты).
    NpcTalk(String),
    PartyInvite(String),
    TradeOpen(String),
    RuneCast {
        staff_uid: String,
        socket_index: u32,
    },
    UseItem(String),
    PowerStrike,
    ConfirmDeath,
    LootTake,
    /// M23: «Взять всё» — партия из лута окна подятия в очередь.
    LootTakeAll,
    /// M23: открыть окно подятия предмета по трупу (ЛКМ в списке
    /// сущностей) — запрос `entity.info` c `Act::PickupInfo`.
    LootOpen(String),
    /// Лут трупа игрока: открыть подтверждение (uid, name) — ФР-11.
    LootTakeMarker(String, String),
    LootCancel,
    /// M20/F20b: добыть объект-ресурс (`cmd/resource.gather`).
    Gather(String),
    /// M22: разделить труп (`action`: skin|gut) — сервер-джоб5 с.
    Harvest(String, String),
    /// M24: открыть админ-панель (кнопка в шапке, только admin).
    AdminOpen,
    /// M24: выдать предмет из каталога → execute-диалог количества.
    AdminGive(String),
    /// M24: удалить предмет из цели → execute-диалог количества.
    AdminRemove(String),
    /// M24: тумблер «режим бога» в шапке админ-окна.
    AdminGod(bool),
    /// M24: выбрать цель правой колонки (`self`/`char:`/`monster:`/
    /// `corpse:`) → `cmd/admin.target.info`.
    AdminTarget(String),
    /// M24: «Удалить все» — очистка предметов выбранной цели.
    AdminClear(String),
    CloseDetail,
    SwitchChar,
}

/// M14/F16: какое действие выполнит таймер выхода.
#[derive(Clone, Copy)]
enum ExitKind {
    /// Из мира — на экран персонажей.
    SwitchChar,
    /// На логин-экран (дисконнект).
    Logout,
}

/// M14/F16: длина таймера выхода, мс.
const EXIT_DELAY_MS: i64 = 20_000;
/// M17/F30a: длительность каст-бара инкрустации, мс (спека: 3 с,
/// без отмены — движение/урон не прерывают).
/// M24: единый прогресс-бар левой колонки (указание заказчика) —
/// одна сущность на снятие шкур/освежевание, инкрустацию и заточку.
/// Подпись «Действие · имя»; запуск — [`Root::start_progress`];
/// гаснет в `tick` по исчезновению [`ProgressSource`]-источника.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CastProgress {
    /// Источник — по нему бар и гаснет.
    pub(crate) source: ProgressSource,
    /// Подпись: «Действие · имя».
    pub(crate) label: String,
    pub(crate) started_ms: i64,
    pub(crate) duration_ms: i64,
}

/// Источник единого прогресс-бара.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressSource {
    /// Разделка трупа — `store.harvest` (сервер-джоб5 с).
    Harvest,
    /// Каст инкрустации — `cast_inscribe` ([`INSCRIBE_MS`]).
    Inscribe,
    /// Прогресс заточки — `upgrade_progress` (`duration_s`).
    Sharpen,
}

pub(crate) const INSCRIBE_MS: i64 = 3_000;

/// Инициализация корневого view (всё, что создаётся вне gpui-контекста).
pub struct Shell {
    pub store: AppStore,
    pub config: Config,
    pub config_store: ConfigStore,
    pub lang: Lang,
    pub transport: TransportHandle,
    pub ingress: mpsc::UnboundedReceiver<IngressEvent>,
    pub update_rx: std::sync::mpsc::Receiver<String>,
    /// M16/F29j: запомненный пароль из keyring для предзаполнения
    /// поля на экране входа (читается в main.rs, один раз при старте).
    pub prefill_password: Option<String>,
}

use tokio::sync::mpsc;

pub struct Root {
    store: AppStore,
    config: Config,
    config_store: ConfigStore,
    lang: Lang,
    transport: TransportHandle,
    ingress: mpsc::UnboundedReceiver<IngressEvent>,
    update_rx: std::sync::mpsc::Receiver<String>,
    update_available: Option<String>,
    login_input: Option<Entity<TextInput>>,
    password_input: Option<Entity<TextInput>>,
    /// M17/F30a: активный каст инкрустации (uid руны, uid оружия,
    /// epoch-мс завершения); без отмены — идёт до конца.
    cast_inscribe: Option<(String, String, i64)>,
    /// M16/F29j: пароль последней отправки входа/регистрации — живёт
    /// до успешного `PersistSession` и там же уходит в keyring
    /// (или стирается, если галочка выключена).
    pending_password: Option<String>,
    /// M16/F29j: предзаполнение пароля из keyring — отдаётся полю
    /// в `init_inputs` (один раз при старте, `take()`).
    prefill_password: Option<String>,
    /// Поля диалога регистрации (M13.3, D35): живут только в UI-состоянии,
    /// в store/логи не попадают.
    register_open: bool,
    register_submitted: bool,
    reg_login: Option<Entity<TextInput>>,
    reg_email: Option<Entity<TextInput>>,
    reg_password: Option<Entity<TextInput>>,
    reg_password2: Option<Entity<TextInput>>,
    host_input: Option<Entity<TextInput>>,
    /// M19 (§15): ввод домена discovery (пусто — прямой режим host).
    domain_input: Option<Entity<TextInput>>,
    settings_open: bool,
    /// M16/F29b: окно персонажа — оверлей поверх игрового экрана
    /// (геометрия/флаги жестов — в `char_popup`).
    char_popup: PopupState,
    /// M16/F29b: окно рюкзака — может быть открыто одновременно с
    /// персонажем (общий scrim группы окон).
    backpack_popup: PopupState,
    /// M18/F36: окно квестов — третий член группы окон (общий scrim).
    quests_popup: PopupState,
    /// M18/F38: окно диалога NPC — геометрия (гейт рендера — наличие
    /// `store.npc_dialog`, см. `npc_window`).
    npc_popup: PopupState,
    item_popup: PopupState,
    /// M18: окно сокетов (инкрустация через меню оружия) — popup.
    socket_popup: PopupState,
    /// M20: окно выбора количества — popup.
    quantity_popup: PopupState,
    /// M20: окно игрового диалога (заготовка) — popup.
    dialog_popup: PopupState,
    /// M18: вторая страница окна инкрустации — выбранный слот
    /// (список вставок); `None` — страница слотов.
    socket_pick: Option<u32>,
    /// M20: открытое окно выбора количества (заготовка под покупку…).
    quantity_pick: Option<quantity::QuantityPick>,
    /// M20: игровой диалог (заготовка) — индекс текущей страницы.
    dialog_page: usize,
    /// M20: execute-диалог — значения number-полей (id → строка).
    dialog_values: std::collections::HashMap<String, String>,
    /// M20: execute-диалог — text-поля (`Entity<TextInput>`; создаются
    /// под диалог, хранятся `(dialog_id, [(input_id, поле)])`).
    dialog_inputs: Option<(String, DialogTextFields)>,
    /// M20: id последнего открытого диалога — сброс страницы/полей.
    dialog_seen: Option<String>,
    /// M21: окно профессий — popup (отдельное, как квесты).
    professions_popup: PopupState,
    /// M23: окно подятия предмета — popup (лут слева, рюкзак справа);
    /// гейт рендера — флаг open + данные `store.pickup`.
    pickup_popup: PopupState,
    /// M23: выбранная строка лута (uid) — для кнопки «Забрать».
    pickup_sel: Option<String>,
    /// M23: какую сущность перезапрашиваем (`cmd/entity.info`).
    pickup_id: Option<String>,
    /// M23: нужен (re)запрос лута — выполняется в `tick`, только когда
    /// транспорт свободен: корреляция ack'ов стора одиночная
    /// (`pending`), параллельная команда перетёрла бы чужой ack.
    pickup_request: bool,
    /// M23: прокрутка левой колонки окна подятия.
    pickup_scroll: ScrollHandle,
    /// M23: прокрутка рюкзак-виджета в правой колонке (своя — ручка
    /// не должна делиться с окном рюкзака при одновременном открытии).
    pickup_bp_scroll: ScrollHandle,
    /// M23: очередь «Взять всё» — uid-ы, которые ещё предстоит взять;
    /// отправляется по одному за свободный ack-слот (`pending`
    /// одиночный, см. `pump_take_queue`).
    pickup_queue: Vec<String>,
    /// M23: «Взять всё» на трупе игрока ждёт подтверждения статуса
    /// (ФР-11) в модалке loot_confirm.
    pickup_confirm_all: bool,
    /// M24: окно админ-панели — popup (листы по кнопкам шапки).
    admin_popup: PopupState,
    /// M24: прокрутка каталога (uniform_list — виртуализация).
    admin_scroll: UniformListScrollHandle,
    /// M24: прокрутка содержимого цели (правая колонка).
    admin_inv_scroll: ScrollHandle,
    /// M24: поле поиска предмета в админ-окне.
    admin_search: Option<Entity<TextInput>>,
    /// M24: нужен (re)запрос каталога — как `pickup_request` (pending
    /// одиночный); каталог кэшируется на сессию (вариант A).
    admin_catalog_request: bool,
    /// M26: догрузка карты мира начата (одна попытка на сессию —
    /// насос чанков `cmd/map.world` в tick).
    map_started: bool,
    /// M26: позиции узлов карты (компасная раскладка от спавна) —
    /// считаются после полной загрузки чанков.
    map_pos: std::collections::HashMap<String, crate::map::MapNode>,
    /// M26: раскладка посчитана.
    map_pos_ready: bool,
    /// M24: цель выдачи (F24b — всегда self; F24c — селект целей).
    admin_target: String,
    /// M24: вкладка-фильтр правой колонки (независима от левой,
    /// не персистентна — разовый просмотр содержимого цели).
    admin_inv_tab: backpack::BackpackTab,
    /// M24: строка поиска правой колонки.
    admin_inv_search: Option<Entity<TextInput>>,
    /// M24: фильтр раритета левого каталога (`all` | rarity).
    admin_rarity: String,
    /// M24: фильтр раритета правой колонки (независимый).
    admin_inv_rarity: String,
    /// M24: нужен (re)запрос содержимого цели (`admin.target.info`) —
    /// как `admin_catalog_request` (pending одиночный).
    admin_target_request: bool,
    /// M24: поле «имя игрока» — цель `char:<имя>` (в т.ч. оффлайн).
    admin_player: Option<Entity<TextInput>>,
    /// M21: вооружённая кнопка «Сбросить» (второй клик — cmd/craft.reset).
    prof_reset_armed: Option<String>,
    /// M18/F53: окно заточки — попап (геометрия карточки).
    upgrade_popup: PopupState,
    /// M16/F29b: mousedown последнего жеста пришёлся на общий scrim
    /// всплывающих окон — закрытие по mouse-up только для настоящего
    /// «клика мимо» (M15/F25e).
    popup_scrim_down: bool,
    /// M16/F29b: владелец текущего жеста drag/resize — `DragMoveEvent`
    /// с общим payload-типом получают ВСЕ открытые окна, двигаться/
    /// ресайзиться может только владелец (иначе окна едут вместе).
    drag_owner: Option<PopupKind>,
    /// M16/F29f: вьюпорт кэшируется в render — базовый размер окна
    /// клампится под экран при открытии (в обработчиках нет Window).
    viewport: (f32, f32),
    /// M16/F29b: активная вкладка рюкзака (персистентна в config).
    backpack_tab: backpack::BackpackTab,
    /// M18/F36: вкладка окна квестов (текущие/выполненные) и выбор
    /// в левой колонке (показывается в правой).
    quests_tab: quests::QuestsTab,
    selected_quest: Option<String>,
    /// M14/F16: открытый диалог выхода — (действие, дедлайн мс по now_ms()).
    exit_timer: Option<(ExitKind, i64)>,
    /// M18/F53: прогресс улучшения в диалоге — (uid, preview_id, старт мс,
    /// длительность мс). Команда `upgrade.start` отправляется по истечении.
    upgrade_progress: Option<(String, String, i64, i64)>,
    /// M24: единый прогресс-бар левой колонки (см. [`CastProgress`]).
    cast_progress: Option<CastProgress>,
    /// M14/F17: открытая карточка навыка — id из снапшота.
    skill_info: Option<String>,
    first_frame: bool,

    /// Поле имени новой гильдии (ФР-16).
    guild_input: Option<Entity<TextInput>>,
    /// Открытая вкладка листа (ФР-19: персистентна в config).
    sheet_tab: sheet::SheetTab,
    /// M16/F29d: запрос контекстного меню экипировки — (слот, номер,
    /// точка клика); номер растёт, чтобы Ely переоткрыл меню.
    equip_menu: Option<(String, u64, gpui::Point<gpui::Pixels>)>,
    /// M16/F29g: то же для контекстного меню предмета рюкзака (uid).
    bp_menu: Option<(String, u64, gpui::Point<gpui::Pixels>)>,
    /// M10.5: чат — ввод, фильтр, активный канал.
    chat_input: Option<Entity<TextInput>>,
    chat_filter: Option<Entity<TextInput>>,
    /// История ввода чата (↑/↓ — заказчик 2026-11-20).
    chat_history: Vec<String>,
    /// Позиция в истории (None — текущий ввод).
    chat_history_pos: Option<usize>,
    /// Окно таймаута отправки чата (анти-спам; §5 chat.cooldown_s).
    chat_cooldown_until: Option<std::time::Instant>,
    /// hovered-предмет (переливание рамки): `bp:<uid>` / `eq:<slot>`.
    hovered_item: Option<String>,
    /// R-dnd: тянущийся предмет (зоны/подтверждение выброса).
    drag_item: Option<ItemDrag>,
    /// R-dnd: подтверждение выброса (окно).
    drop_confirm: Option<DropPending>,
    /// R-dnd: reorder-состояние списка рюкзака (held-строка).
    bp_hold: Option<BpHold>,

    chat_channel: String,
    /// M10.5: окно сделки — поле золота.
    /// M11.3: золото сделки (NumberInput Ely; фиксируется операцией).
    trade_gold_value: f64,

    trade_pick: Option<String>,
    /// M11.7 (D20/D21): поле ввода ника и длительность таймера удаления.
    delete_name: Option<Entity<TextInput>>,
    delete_delay: i64,
    /// M11.4: тосты Ely (notice → всплывающее уведомление).
    toaster: Entity<Toaster>,
    last_notice: Option<String>,
    /// M11.4: прокрутка журнала (M14/F7 — свободная, см. `journal_*`)
    /// и stick-to-bottom чата.
    journal_scroll: ScrollHandle,
    chat_scroll: ScrollHandle,
    /// M18/F46: прокрутка ленты диалога NPC (stick-to-bottom) и длина
    /// ленты на прошлом кадре — по ней решается досcrollивание вниз.
    npc_scroll: ScrollHandle,
    npc_seen: usize,
    /// M14/F3: обычная прокрутка вкладок листа «Навыки»/«Профессии».
    skills_scroll: ScrollHandle,
    professions_scroll: ScrollHandle,
    /// M22: прокрутка панели обучения в диалоге НПС.
    training_scroll: ScrollHandle,
    /// M16/F29b: прокрутка тела окна рюкзака (сетка предметов).
    backpack_scroll: ScrollHandle,
    /// M18/F36: скролл тела окна квестов.
    quests_scroll: ScrollHandle,
    /// M15/F26: прокрутка вкладки «Параметры» (16 строк).
    params_scroll: ScrollHandle,
    /// M14/F7: активная вкладка блока журнала (Лог/Чат), состояние сессии.
    /// M14/F9: вкладка настроек и её прокрутка (состояние сессии).
    settings_tab: SettingsTab,
    settings_scroll: ScrollHandle,
    /// M14/F11: раскрыт ли ввод нового адреса сервера (сессия).
    host_editing: bool,
    /// M14/F14: какой ColorPicker раскрыт под свотчем (id строки).
    color_picker_open: Option<&'static str>,
    /// Длина лога на прошлой отрисовке — чтобы приклеивать вниз только
    /// при изменении (M14/F13: лог всегда промотан вниз).
    journal_seen: usize,
    /// R16: момент последней перекалибровки часов (раз в 5 мин).
    last_time_sync: Option<std::time::Instant>,
    last_chat_len: usize,
    /// M10.5: свёрнут ли пати-виджет (ФР-19, из конфига).
    party_collapsed: bool,
    /// M10.6: создание/удаление персонажей (ФР-02).
    create_name: Option<Entity<TextInput>>,
    create_profile: ld2_core::store::Profile,
    /// M18/F34: пол — селектор в форме создания (по умолчанию мужской).
    create_gender: ld2_core::store::Gender,
    create_open: bool,
    create_submitted: bool,
    delete_target: Option<(String, String)>,
}

/// M15/F25i: поле ввода, чей фокус блокирует хоткеи на экране:
/// (экран, имя поля для теста, доступ к полю Root).
struct TypingField {
    screen: Screen,
    /// Имя поля — только для теста покрытия (в рендере не читается).
    #[allow(dead_code)]
    name: &'static str,
    get: fn(&Root) -> &Option<Entity<TextInput>>,
}

/// M15/F25i: поля ввода по экранам — источник истины
/// `Root::typing_focused` (тест покрывает все TextInput Root): на
/// экране проверяются только его поля — после смены экрана фокус
/// формально остаётся на неотрисованном поле (пароль на Login →
/// Game), и все хоткеи «залипали».
const TYPING_FIELDS: &[TypingField] = &[
    TypingField {
        screen: Screen::Login,
        name: "login",
        get: |r| &r.login_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "password",
        get: |r| &r.password_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_login",
        get: |r| &r.reg_login,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_email",
        get: |r| &r.reg_email,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_password",
        get: |r| &r.reg_password,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_password2",
        get: |r| &r.reg_password2,
    },
    TypingField {
        screen: Screen::Login,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "domain",
        get: |r| &r.domain_input,
    },
    TypingField {
        screen: Screen::Characters,
        name: "create_name",
        get: |r| &r.create_name,
    },
    TypingField {
        screen: Screen::Characters,
        name: "delete_name",
        get: |r| &r.delete_name,
    },
    TypingField {
        screen: Screen::Characters,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Characters,
        name: "domain",
        get: |r| &r.domain_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "chat",
        get: |r| &r.chat_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "chat_filter",
        get: |r| &r.chat_filter,
    },
    TypingField {
        screen: Screen::Game,
        name: "guild",
        get: |r| &r.guild_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "domain",
        get: |r| &r.domain_input,
    },
    // M24: поля фильтров админ-панели — при наборе не должны
    // срабатывать горячие клавиши (C/I/… открывали окна).
    TypingField {
        screen: Screen::Game,
        name: "admin_search",
        get: |r| &r.admin_search,
    },
    TypingField {
        screen: Screen::Game,
        name: "admin_inv_search",
        get: |r| &r.admin_inv_search,
    },
    TypingField {
        screen: Screen::Game,
        name: "admin_player",
        get: |r| &r.admin_player,
    },
];

impl Root {
    pub fn new(cx: &mut Context<'_, Self>, shell: Shell) -> Self {
        let sheet_tab = sheet::SheetTab::parse(&shell.config.sheet_tab);
        let party_collapsed = shell.config.party_collapsed;
        // Заказчик 2026-11-20: журнал переводит каналы чата по языку.
        let mut store = shell.store;
        store.lang = shell.lang;
        Self {
            store,
            config: shell.config,
            config_store: shell.config_store,
            lang: shell.lang,
            transport: shell.transport,
            ingress: shell.ingress,
            update_rx: shell.update_rx,
            update_available: None,
            login_input: None,
            password_input: None,
            cast_inscribe: None,
            pending_password: None,
            prefill_password: shell.prefill_password,
            register_open: false,
            register_submitted: false,
            reg_login: None,
            reg_email: None,
            reg_password: None,
            reg_password2: None,
            host_input: None,
            domain_input: None,
            settings_open: false,
            char_popup: PopupState::new((760.0, 560.0)),
            // Рюкзак со сдвигом: при открытии обоих окон видны обе
            // шапки (дальше позиция перетаскивается).
            backpack_popup: PopupState {
                delta: (40.0, 40.0),
                ..PopupState::new((760.0, 560.0))
            },
            // M18/F36: квесты — левее рюкзака (обе шапки видны).
            quests_popup: PopupState {
                delta: (-40.0, 40.0),
                ..PopupState::new((800.0, 560.0))
            },
            // M18/F38: диалог NPC — правее и выше центра.
            npc_popup: PopupState {
                delta: (60.0, -40.0),
                ..PopupState::new((640.0, 560.0))
            },
            item_popup: PopupState {
                delta: (80.0, -20.0),
                // M18/F55: фоллбэк до open_popup — в диапазоне
                // «резиновой» ширины (320..480).
                ..PopupState::new((400.0, 420.0))
            },
            // M18: окно сокетов — карточка по центру (как заточка).
            socket_popup: PopupState {
                delta: (0.0, -40.0),
                ..PopupState::new((440.0, 420.0))
            },
            // M20: окно количества — компактная карточка по центру.
            quantity_popup: PopupState {
                delta: (0.0, -60.0),
                ..PopupState::new((400.0, 280.0))
            },
            // M20: окно игрового диалога — карточка по центру.
            dialog_popup: PopupState::new((560.0, 460.0)),
            socket_pick: None,
            quantity_pick: None,
            dialog_page: 0,
            dialog_values: std::collections::HashMap::new(),
            dialog_inputs: None,
            dialog_seen: None,
            professions_popup: PopupState {
                delta: (0.0, -30.0),
                ..PopupState::new((880.0, 620.0))
            },
            // M23: окно подятия — два столбца (лут | рюкзак);
            // правая колонка шире — под вкладки-фильтры рюкзака.
            pickup_popup: PopupState {
                delta: (40.0, -30.0),
                ..PopupState::new((1040.0, 600.0))
            },
            pickup_sel: None,
            pickup_id: None,
            pickup_request: false,
            pickup_scroll: ScrollHandle::new(),
            pickup_bp_scroll: ScrollHandle::new(),
            pickup_queue: Vec::new(),
            pickup_confirm_all: false,
            admin_popup: PopupState {
                delta: (0.0, -20.0),
                ..PopupState::new((1040.0, 640.0))
            },
            admin_scroll: UniformListScrollHandle::new(),
            admin_inv_scroll: ScrollHandle::new(),
            admin_search: None,
            admin_catalog_request: false,
            map_started: false,
            map_pos: std::collections::HashMap::new(),
            map_pos_ready: false,
            admin_target: "self".into(),
            admin_inv_tab: backpack::BackpackTab::default(),
            admin_inv_search: None,
            admin_rarity: "all".into(),
            admin_inv_rarity: "all".into(),
            admin_target_request: false,
            admin_player: None,
            prof_reset_armed: None,
            // M18/F53: окно заточки — компактная карточка по центру,
            // без затемнения (см. open_popup: базовый размер).
            upgrade_popup: PopupState {
                delta: (0.0, -40.0),
                ..PopupState::new((440.0, 360.0))
            },
            backpack_tab: backpack::BackpackTab::default(),
            quests_tab: quests::QuestsTab::default(),
            selected_quest: None,
            popup_scrim_down: false,
            drag_owner: None,
            viewport: (0.0, 0.0),
            exit_timer: None,
            upgrade_progress: None,
            cast_progress: None,
            skill_info: None,
            first_frame: true,
            guild_input: None,
            sheet_tab,
            equip_menu: None,
            bp_menu: None,
            chat_input: None,
            chat_filter: None,
            chat_history: Vec::new(),
            chat_history_pos: None,
            chat_cooldown_until: None,
            hovered_item: None,
            drag_item: None,
            drop_confirm: None,
            bp_hold: None,

            chat_channel: "location".into(),
            trade_gold_value: 0.0,
            trade_pick: None,
            delete_name: None,
            delete_delay: 10,
            toaster: cx.new(|_| Toaster::default()),
            last_notice: None,
            journal_scroll: ScrollHandle::new(),
            chat_scroll: ScrollHandle::new(),
            npc_scroll: ScrollHandle::new(),
            npc_seen: 0,
            skills_scroll: ScrollHandle::new(),
            professions_scroll: ScrollHandle::new(),
            training_scroll: ScrollHandle::new(),
            backpack_scroll: ScrollHandle::new(),
            quests_scroll: ScrollHandle::new(),
            params_scroll: ScrollHandle::new(),
            settings_tab: SettingsTab::General,
            settings_scroll: ScrollHandle::new(),
            host_editing: false,
            color_picker_open: None,
            journal_seen: 0,
            last_time_sync: None,
            last_chat_len: 0,
            party_collapsed,
            create_name: None,
            create_profile: ld2_core::store::Profile::default(),
            create_gender: ld2_core::store::Gender::default(),
            create_open: false,
            create_submitted: false,
            delete_target: None,
        }
    }

    // ── tick: опрос транспорта + эффекты + поля ввода ─────────────────────

    /// Один шаг фонового цикла: дренаж ingress (≤64), исполнение эффектов,
    /// Enter в полях входа, бейдж обновлений. Возвращает задержку до
    /// следующего шага (НФР §8: события → 50 мс, idle → 200 мс ≤ 5 fps).
    pub fn tick(&mut self, cx: &mut Context<'_, Self>) -> Duration {
        // R-dnd: drag закончился и НИ ОДНА зона не поглотила его своим
        // on_drop (hover hit-test — работает для drag из любого места,
        // в отличие от on_drag_move, который срабатывает только у
        // элемента, где drag начался) — отпускание вне зон:
        // подтверждение выброса (заказчик).
        // R-dnd: drag закончился и НИ ОДНА зона не поглотила его своим
        // on_drop (hover hit-test — работает для drag из любого места,
        // в отличие от on_drag_move, который срабатывает только у
        // элемента, где drag начался) — отпускание вне зон:
        // подтверждение выброса (заказчик).
        if let Some(drag) = self.drag_item.take() {
            if !cx.has_active_drag() {
                tracing::info!("dnd: конец drag вне зон → подтверждение выброса");
                // Стак: max — сколько в стаке (1 — одиночный).
                let found = self
                    .store
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.char.inventory.iter().find(|i| i.uid == drag.uid))
                    .map(|i| (i.name.clone(), i.qty.unwrap_or(1).max(1)));
                let (name, max) = found.unwrap_or_else(|| (drag.uid.clone(), 1));
                self.drop_confirm = Some(DropPending {
                    uid: drag.uid.clone(),
                    name,
                    from: drag.from,
                    max,
                    qty: max,
                });
                self.bp_hold = None;
                cx.notify();
            } else {
                self.drag_item = Some(drag);
            }
        }
        let mut had_events = false;
        for _ in 0..64 {
            match self.ingress.try_recv() {
                Ok(ev) => {
                    had_events = true;
                    let effects = self.store.apply(ev);
                    self.exec_effects(effects);
                }
                Err(_) => break,
            }
        }

        // R16 (§1.1): перекалибровка часов каждые 5 мин — сервер
        // отвечает evt/time.sync; медиану держит стор.
        let recalibrate = self
            .last_time_sync
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(300))
            .unwrap_or(true);
        if recalibrate {
            self.last_time_sync = Some(std::time::Instant::now());
            let _ = self.transport.send(&TimeSync {});
        }
        // M23: лут трупа обновился (разделка/создание трупа) —
        // перечитываем, пока окно подятия открыто.
        if self.store.pickup_dirty {
            self.store.pickup_dirty = false;
            if self.pickup_popup.open {
                self.pickup_request = true;
            }
        }
        // M23: очередь «Взять всё» — по одному взятию за свободный
        // ack-слот; лут окна перечитывается после неё.
        if !self.pickup_queue.is_empty() {
            self.pump_take_queue(cx);
        }
        // M23: окно подятия — догрузка/обновления лута, когда
        // транспорт свободен (см. `pickup_request`).
        if self.pickup_request && self.pickup_queue.is_empty() {
            self.pump_pickup(cx);
        }
        // M24: админ-панель — догрузка каталога и содержимого цели
        // при свободном ack-слоте.
        if self.admin_catalog_request {
            self.pump_admin_catalog(cx);
        }
        // M26: карта мира — одна догрузка чанков на сессию (насос;
        // apply_ack ставит map_request на следующий чанк).
        if !self.map_started && self.store.screen == Screen::Game {
            self.map_started = true;
            self.store.map_request = true;
        }
        if self.store.map_request {
            self.pump_map_world(cx);
        }
        // M26: карта загружена целиком → компасная раскладка (один раз).
        if !self.map_pos_ready
            && self.store.map_total > 0
            && self.store.map_offset >= self.store.map_total
        {
            self.map_pos = crate::map::layout(&self.store.map_locations, &self.store.map_root);
            self.map_pos_ready = true;
            tracing::info!("dnd/карта: раскладка {} узлов", self.map_pos.len());
            cx.notify();
        }
        if self.admin_target_request {
            self.pump_admin_target(cx);
        }

        // Результат фоновой проверки обновлений (§12).
        while let Ok(version) = self.update_rx.try_recv() {
            if ld2_core::update_check::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                tracing::info!("доступна версия v{version}");
                self.update_available = Some(version);
            }
        }

        // M11.7: таймер удаления (D20): тикает только на экране персонажей;
        // вход в игру/логаут (смена экрана) → отмена; истёк → char.delete.
        if self.store.pending_delete.is_some() {
            if self.store.screen != Screen::Characters {
                self.store.cancel_delete();
            } else if let Some(char_id) = self.store.tick_delete(local_epoch()) {
                let cmd = CharDelete {
                    char_id: char_id.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_delete_sent(&char_id),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            // Отсчёт на карточке — перерисовка каждый тик.
            cx.notify();
        }

        // M14/F16: таймер выхода — полоса анимируется (тик 50 мс),
        // по истечении 20 с действие выполняется автоматически.
        let exit_active = self.exit_timer.is_some();
        if let Some((kind, deadline)) = self.exit_timer {
            if self.now_ms() >= deadline {
                self.perform_exit(kind);
            }
        }

        // M17/F30a: каст-бар инкрустации (3 с, без отмены) — по
        // завершении отправляем socket.insert в первый свободный сокет.
        let inscribe_active = self.cast_inscribe.is_some();
        if let Some((rune_uid, target_uid, ends)) = self.cast_inscribe.clone() {
            if self.now_ms() >= ends {
                self.cast_inscribe = None;
                self.finish_inscribe(rune_uid, target_uid);
            }
        }

        // M18/F53: прогресс улучшения в диалоге — по истечении 3 с
        // (из настроек сервера) отправляем cmd/upgrade.start; модалка
        // закроется по evt/upgrade/result.
        let upgrade_progress_active = self.upgrade_progress.is_some();
        // M22: прогресс-бар разделки (5 с) должен идти плавно —
        // держим быстрый тик (50 мс) с notify, пока он активен.
        let harvest_active = self.store.harvest.is_some();
        // M24: прогрессбары зарядов рун живут по времени — тик,
        // пока хотя бы одна руна в надетом снаряжении не заполнена.
        let rune_bar_active = self.rune_bar_active();
        // M24: индикаторы бафов рун — живой отсчёт и моргание.
        let rune_buffs_active = self.rune_buffs_active();
        // M25: бары атак монстров — живая заливка до следующей атаки.
        let attack_bar_active = self.attack_bars_active();
        // M25: стан истёк — закрываем всплывающее окно.
        let stun_expired = self
            .store
            .stun
            .as_ref()
            .is_some_and(|s| self.now_ms() >= s.until_ms);
        if stun_expired {
            self.store.stun = None;
        }
        // M25: журнал локации исчезающий — строки старше TTL сервера
        // (`journal_ttl_s`, по умолчанию 30 с) удаляются.
        let log_ttl_s = self
            .store
            .session
            .as_ref()
            .map(|s| s.journal_ttl_s)
            .unwrap_or(30);
        let log_expired = self.store.expire_log(local_epoch_ms(), log_ttl_s);
        // M24 фикс: 4-й элемент — **длительность** (док поля), но
        // сравнивался с epoch-мс как дедлайн → старт уходил мгновенно
        // и прогресс-бар заточки не был виден. Теперь: старт + длительность.
        if let Some((uid, preview_id, started, duration)) = self.upgrade_progress.clone() {
            // M18/F54: камень мог выбыть за 3 с прогресса — старт без
            // него сервер отверг бы; сбрасываем, ничего не отправляя.
            if self.store.sharpen_stone.is_none() {
                tracing::debug!("прогресс заточки сброшен: камень выбыл");
                self.upgrade_progress = None;
            } else if self.now_ms() >= started + duration {
                self.upgrade_progress = None;
                let cmd = ld2_protocol::cmd::UpgradeStart {
                    item_uid: uid,
                    preview_id,
                };
                self.send_cmd(&cmd, Act::Upgrade);
            }
        }
        // Если модалка закрыта (Esc/✕) — сбрасываем прогресс.
        if self.store.upgrade.is_none() && self.store.sharpen_stone.is_none() {
            self.upgrade_progress = None;
        }

        // M24: единый прогресс-бар левой колонки — гаснет по
        // исчезновению своего источника (результат/отказ/закрытие).
        if let Some(p) = self.cast_progress.as_ref() {
            let gone = match p.source {
                ProgressSource::Harvest => self.store.harvest.is_none(),
                ProgressSource::Inscribe => self.cast_inscribe.is_none(),
                ProgressSource::Sharpen => self.upgrade_progress.is_none(),
            };
            if gone {
                self.cast_progress = None;
            }
        }

        // M18/F53: после результата модалка остаётся открытой —
        // автоматически запрашиваем новое превью для следующего улучшения.
        if self.store.sharpen_stone.is_some()
            && self.store.pending != Some(Act::Preview)
            && self
                .store
                .upgrade
                .as_ref()
                .is_some_and(|v| v.result.is_some() && v.preview.is_none())
        {
            let uid = self.store.upgrade.as_ref().unwrap().item_uid.clone();
            let cmd = ld2_protocol::cmd::UpgradePreview { item_uid: uid };
            self.send_cmd(&cmd, Act::Preview);
        }

        // M11.4: новый notice → всплывающий тост Ely (текст в экране
        // остаётся до замены — на время жизни тоста возможен дубль).
        if let Some(notice) = self.store.notice.clone() {
            if self.last_notice.as_deref() != Some(notice.as_str()) {
                self.last_notice = Some(notice.clone());
                let toast = Toast::new(notice);
                self.toaster.update(cx, |t, cx| {
                    t.push(toast, cx);
                });
            }
        } else {
            self.last_notice = None;
        }

        // M14/F4: пока есть активный кулдаун (глобальный или любой
        // собственный) — тик 50 мс, чтобы отсчёт на кнопках шёл плавно.
        let now_ms = self.now_ms();
        let cd_active = self.store.global_cooldown_left(now_ms) > 0
            || self
                .store
                .combat_cooldowns
                .values()
                .any(|until_s| until_s.saturating_mul(1000) > now_ms)
            // M17/F30g: анимация прогресс-баров предметных кнопок.
            || self
                .store
                .item_cooldowns
                .values()
                .any(|(until_ms, _)| *until_ms > now_ms)
            // M18/F42: живой отсчёт таймеров реплик на кулдауне.
            || self.store.npc_dialog.as_ref().is_some_and(|dialog| {
                dialog.node.as_ref().is_some_and(|node| {
                    node.replies.iter().any(|reply| {
                        reply
                            .wait_until_ms
                            .is_some_and(|until| until > now_ms)
                    })
                })
            })
            // M18/F53: прогресс улучшения в диалоге.
            || self.upgrade_progress.is_some();

        if had_events
            || cd_active
            || exit_active
            || inscribe_active
            || upgrade_progress_active
            || harvest_active
            || rune_bar_active
            || rune_buffs_active
            || attack_bar_active
            || stun_expired
            || log_expired
        {
            // gpui перерисовывается только по notify (аналог request_repaint).
            cx.notify();
        }
        Duration::from_millis(
            if had_events
                || cd_active
                || exit_active
                || inscribe_active
                || upgrade_progress_active
                || harvest_active
                || rune_bar_active
                || rune_buffs_active
                || attack_bar_active
            {
                50
            } else {
                200
            },
        )
    }

    /// Статус-бар экранов логина/выбора персонажа (заказчик): в центре —
    /// автор и версия клиента, справа — статус сервера.
    #[allow(unused_variables)] // cx — паритет сигнатуры статус-баров
    fn meta_status_bar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let status = self.store.transport.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(rgb(pal.border))
            .flex_shrink_0()
            .child(div().flex_grow_1())
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(tf(
                        lang,
                        Key::StatusBarAuthor,
                        &[("{author}", AUTHOR), ("{v}", env!("CARGO_PKG_VERSION"))],
                    )),
            )
            .child(div().flex_grow_1())
            .child(status_badge(pal, lang, &status, self.store.rtt_ms))
    }

    /// R-dnd: слот dragged-предмета (для подсветки зон).
    /// R-dnd: окно подтверждения выброса (Dialog, как выход).
    pub(crate) fn drop_confirm_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let pending = self.drop_confirm.clone()?;
        let lang = self.lang;
        let title = t(lang, Key::DropConfirmTitle).to_string();
        let close = self.dismiss(cx, |root| root.drop_confirm = None);
        let drop_btn = act_btn(
            cx,
            t(lang, Key::MenuDrop),
            pal.danger,
            0xffffff,
            |this, _cx| {
                if let Some(p) = this.drop_confirm.take() {
                    let cmd = ld2_protocol::cmd::ItemDrop {
                        item_uid: p.uid,
                        qty: None,
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => this.store.on_sheet_sent(Act::Drop),
                        Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
            },
        );
        let cancel = act_btn(
            cx,
            t(lang, Key::CancelBtn),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.drop_confirm = None;
            },
        );
        // Стак >1: ввод количества (цифры), ползунок, «Все»,
        // подтверждение шлёт ItemDrop с выбранным qty (заказчик).
        let mut body = div().flex().flex_col().gap_3();
        body = body.child(div().child(tf(
            lang,
            Key::DropConfirmHint,
            &[("{name}", pending.name.as_str())],
        )));
        if pending.max > 1 {
            let max = pending.max as f64;
            let qty = pending.qty.clamp(1, pending.max) as f64;
            let this = cx.entity();
            body = body.child(
                div().w(px(220.0)).child(
                    NumberInput::new("ld2-drop-qty", qty)
                        .precision(0)
                        .step(1.0)
                        .range(1.0, max)
                        .on_change(move |v, _window, acx| {
                            this.update(acx, |root, cx| {
                                if let Some(p) = root.drop_confirm.as_mut() {
                                    p.qty = (v.round() as i64).clamp(1, p.max);
                                }
                                cx.notify();
                            });
                        }),
                ),
            );
            let this = cx.entity();
            body = body.child(
                Slider::new("ld2-drop-qty-slider", qty)
                    .range(1.0, max)
                    .step(1.0)
                    .on_change(move |v, _window, acx| {
                        this.update(acx, |root, cx| {
                            if let Some(p) = root.drop_confirm.as_mut() {
                                p.qty = (v.round() as i64).clamp(1, p.max);
                            }
                            cx.notify();
                        });
                    }),
            );
            // «Все» — мгновенно максимум (заказчик).
            body = body.child(div().flex().flex_row().gap_2().child(act_btn(
                cx,
                t(lang, Key::QtyAll),
                pal.panel,
                pal.text,
                move |this, cx| {
                    // this — уже Root в обновлении; реентерантный
                    // entity.update себя же = краш.
                    if let Some(p) = this.drop_confirm.as_mut() {
                        p.qty = p.max;
                    }
                    cx.notify();
                },
            )));
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(cancel)
                .child(drop_btn),
        );
        Some(
            Dialog::new("ld2-drop-confirm", title, close)
                .child(body)
                .into_any_element(),
        )
    }

    fn exec_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::PersistSession {
                    login,
                    refresh_token,
                } => {
                    if login.trim().is_empty() {
                        continue;
                    }
                    // R5: запись с таймаутом — диалог macOS не вешает UI.
                    if let Err(e) = ld2_core::secrets::set_token_with_timeout(
                        &self.config.host,
                        &login,
                        &refresh_token,
                        std::time::Duration::from_secs(2),
                    ) {
                        tracing::warn!("keyring: не удалось сохранить токен: {e}");
                    }
                    // M16/F29j: запомненный пароль — только в keyring.
                    // Стэш пуст при resume (вход по токену) — старую
                    // запись не трогаем; выключенная галочка её стирает.
                    let pending = self.pending_password.take();
                    if self.config.remember_password {
                        if let Some(pw) = pending {
                            if let Err(e) = ld2_core::secrets::set_password_with_timeout(
                                &self.config.host,
                                &login,
                                &pw,
                                std::time::Duration::from_secs(2),
                            ) {
                                tracing::warn!("keyring: не удалось сохранить пароль: {e}");
                            }
                        }
                    } else if pending.is_some() {
                        if let Err(e) = ld2_core::secrets::delete_password_with_timeout(
                            &self.config.host,
                            &login,
                            std::time::Duration::from_secs(2),
                        ) {
                            tracing::warn!("keyring: не удалось удалить пароль: {e}");
                        }
                    }
                    self.config.last_login = Some(login);
                    if let Err(e) = self.config_store.save(&self.config) {
                        tracing::warn!("не удалось сохранить конфиг: {e:#}");
                    }
                }
                Effect::ForgetSession { login } => {
                    // R5: удаление с таймаутом — диалог macOS не вешает UI.
                    match ld2_core::secrets::delete_token_with_timeout(
                        &self.config.host,
                        &login,
                        std::time::Duration::from_secs(2),
                    ) {
                        Ok(()) | Err(keyring::Error::NoEntry) => {
                            tracing::info!("keyring: токен удалён ({login})");
                        }
                        Err(e) => tracing::warn!("keyring: не удалось удалить токен: {e}"),
                    }
                }
                Effect::TransportToken(token) => {
                    if let Err(e) = self.transport.set_refresh_token(Some(token)) {
                        tracing::warn!("транспорт недоступен для ротации токена: {e}");
                    }
                }
                Effect::ResyncState => {
                    // R11: дельта неприменима/фатальная ошибка — полный
                    // снапшот (§12.4); 0 → сервер отвечает целиком.
                    let version = self
                        .store
                        .snapshot
                        .as_ref()
                        .map(|s| s.state_version)
                        .unwrap_or(0);
                    let _ = self.transport.send(&StateResync {
                        last_state_version: version,
                    });
                }
                Effect::Notify(kind) => {
                    // M18/F55: итог заточки рисуется цветом прямо на
                    // экране заточки — системное уведомление («новое
                    // окно») для него убрано. Остальные события (D7) —
                    // под флагом desktop.notify.
                    if matches!(kind, ld2_core::store::NotifyKind::Upgrade) {
                        continue;
                    }
                    if self.config.notify {
                        ld2_core::notify::show(&l10n::notify_body(self.lang, kind));
                    }
                }
            }
        }
    }

    // ── действия ───────────────────────────────────────────────────────────

    fn send_login(&mut self, cx: &Context<'_, Self>) {
        let login = self.ti(&self.login_input).read(cx).text().to_string();
        let password = self.ti(&self.password_input).read(cx).text().to_string();
        if login.trim().is_empty() || password.is_empty() {
            self.store.login_error = Some(t(self.lang, Key::ErrFillFields).to_string());
            return;
        }
        // M16/F29j: пароль до move в cmd — для keyring при успешном
        // входе (PersistSession).
        self.pending_password = Some(password.clone());
        let cmd = AuthLogin {
            login: login.clone(),
            password,
        };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_login_sent(&login),
            Err(e) => self.store.login_error = Some(format!("нет связи: {e}")),
        }
    }

    /// M17/F30a: начало каста инкрустации — окно закрывается, полоса
    /// идёт 3 с без отмены; повторный каст во время активного не стартует.
    pub(crate) fn start_inscribe(&mut self, rune_uid: String, target_uid: String) {
        if self.cast_inscribe.is_some() {
            return;
        }
        self.store.close_inscribe();
        // M24: единый прогресс-бар — подпись «Действие · имя».
        let target = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == target_uid))
            .map(|i| i.name.clone())
            .unwrap_or_default();
        self.cast_inscribe = Some((rune_uid, target_uid, self.now_ms() + INSCRIBE_MS));
        self.start_progress(
            ProgressSource::Inscribe,
            format!("{} · {}", t(self.lang, Key::InscribeCasting), target),
            INSCRIBE_MS,
        );
    }

    /// M24: заряды рун в надетом снаряжении ниже максимума (режим
    /// `time`) — тик 50 мс, пока прогрессбары заполняются.
    fn rune_bar_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let Some(session) = self.store.session.as_ref() else {
            return false;
        };
        if session.magic.rune_recharge != "time" {
            return false;
        }
        let equipped: std::collections::HashSet<&str> = snap
            .char
            .equip
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        snap.char
            .inventory
            .iter()
            .filter(|i| equipped.contains(i.uid.as_str()))
            .flat_map(|i| i.sockets.iter().flatten())
            .any(|s| s.rune.as_ref().is_some_and(|r| r.charges < r.max))
    }

    /// M25: есть ли в локации монстры с непрожёванным баром атаки —
    /// тик50 мс, чтобы заливка шла плавно.
    fn attack_bars_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let now = self.now_ms();
        snap.location.entities.iter().any(|e| {
            e.kind == ld2_protocol::types::EntityKind::Monster
                && e.next_attack_at.unwrap_or(0) > now
                && e.attack_interval.unwrap_or(0) > 0
        })
    }

    /// M24: активны ли бафы рун — тик для отсчёта остатка и
    /// моргания на последних3 секундах.
    fn rune_buffs_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let now = self.now_ms() / 1000;
        filter_active_buffs(&snap.char.buffs, now)
            .iter()
            .any(|b| b.kind.starts_with("rune_"))
    }

    /// M24: запуск единого прогресс-бара левой колонки — подпись
    /// «Действие · имя» и длительность действия; гаснет в `tick` по
    /// исчезновению [`ProgressSource`]-источника.
    pub(crate) fn start_progress(
        &mut self,
        source: ProgressSource,
        label: String,
        duration_ms: i64,
    ) {
        self.cast_progress = Some(CastProgress {
            source,
            label,
            started_ms: self.now_ms(),
            duration_ms: duration_ms.max(1),
        });
    }

    /// M17/F30a: каст завершён — cmd/socket.insert в первый свободный
    /// сокет (индекс по свежему снапшоту; сервер валидирует — занятый
    /// слот/нет сокетов → тост через notice).
    fn finish_inscribe(&mut self, rune_uid: String, target_uid: String) {
        // M18/F58f (баг «не работает инкрустация через меню камня»):
        // слот — первый свободный, **принимающий тип вставки**
        // (accepts, ADR-012): иначе самоцвет шёл в рунный слот и
        // сервер отвечал «слот принимает только руны».
        let snapshot = self.store.snapshot.clone();
        let insert_type = snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == rune_uid))
            .map(|i| i.item_type.clone());
        let index = snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == target_uid))
            .and_then(|item| item.sockets.as_ref())
            .and_then(|sockets| {
                insert_type
                    .as_deref()
                    .and_then(|t| sheet::first_compatible_socket(sockets, t))
            });
        let Some(index) = index else {
            self.store.notice = Some(t(self.lang, Key::InscribeNoWeapons).to_string());
            return;
        };
        let cmd = ld2_protocol::cmd::SocketInsert {
            staff_uid: target_uid,
            socket_index: index as u32,
            rune_uid,
        };
        self.send_cmd(&cmd, Act::Socket);
    }

    fn logout(&mut self) {
        let _ = self.transport.send(&ld2_protocol::cmd::AuthLogout {});
        let effects = self.store.logout();
        self.exec_effects(effects);
    }

    /// Смена хоста в настройках (ФР-19): переподключение транспорта.
    /// Токены keyring привязаны к адресу — на новом сервере чистый вход.
    fn reconnect(&mut self) {
        tracing::info!("смена сервера → переподключение: {}", self.config.host);
        self.transport.close();
        let (tx, rx) = mpsc::unbounded_channel();
        self.transport = transport::spawn(
            TransportConfig::new(self.config.host.clone()).with_domain(self.config.domain.clone()),
            tx,
        );
        self.ingress = rx;
        self.store.to_login_screen();
    }

    /// Поле, созданное на первом кадре (паникующий доступ: render всегда
    /// идёт после инициализации).
    pub(crate) fn ti<'s>(&self, slot: &'s Option<Entity<TextInput>>) -> &'s Entity<TextInput> {
        slot.as_ref().expect("поле создано на первом кадре")
    }

    /// M11.3: поля Ely создаются на первом кадре — `TextInput::new` требует
    /// Window. Enter в логине/чате = отправка через InputEvent::Submit
    /// (поллинг take_submit упразднён); пароль — masked (отклонение M10 снято).
    fn init_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let lang = self.lang;
        let last_login = self.config.last_login.clone();
        let host = self.config.host.clone();
        let mk = |window: &mut Window,
                  cx: &mut Context<'_, Self>,
                  placeholder: String,
                  masked: bool,
                  initial: Option<String>| {
            cx.new(|cx| {
                let mut i = TextInput::new(window, cx).placeholder(placeholder);
                if masked {
                    i = i.masked();
                }
                if let Some(text) = initial {
                    i.set_text(text, cx);
                }
                i
            })
        };
        self.login_input = Some(mk(
            window,
            cx,
            t(lang, Key::FieldLogin).to_string(),
            false,
            last_login,
        ));
        self.password_input = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword).to_string(),
            true,
            // M16/F29j: при включённой галочке «Запомнить пароль»
            // пароль уже введён (чтение keyring — в main.rs, один
            // раз при старте).
            if self.config.remember_password {
                self.prefill_password.take()
            } else {
                None
            },
        ));
        self.host_input = Some(mk(window, cx, host.clone(), false, Some(host)));
        // M19 (§15): поле домена discovery (пусто — прямой режим).
        let domain = self.config.domain.clone().unwrap_or_default();
        self.domain_input = Some(mk(window, cx, domain.clone(), false, Some(domain)));
        self.guild_input = Some(mk(window, cx, "…".into(), false, None));
        self.create_name = Some(mk(
            window,
            cx,
            t(lang, Key::FieldCharName).to_string(),
            false,
            None,
        ));
        // M13.3: поля регистрации (пароль и подтверждение — masked).
        self.reg_login = Some(mk(
            window,
            cx,
            t(lang, Key::FieldLogin).to_string(),
            false,
            None,
        ));
        self.reg_email = Some(mk(
            window,
            cx,
            t(lang, Key::FieldEmail).to_string(),
            false,
            None,
        ));
        self.reg_password = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword).to_string(),
            true,
            None,
        ));
        self.reg_password2 = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword2).to_string(),
            true,
            None,
        ));
        self.chat_input = Some(mk(window, cx, String::new(), false, None));
        self.chat_filter = Some(mk(
            window,
            cx,
            t(lang, Key::ChFilter).to_string(),
            false,
            None,
        ));
        self.admin_search = Some(mk(
            window,
            cx,
            t(lang, Key::AdminSearch).to_string(),
            false,
            None,
        ));
        self.admin_inv_search = Some(mk(
            window,
            cx,
            t(lang, Key::AdminInvSearch).to_string(),
            false,
            None,
        ));
        self.admin_player = Some(mk(
            window,
            cx,
            t(lang, Key::AdminPlayerName).to_string(),
            false,
            None,
        ));

        self.delete_name = Some(mk(
            window,
            cx,
            t(lang, Key::DeleteTypeNick).to_string(),
            false,
            None,
        ));
        for slot in [self.login_input.clone(), self.password_input.clone()] {
            let ent = slot.expect("создано выше");
            cx.subscribe(&ent, |root, _field, event: &InputEvent, cx| {
                if *event == InputEvent::Submit && root.store.screen == Screen::Login {
                    root.send_login(cx);
                }
            })
            .detach();
        }
        // Enter в полях регистрации — отправка (как в логине, §ФР-01).
        for slot in [
            self.reg_login.clone(),
            self.reg_email.clone(),
            self.reg_password.clone(),
            self.reg_password2.clone(),
        ] {
            let ent = slot.expect("создано выше");
            cx.subscribe(&ent, |root, _field, event: &InputEvent, cx| {
                if *event == InputEvent::Submit
                    && root.store.screen == Screen::Login
                    && root.register_open
                {
                    root.send_register(cx);
                }
            })
            .detach();
        }
        let chat = self.chat_input.clone().expect("создано выше");
        cx.subscribe(&chat, |root, _field, event: &InputEvent, cx| {
            if *event == InputEvent::Submit && root.store.screen == Screen::Game {
                root.send_chat(cx);
            }
        })
        .detach();
    }

    /// Есть ли фокус в текстовом поле **текущего экрана** (навигация
    /// не должна срабатывать при наборе текста).
    pub(crate) fn typing_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        TYPING_FIELDS
            .iter()
            .filter(|f| f.screen == self.store.screen)
            .filter_map(|f| (f.get)(self).as_ref())
            .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            // M24: поля execute-диалога (ввод количества) живут в
            // `dialog_inputs`, а не в TYPING_FIELDS — цифры не должны
            // стрелять хоткеями hotbar.
            || self.dialog_inputs.as_ref().is_some_and(|(_, fields)| {
                fields.iter().any(|(_, input)| {
                    input.read(cx).focus_handle(cx).is_focused(window)
                })
            })
    }

    /// M18/F41: снять фокус со всех полей ввода окна (`Window::blur`).
    ///
    /// gpui не blur'ит поле само: после набора текста фокус «залипал»
    /// в чате, и гейт [`Self::typing_focused`] глушил **все** глобальные
    /// хоткеи (C/I/квесты не открывались, в т.ч. параллельно). Прямой
    /// `handle.update` из обработчика ввода недоступен: событие
    /// обрабатывается внутри `update_window`, где окно временно снято
    /// с карты `App` (вернётся «window not found»). Откладываем blur
    /// до конца цикла эффектов — к `flush_effects` окно уже на месте.
    pub(crate) fn blur_typing(cx: &mut Context<'_, Self>) {
        cx.defer(|cx| {
            let Some(handle) = cx.active_window() else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| window.blur(cx));
        });
    }

    /// Обработка глобального нажатия: навигация/цикл целей по хоткеям
    /// из config (ФР-19); не срабатывает вне игры и при наборе текста.
    pub(crate) fn handle_keystroke(
        &mut self,
        ks: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // M18/F41: Esc в поле ввода = выход из чата (снять фокус),
        // иначе гейт ниже глушит хоткеи до следующего клика мышью.
        if ks.key == "escape" && self.typing_focused(window, cx) {
            tracing::debug!("Esc в поле ввода → снятие фокуса");
            window.blur(cx);
            cx.notify();
            return;
        }
        // Заказчик 2026-11-20: ↑/↓ в поле чата — листать историю ввода
        // (до гейта typing: фокус именно в поле чата).
        if ks.key == "up" || ks.key == "down" {
            let chat_focus = self
                .chat_input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
            if chat_focus {
                self.chat_history_nav(ks.key == "up", cx);
                cx.notify();
                return;
            }
        }
        let typing = self.typing_focused(window, cx);
        if self.store.screen != Screen::Game || typing {
            // Диагностика жалоб «хоткеи не работают»: ранний выход из-за
            // экрана или фокуса в поле ввода.
            tracing::debug!(
                "keystroke {:?} пропущен гейтом: screen={:?}, typing_focused={}",
                ks,
                self.store.screen,
                typing
            );
            return;
        }
        for (spec, dir) in [
            (self.config.hotkeys.nav_up.clone(), "north"),
            (self.config.hotkeys.nav_down.clone(), "south"),
            (self.config.hotkeys.nav_left.clone(), "west"),
            (self.config.hotkeys.nav_right.clone(), "east"),
        ] {
            if keystroke_matches(&spec, ks) {
                self.nav_direction(dir);
                cx.notify();
                return;
            }
        }
        if keystroke_matches(&self.config.hotkeys.cycle_target, ks) {
            self.cycle_target();
            cx.notify();
            return;
        }
        // M15/F25/M16: Esc закрывает верхнее из окон — но не поверх
        // настроек/диалога выхода (они ловят Esc сами).
        if !self.settings_open && self.exit_timer.is_none() && ks.key == "escape" {
            // M23: окно подятия — верхний слой группы попапов (рисуется
            // последним поверх всех попапов): Esc закрывает его первым.
            if self.pickup_popup.open {
                tracing::debug!("закрытие окна подятия: Esc");
                self.close_pickup();
                cx.notify();
                return;
            }
            // M18/F54: окно заточки (список предметов или сама заточка) —
            // верхний слой группы окон; Esc закрывает оба экрана разом.
            if self.store.sharpen_stone.is_some() || self.store.upgrade.is_some() {
                tracing::debug!("закрытие окна заточки: Esc");
                self.store.close_upgrade();
                self.store.sharpen_stone = None;
                self.store.sharpen_list = false;
                self.upgrade_progress = None;
                cx.notify();
                return;
            }
            // M20: окно количества — верхний слой.
            if self.quantity_pick.is_some() {
                self.quantity_pick = None;
                self.quantity_popup.open = false;
                cx.notify();
                return;
            }
            // M18: окно сокетов — верхний слой группы попапов; Esc
            // сначала возвращает со второй страницы к списку слотов.
            if self.store.socketing.is_some() {
                if self.socket_pick.is_some() {
                    self.socket_pick = None;
                    cx.notify();
                    return;
                }
                tracing::info!("закрытие окна сокетов: Esc");
                self.store.close_socketing();
                self.socket_popup.open = false;
                cx.notify();
                return;
            }
            // M18/F53: описание предмета — поверх окна заточки.
            if self.store.item_detail.is_some() {
                tracing::debug!("закрытие описания предмета: Esc");
                self.store.item_detail = None;
                cx.notify();
                return;
            }
            // M20: игровой диалог — верхний слой (заготовка).
            if self.store.dialog.is_some() {
                tracing::debug!("закрытие игрового диалога: Esc");
                self.close_game_dialog();
                cx.notify();
                return;
            }
            // M21: окно профессий (заготовка сброса гаснет вместе).
            if self.professions_popup.open {
                tracing::debug!("закрытие окна профессий: Esc");
                self.professions_popup.open = false;
                self.prof_reset_armed = None;
                cx.notify();
                return;
            }
            // M24: админ-панель — ниже диалога/профессий в стеке слоёв.
            if self.admin_popup.open {
                tracing::debug!("закрытие админ-панели: Esc");
                self.close_admin();
                cx.notify();
                return;
            }
            // M18/F38: диалог NPC — верхний слой группы окон.
            if self.store.npc_dialog.is_some() {
                tracing::debug!("закрытие диалога NPC: Esc");
                self.store.npc_dialog = None;
                self.npc_popup.open = false;
                cx.notify();
                return;
            }
            // M18/F36: квесты рисуются поверх рюкзака — Esc закрывает
            // верхнее (после рассуждений порядок: диалог → квесты →
            // рюкзак → персонаж).
            if self.quests_popup.open {
                self.quests_popup.open = false;
                cx.notify();
                return;
            }
            if self.backpack_popup.open {
                self.backpack_popup.open = false;
                cx.notify();
                return;
            }
            if self.char_popup.open {
                self.char_popup.open = false;
                cx.notify();
                return;
            }
        }
        // M15/F25: хоткей окна персонажа (кнопка в шапке дублирует);
        // M16/F29b: окна открываются одновременно (общий scrim).
        if keystroke_matches(&self.config.hotkeys.open_character, ks) {
            tracing::debug!("hotkey окна персонажа → open={}", !self.char_popup.open);
            if self.char_popup.open {
                self.char_popup.open = false;
            } else {
                // M16/F29d: базовый размер + первая вкладка.
                self.open_popup(PopupKind::Character);
            }
            cx.notify();
            return;
        }
        // M16/F29b: хоткей рюкзака (кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_backpack, ks) {
            if self.backpack_popup.open {
                self.backpack_popup.open = false;
            } else {
                self.open_popup(PopupKind::Backpack);
            }
            cx.notify();
            return;
        }
        // M18/F36: хоткей окна квестов (кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_quests, ks) {
            if self.quests_popup.open {
                self.quests_popup.open = false;
            } else {
                self.open_popup(PopupKind::Quests);
            }
            cx.notify();
            return;
        }
        // M21: хоткей окна профессий («p» — кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_professions, ks) {
            if self.professions_popup.open {
                self.professions_popup.open = false;
            } else {
                self.open_popup(PopupKind::Professions);
            }
            self.prof_reset_armed = None;
            cx.notify();
            return;
        }
        // M14/F4: боевые действия панели — те же действия, что кнопки
        // (цифры 1–6; Esc занят диалогами → «Побег» = 4).
        if self.action_hotkey(ks, cx) {
            cx.notify();
        }
    }

    /// M14/F4: боевой хоткей → то же действие, что кнопка панели.
    /// Во время кулдауна действие не отправляется (молча — как
    /// disabled-кнопка с тултипом). Возвращает true, если спецификация
    /// совпала с одним из боевых действий.
    fn action_hotkey(&mut self, ks: &gpui::Keystroke, cx: &mut Context<'_, Self>) -> bool {
        #[derive(Clone, Copy)]
        enum Action {
            Attack,
            PowerStrike,
            Cleave,
            Flee,
            Rune,
            Consumable,
        }
        let hk = self.config.hotkeys.clone();
        let action = if keystroke_matches(&hk.action_attack, ks) {
            Action::Attack
        } else if keystroke_matches(&hk.action_power_strike, ks) {
            Action::PowerStrike
        } else if keystroke_matches(&hk.action_cleave, ks) {
            Action::Cleave
        } else if keystroke_matches(&hk.action_flee, ks) {
            Action::Flee
        } else if keystroke_matches(&hk.action_rune, ks) {
            Action::Rune
        } else if keystroke_matches(&hk.action_consumable, ks) {
            Action::Consumable
        } else {
            return false;
        };

        // Глобальный кулдаун (1 с) блокирует любое боевое действие.
        let now = self.now_ms();
        if self.store.global_cooldown_left(now) > 0 {
            return true;
        }

        match action {
            Action::Attack => {
                if let Some(id) = self.store.target.clone() {
                    self.on_click(Click::Attack(id), cx);
                }
            }
            Action::PowerStrike => {
                // Собственный кулдаун активки + гейт навыка (§4).
                let own = self.own_cd_left_ms("power_strike", now);
                let open = self
                    .store
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| game::power_strike_open(&s.char.skills));
                if AppStore::action_ready(own, 0).0 && open {
                    self.on_click(Click::PowerStrike, cx);
                }
            }
            // Cleave не реализован: кнопка скрыта, хоткей молчит.
            Action::Cleave => {}
            Action::Flee => self.on_click(Click::Flee, cx),
            Action::Rune => {
                if let Some(click) = self.first_rune_click() {
                    self.on_click(click, cx);
                }
            }
            Action::Consumable => {
                if let Some(uid) = self.first_consumable_uid() {
                    self.on_click(Click::UseItem(uid), cx);
                }
            }
        }
        true
    }

    /// Навигация по направлению (ФР-03; хоткеи из config — ФР-19).
    pub(crate) fn nav_direction(&mut self, dir: &str) {
        let Some(exit) = self.store.snapshot.as_ref().and_then(|s| {
            s.location
                .exits
                .iter()
                .find(|e| e.dir.as_deref() == Some(dir))
                .map(|e| e.id.clone())
        }) else {
            return;
        };
        let cmd = ld2_protocol::cmd::LocationMove { exit_id: exit };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_world_sent(Act::Move),
            Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
        }
    }

    /// M14/F14: свотч открывает/закрывает пикер этого цвета (один разом).
    fn toggle_color_picker(&mut self, key: &'static str) {
        self.color_picker_open = match self.color_picker_open {
            Some(open) if open == key => None,
            _ => Some(key),
        };
    }

    /// Цикл целей по Tab (ФР-05).
    pub(crate) fn cycle_target(&mut self) {
        self.store.cycle_target();
    }

    /// Текущее время (epoch, **мс**) с поправкой на часы сервера —
    /// база для отсчёта кулдаунов (M14/F4).
    pub(crate) fn now_ms(&self) -> i64 {
        local_epoch_ms() + self.store.time_offset * 1000
    }

    /// M14/F4: остаток собственного кулдауна действия, мс. Сервер
    /// шлёт `until` в секундах → граница секунды в мс, чтобы отсчёт
    /// на кнопке шёл плавно (тик 50 мс), а не дёргался раз в секунду.
    pub(crate) fn own_cd_left_ms(&self, skill: &str, now_ms: i64) -> i64 {
        let until_s = self.store.combat_cooldowns.get(skill).copied().unwrap_or(0);
        until_s.saturating_mul(1000) - now_ms
    }

    /// M16/F29b: состояние всплывающего окна по виду — оболочка
    /// `popup_window` и её жесты работают через этот доступ.
    fn popup_mut(&mut self, kind: PopupKind) -> &mut PopupState {
        match kind {
            PopupKind::Character => &mut self.char_popup,
            PopupKind::Backpack => &mut self.backpack_popup,
            PopupKind::Quests => &mut self.quests_popup,
            PopupKind::Npc => &mut self.npc_popup,
            PopupKind::Item => &mut self.item_popup,
            PopupKind::Upgrade => &mut self.upgrade_popup,
            PopupKind::Socketing => &mut self.socket_popup,
            PopupKind::Quantity => &mut self.quantity_popup,
            PopupKind::Dialog => &mut self.dialog_popup,
            PopupKind::Professions => &mut self.professions_popup,
            PopupKind::LootPickup => &mut self.pickup_popup,
            PopupKind::AdminPanel => &mut self.admin_popup,
        }
    }

    /// M16/F29b: чтение состояния всплывающего окна.
    fn popup(&self, kind: PopupKind) -> &PopupState {
        match kind {
            PopupKind::Character => &self.char_popup,
            PopupKind::Backpack => &self.backpack_popup,
            PopupKind::Quests => &self.quests_popup,
            PopupKind::Npc => &self.npc_popup,
            PopupKind::Item => &self.item_popup,
            PopupKind::Upgrade => &self.upgrade_popup,
            PopupKind::Socketing => &self.socket_popup,
            PopupKind::Quantity => &self.quantity_popup,
            PopupKind::Dialog => &self.dialog_popup,
            PopupKind::Professions => &self.professions_popup,
            PopupKind::LootPickup => &self.pickup_popup,
            PopupKind::AdminPanel => &self.admin_popup,
        }
    }

    /// M16/F29f: открытие окна — базовый размер (персонаж **выше**:
    /// 760×760; рюкзак 760×560), по центру с клампом под вьюпорт
    /// (drag/resize живут до закрытия) и первая вкладка.
    /// M18/F55: базовый размер карточки для `open_popup`.
    ///
    /// Окно описания предмета — **«резиновое»**: ширина считается доли
    /// вьюпорта (28 %, clamp `320..480`) вместо прежних фиксированных
    /// 520 px; высота остаётся константой, докрутит resize-grip.
    /// `clamp` также спасает нулевой viewport до первой отрисовки.
    /// Остальные окна — константы (как были).
    fn popup_base_size(kind: PopupKind, viewport: (f32, f32)) -> (f32, f32) {
        let (vw, _) = viewport;
        match kind {
            PopupKind::Character => (760.0, 760.0),
            PopupKind::Backpack => (760.0, 560.0),
            PopupKind::Quests => (800.0, 560.0),
            PopupKind::Npc => (640.0, 560.0),
            PopupKind::Item => ((vw * 0.28).clamp(320.0, 480.0), 420.0),
            PopupKind::Upgrade => (440.0, 360.0),
            PopupKind::Socketing => (440.0, 420.0),
            PopupKind::Quantity => (400.0, 280.0),
            PopupKind::Dialog => (560.0, 460.0),
            PopupKind::Professions => (880.0, 620.0),
            PopupKind::LootPickup => (1040.0, 600.0),
            PopupKind::AdminPanel => (1040.0, 640.0),
        }
    }

    /// M24: закрытие админ-панели; каталог остаётся в сторе — кэш
    /// на сессию (вариант A), чистится при выходе/смене персонажа.
    fn close_admin(&mut self) {
        self.admin_popup.open = false;
        self.admin_catalog_request = false;
        self.admin_target_request = false;
        // Цель и её содержимое не переживают закрытие (сущность могла
        // исчезнуть); каталог остаётся — кэш на сессию.
        self.admin_target = "self".into();
        self.store.admin_target_items = None;
    }

    /// M23: закрытие окна подятия — данные/выбор/запрос чистятся
    /// вместе с флагом (stale-лут не должен пережить закрытие).
    pub(crate) fn close_pickup(&mut self) {
        self.pickup_popup.open = false;
        self.store.pickup = None;
        self.pickup_sel = None;
        self.pickup_id = None;
        self.pickup_request = false;
        // Партия «Взять всё» и её подтверждение не переживают окно.
        self.pickup_queue.clear();
        self.pickup_confirm_all = false;
    }

    /// M23: запрос данных окна подятия (`cmd/entity.info` →
    /// `Act::PickupInfo`) — только при свободном `pending` (см.
    /// `pickup_request`); иначе ждёт следующего тика.
    fn pump_pickup(&mut self, cx: &mut Context<'_, Self>) {
        if !self.pickup_request || !self.pickup_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let Some(id) = self.pickup_id.clone() else {
            self.pickup_request = false;
            return;
        };
        let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_world_sent(Act::PickupInfo);
                self.pickup_request = false;
                cx.notify();
            }
            Err(e) => {
                self.pickup_request = false;
                self.store.world_error = Some(format!("нет связи: {e}"));
            }
        }
    }

    /// M23: очередь «Взять всё» — следующий `item.use`, когда
    /// транспорт свободен (корреляция ack-ов одиночная); после
    /// отправки ставится `pickup_request` — когда очередь опустеет,
    /// лут окна освежится обычным перезапросом.
    fn pump_take_queue(&mut self, cx: &mut Context<'_, Self>) {
        if self.pickup_queue.is_empty() || !self.pickup_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let uid = self.pickup_queue.remove(0);
        let cmd = ld2_protocol::cmd::ItemUse {
            item_uid: uid,
            target_id: None,
        };
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_world_sent(Act::Drop);
                self.pickup_request = true;
                cx.notify();
            }
            Err(e) => {
                self.pickup_queue.clear();
                self.store.world_error = Some(format!("нет связи: {e}"));
            }
        }
    }

    /// M26: догрузка карты мира — следующий чанк `cmd/map.world`
    /// при свободном `pending` (корреляция ack-ов стора одиночная);
    /// после ack apply_ack ставит map_request на продолжение.
    fn pump_map_world(&mut self, cx: &mut Context<'_, Self>) {
        if !self.store.map_request {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::MapWorld {
            offset: self.store.map_offset,
        };
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_world_sent(Act::MapWorld);
                self.store.map_request = false;
                cx.notify();
            }
            Err(e) => {
                self.store.map_request = false;
                self.store.notice = Some(format!("нет связи: {e}"));
            }
        }
    }

    /// M24: каталог админ-окна — `cmd/admin.items` при свободном
    /// `pending` (корреляция ack-ов стора одиночная), как у окна
    /// подятия; после отправки флаг гаснет.
    fn pump_admin_catalog(&mut self, cx: &mut Context<'_, Self>) {
        if !self.admin_catalog_request || !self.admin_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::AdminItems {};
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_world_sent(Act::AdminCatalog);
                self.admin_catalog_request = false;
                cx.notify();
            }
            Err(e) => {
                self.admin_catalog_request = false;
                self.store.notice = Some(format!("нет связи: {e}"));
            }
        }
    }

    /// M24: содержимое цели правой колонки — `cmd/admin.target.info`
    /// при свободном `pending` (как каталог); после отправки флаг гаснет.
    fn pump_admin_target(&mut self, cx: &mut Context<'_, Self>) {
        if !self.admin_target_request || !self.admin_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::AdminTargetInfo {
            target: self.admin_target.clone(),
        };
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_world_sent(Act::AdminTarget);
                self.admin_target_request = false;
                cx.notify();
            }
            Err(e) => {
                self.admin_target_request = false;
                self.store.notice = Some(format!("нет связи: {e}"));
            }
        }
    }

    fn open_popup(&mut self, kind: PopupKind) {
        // M18/F33/M24: чистое контекстное меню при каждом открытии —
        // оба меню (см. clear_popup_menu).
        clear_popup_menu(&mut self.bp_menu, &mut self.equip_menu);
        let (vw, vh) = self.viewport;
        let (tw, th) = Self::popup_base_size(kind, self.viewport);
        let (w, h) = if vw > 0.0 && vh > 0.0 {
            clamp_char_size(tw, th, vw, vh)
        } else {
            (tw, th)
        };
        let delta = match kind {
            PopupKind::Character => (0.0, 0.0),
            PopupKind::Backpack => (40.0, 40.0),
            PopupKind::Quests => (-40.0, 40.0),
            PopupKind::Npc => (60.0, -40.0),
            PopupKind::Item => (80.0, -20.0),
            PopupKind::Upgrade => (0.0, -40.0),
            PopupKind::Socketing => (0.0, -40.0),
            PopupKind::Quantity => (0.0, -60.0),
            PopupKind::Dialog => (0.0, -40.0),
            PopupKind::Professions => (0.0, -30.0),
            PopupKind::LootPickup => (40.0, -30.0),
            PopupKind::AdminPanel => (0.0, -20.0),
        };
        let st = self.popup_mut(kind);
        st.open = true;
        st.size = (w, h);
        st.delta = delta;
        st.drag_offset = None;
        st.resize_anchor = None;
        match kind {
            PopupKind::Character => self.sheet_tab = sheet::SheetTab::Params,
            PopupKind::Backpack => self.backpack_tab = backpack::BackpackTab::default(),
            // M18/F36: на «Текущих» при каждом открытии (паритет окон);
            // выбор квеста переживает закрытие — правая колонка
            // корректно откатывается на первой строке вкладки.
            PopupKind::Quests => self.quests_tab = quests::QuestsTab::default(),
            // M18/F38: окно диалога открывает ack, не open_popup.
            PopupKind::Npc => {}
            // M18: описание предмета открывает контекстное меню.
            PopupKind::Item => {}
            // M18/F53: окно заточки открывает контекстное меню.
            PopupKind::Upgrade => {}
            // M18: окно сокетов открывает контекстное меню.
            PopupKind::Socketing => {}
            // M20: окно количества — состояние уже установлено вызывающим.
            PopupKind::Quantity => {}
            // M20: окно диалога открывает `evt/dialog/open`.
            PopupKind::Dialog => {}
            // M21: окно профессий открывает кнопка/хоткей.
            PopupKind::Professions => {}
            // M23: окно подятия — выбор сбрасывает вызывающий.
            PopupKind::LootPickup => {}
            // M24: админ-панель — состояние (поиск/каталог) переживает
            // переоткрытия на сессию.
            PopupKind::AdminPanel => {}
        }
    }

    /// M16/F29d: запрос контекстного меню экипировки (левый/правый
    /// клик по предмету) — номер растёт, чтобы Ely переоткрыл меню.
    fn bump_equip_menu(
        &mut self,
        slot: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let n = self.equip_menu.as_ref().map(|(_, n, _)| *n).unwrap_or(0) + 1;
        self.equip_menu = Some((slot, n, at));
        cx.notify();
    }

    /// M16/F29g: запрос контекстного меню предмета рюкзака (левый и
    /// правый клик) — номер растёт, чтобы Ely переоткрыл меню.
    fn bump_bp_menu(
        &mut self,
        uid: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let n = self.bp_menu.as_ref().map(|(_, n, _)| *n).unwrap_or(0) + 1;
        self.bp_menu = Some((uid, n, at));
        cx.notify();
    }

    /// M16/F29h: системные уведомления — стеком вверху по центру.
    /// Тайминг и жизнь тостов — у Toaster (ely-стек показывает их
    /// внизу справа и не позиционируется — здесь только отрисовка).
    fn toast_stack(&self, window: &Window, cx: &Context<'_, Self>, pal: &Palette) -> Div {
        let toasts = self.toaster.read(cx).shown();
        if toasts.is_empty() {
            return div();
        }
        let width = 420.0f32;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(8.0);
        div()
            .absolute()
            .left(px(left))
            .top(px(16.0))
            .w(px(width))
            // Клики по уведомлению не проходят сквозь него (occlude).
            .occlude()
            .flex()
            .flex_col()
            .gap_2()
            .children(toasts.into_iter().map(|(title, _)| {
                div()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .border_1()
                    .border_color(rgb(pal.accent))
                    .text_size(px(13.0))
                    .text_center()
                    .child(title.to_string())
            }))
    }

    /// M16/F29b: общий scrim группы всплывающих окон — окна могут
    /// быть открыты одновременно (персонаж + рюкзак), поэтому scrim
    /// один на всех и лежит ПОД карточками; клик мимо закрывает
    /// верхнее открытое окно (рюкзак, затем персонаж).
    fn popup_scrim(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let npc_open = self.store.npc_dialog.is_some();
        let dialog_open = self.store.dialog.is_some();
        let professions_open = self.professions_popup.open;
        let admin_open = self.admin_popup.open;
        let pickup_open = self.pickup_popup.open;
        if !self.char_popup.open
            && !self.backpack_popup.open
            && !self.quests_popup.open
            && !npc_open
            && !dialog_open
            && !professions_open
            && !admin_open
            && !pickup_open
        {
            return None;
        }
        Some(
            div()
                .id("ld2-popup-scrim-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    div()
                        .id("ld2-popup-scrim")
                        .size_full()
                        .bg(rgba(0x99000000))
                        // M15/F25e: запоминаем, что жест начат на scrim.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|root, _, _, _| {
                                root.popup_scrim_down = true;
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|root, _, _, cx| {
                                let close =
                                    scrim_closes_on_up(root.popup_scrim_down, cx.has_active_drag());
                                root.popup_scrim_down = false;
                                if close {
                                    // Верхнее окно: подятие → сокеты → заточка →
                                    // описание → диалог → квесты → рюкзак →
                                    // персонаж (обратный порядок рендера).
                                    if root.pickup_popup.open {
                                        tracing::debug!("закрытие окна подятия: скрим");
                                        root.close_pickup();
                                    } else if root.quantity_pick.is_some() {
                                        root.quantity_pick = None;
                                        root.quantity_popup.open = false;
                                    } else if root.store.socketing.is_some() {
                                        if root.socket_pick.is_some() {
                                            root.socket_pick = None;
                                        } else {
                                            tracing::info!("закрытие окна сокетов: скрим");
                                            root.store.close_socketing();
                                            root.socket_popup.open = false;
                                        }
                                    } else if root.store.sharpen_stone.is_some()
                                        || root.store.upgrade.is_some()
                                    {
                                        root.store.close_upgrade();
                                        root.store.sharpen_stone = None;
                                        root.store.sharpen_list = false;
                                        root.upgrade_progress = None;
                                    } else if root.store.item_detail.is_some() {
                                        root.store.item_detail = None;
                                    } else if root.store.dialog.is_some() {
                                        tracing::debug!("закрытие игрового диалога: скрим");
                                        root.close_game_dialog();
                                    } else if root.professions_popup.open {
                                        tracing::debug!("закрытие окна профессий: скрим");
                                        root.professions_popup.open = false;
                                        root.prof_reset_armed = None;
                                    } else if root.admin_popup.open {
                                        tracing::debug!("закрытие админ-панели: скрим");
                                        root.close_admin();
                                    } else if root.store.npc_dialog.is_some() {
                                        tracing::debug!("закрытие диалога NPC: скрим");
                                        root.store.npc_dialog = None;
                                        root.npc_popup.open = false;
                                    } else if root.quests_popup.open {
                                        root.quests_popup.open = false;
                                    } else if root.backpack_popup.open {
                                        root.backpack_popup.open = false;
                                    } else if root.char_popup.open {
                                        root.char_popup.open = false;
                                    }
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config_store.save(&self.config) {
            tracing::warn!("не удалось сохранить конфиг: {e:#}");
        }
    }

    pub(crate) fn on_click(&mut self, click: Click, cx: &mut Context<'_, Self>) {
        match click {
            Click::Login => self.send_login(cx),
            Click::OpenRegister => {
                self.register_open = true;
                self.register_submitted = false;
                self.store.register_error = None;
            }
            Click::Logout => self.open_exit(ExitKind::Logout, cx),
            Click::Lang(code) => {
                self.lang = Lang::from_code(code);
                self.config.lang = self.lang.code().to_string();
                // Журнал переводит каналы по языку (заказчик 2026-11-20).
                self.store.lang = self.lang;
                self.save_config();
            }
            Click::ToggleSettings => {
                self.settings_open = !self.settings_open;
                if !self.settings_open {
                    self.color_picker_open = None;
                }
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host.clone(), cx));
                }
                let domain = self.config.domain.clone().unwrap_or_default();
                if let Some(input) = self.domain_input.clone() {
                    input.update(cx, |i, cx| i.set_text(domain.clone(), cx));
                }
            }
            Click::ToggleCharacter => {
                if self.char_popup.open {
                    self.char_popup.open = false;
                } else {
                    // M16/F29d: базовый размер + первая вкладка.
                    self.open_popup(PopupKind::Character);
                }
                cx.notify();
            }
            // M16/F29b: окно рюкзака (открывается вместе с персонажем).
            Click::ToggleInventory => {
                if self.backpack_popup.open {
                    self.backpack_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Backpack);
                }
                cx.notify();
            }
            // M18/F36: окно квестов (текущие/выполненные).
            Click::ToggleQuests => {
                if self.quests_popup.open {
                    self.quests_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Quests);
                }
                cx.notify();
            }
            Click::ToggleProfessions => {
                if self.professions_popup.open {
                    self.professions_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Professions);
                }
                self.prof_reset_armed = None;
                cx.notify();
            }
            Click::Theme(theme) => {
                self.config.theme = theme.to_string();
                // M11.5: тема Ely переключается вместе с config (ФР-19).
                Theme::set_mode_now(
                    if theme == "light" {
                        Mode::Light
                    } else {
                        Mode::Dark
                    },
                    cx,
                );
                self.save_config();
            }
            Click::ChatLimit(delta) => {
                let limit = (self.config.chat_limit as i64 + delta).clamp(100, 10_000) as usize;
                self.config.chat_limit = limit;
                self.store.chat_limit = limit;
                self.save_config();
            }
            Click::SetNotify(notify) => {
                self.config.notify = notify;
                self.save_config();
            }
            Click::SetMuteFilter(on) => {
                self.config.chat_mute_filter = on;
                self.save_config();
            }
            Click::EditHost => {
                // Предзаполнение текущим адресом (как при открытии
                // настроек) и раскрытие ввода.
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host, cx));
                }
                let domain = self.config.domain.clone().unwrap_or_default();
                if let Some(input) = self.domain_input.clone() {
                    input.update(cx, |i, cx| i.set_text(domain, cx));
                }
                self.host_editing = true;
            }
            Click::TogglePicker(key) => {
                self.toggle_color_picker(key);
            }
            Click::LogDamageBold(bold) => {
                self.config.log_damage_bold = bold;
                self.save_config();
            }
            Click::ApplyHost => {
                let host = self.ti(&self.host_input).read(cx).text().to_string();
                let host = host.trim().to_string();
                if !host_allowed(&host, RELEASE) {
                    self.store.notice = Some(t(self.lang, Key::SettingsWsDenied).to_string());
                    return;
                }
                // M19 (§15): домен discovery — пусто = прямой режим;
                // пробелы внутри — некорректный ввод (отказ до сохранения).
                let domain_raw = self.ti(&self.domain_input).read(cx).text().to_string();
                let domain_raw = domain_raw.trim().to_string();
                if !domain_raw.is_empty() && domain_raw.contains(char::is_whitespace) {
                    self.store.notice = Some(t(self.lang, Key::DiscBadDoc).to_string());
                    return;
                }
                let domain = (!domain_raw.is_empty()).then(|| domain_raw.clone());
                let changed = host != self.config.host || domain != self.config.domain;
                if changed {
                    self.config.host = host;
                    self.config.domain = domain;
                    self.save_config();
                    self.reconnect();
                }
                self.host_editing = false;
                self.settings_open = false;
            }
            Click::MoveExit(exit_id) => {
                let cmd = LocationMove { exit_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Move),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::OpenReleases => cx.open_url(ld2_core::update_check::RELEASES_PAGE),
            // ── M10.4: игровой экран ──
            Click::Attack(id) => {
                let cmd = ld2_protocol::cmd::CombatAttack { target_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Attack);
                        // M14/F4: любое боевое действие → глобальный кулдаун.
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Attack);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::Examine(id) => {
                let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Examine),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            // M23: ЛКМ по трупу — окно подятия (лут слева, рюкзак
            // справа); данные — отдельный `entity.info` → `pickup`.
            Click::LootOpen(id) => {
                self.pickup_id = Some(id);
                self.pickup_sel = None;
                // Лут прошлого трупа не должен мелькнуть под новым.
                self.store.pickup = None;
                // Без флага pump не отправит запрос, а рендер с пустыми
                // данными закроет окно в первом же кадре (баг M23).
                self.pickup_request = true;
                self.open_popup(PopupKind::LootPickup);
                self.pump_pickup(cx);
            }
            // M18/F35: диалог NPC — реплики/квесты вместо осмотра.
            Click::NpcTalk(id) => {
                tracing::debug!("вых. cmd/npc.talk {id}");
                let cmd = ld2_protocol::cmd::NpcTalk { entity_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::NpcTalk),
                    Err(e) => {
                        tracing::debug!("cmd/npc.talk не ушла: {e}");
                        self.store.world_error = Some(format!("нет связи: {e}"));
                    }
                }
            }
            Click::PartyInvite(player_id) => {
                let cmd = ld2_protocol::cmd::PartyInvite { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Party),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::TradeOpen(player_id) => {
                let cmd = ld2_protocol::cmd::TradeOpen { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Trade),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::RuneCast {
                staff_uid,
                socket_index,
            } => {
                let cmd = ld2_protocol::cmd::MagicRuneCast {
                    staff_uid,
                    socket_index,
                    target_id: self.store.target.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Cast);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::UseItem(uid) => {
                // M14/F4: расходник боя (свиток/зелье) запускает
                // глобальный кулдаун; лут и прочие предметы — нет.
                // M18/F54: камень усиления — отдельный ack (Act::Sharpen).
                let stone = self.store.snapshot.as_ref().and_then(|s| {
                    s.char
                        .inventory
                        .iter()
                        .find(|i| i.uid == uid && i.item_type == "upgrade_stone")
                        .map(|i| i.uid.clone())
                });
                let combat_item = self.store.snapshot.as_ref().is_some_and(|s| {
                    s.char.inventory.iter().any(|i| {
                        i.uid == uid && matches!(i.item_type.as_str(), "scroll" | "potion")
                    })
                });
                // M23: взятие из окна подятия (`loot:`/`inner:`) —
                // перезапрашиваем его лут, когда транспорт освободится.
                let is_loot = uid.starts_with("loot:") || uid.starts_with("inner:");
                let cmd = ld2_protocol::cmd::ItemUse {
                    item_uid: uid,
                    target_id: None,
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(if stone.is_some() {
                            Act::Sharpen
                        } else {
                            Act::Drop
                        });
                        if is_loot && self.pickup_popup.open {
                            self.pickup_request = true;
                        }
                        if combat_item {
                            self.store.begin_global_cooldown(self.now_ms());
                        }
                        if stone.is_some() {
                            // M18/F54: геометрия карточки; гейт рендера —
                            // sharpen_stone, которое придёт в ack.
                            self.open_popup(PopupKind::Upgrade);
                        }
                        self.store.entity_detail = None;
                        self.store.clear_loot_confirm();
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::PowerStrike => {
                let cmd = ld2_protocol::cmd::SkillUse {
                    skill_id: "power_strike".into(),
                    target_id: self.store.target.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Cast);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::ConfirmDeath => self.store.confirm_death(),
            Click::LootTakeMarker(uid, name) => self.store.set_loot_confirm(uid, name),
            // M23: «Взять всё» — партия в очередь; труп игрока —
            // одно подтверждение статуса на всю партию (ФР-11).
            Click::LootTakeAll => {
                let Some(detail) = self.store.pickup.clone() else {
                    return;
                };
                let uids = pickup::pickup_uids(&detail);
                if uids.is_empty() {
                    return;
                }
                if detail["was_player"] == true {
                    self.pickup_confirm_all = true;
                    let name = detail["entity"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    self.store.set_loot_confirm(String::new(), name);
                } else {
                    self.pickup_queue = uids;
                    self.pump_take_queue(cx);
                }
            }
            Click::LootTake => {
                if self.pickup_confirm_all {
                    // Подтверждение партии на трупе игрока (ФР-11).
                    self.pickup_confirm_all = false;
                    self.store.clear_loot_confirm();
                    self.pickup_queue = self
                        .store
                        .pickup
                        .as_ref()
                        .map(pickup::pickup_uids)
                        .unwrap_or_default();
                    self.pump_take_queue(cx);
                } else if let Some((uid, _)) = self.store.loot_confirm.clone() {
                    if !uid.is_empty() {
                        self.on_click(Click::UseItem(uid), cx);
                    }
                }
            }
            Click::LootCancel => {
                self.pickup_confirm_all = false;
                self.store.clear_loot_confirm();
            }
            Click::Gather(id) => {
                let cmd = ld2_protocol::cmd::ResourceGather { entity_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Gather),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::Harvest(id, action) => {
                // Имя животного — из трупа снапшота (без «Труп:»).
                let name = self
                    .store
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.location.entities.iter().find(|e| e.id == id))
                    .map(|e| e.name.strip_prefix("Труп: ").unwrap_or(&e.name).to_string())
                    .unwrap_or_default();
                let cmd = ld2_protocol::cmd::CorpseHarvest {
                    entity_id: id.clone(),
                    action: action.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Harvest);
                        // Прогресс-бар — с момента отправки (5 с);
                        // отказ сервера чистит Act::Harvest (ошибка).
                        // entity_id (M24): итог другого трупа бар не гасит.
                        let label = format!(
                            "{} · {}",
                            if action == "skin" {
                                t(self.lang, Key::SkinBtn)
                            } else {
                                t(self.lang, Key::GutBtn)
                            },
                            name
                        );
                        self.store.harvest = Some(ld2_core::store::HarvestProgress {
                            started_ms: self.now_ms(),
                            action,
                            name,
                            entity_id: id,
                        });
                        // M24: единый прогресс-бар (сервер-джоб5 с).
                        self.start_progress(ProgressSource::Harvest, label, 5_000);
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            // M24: админ-панель — роль гейтит сервер; кнопка в шапке
            // рисуется только для admin.
            Click::AdminOpen => {
                self.open_popup(PopupKind::AdminPanel);
                self.admin_catalog_request = true;
                self.pump_admin_catalog(cx);
            }
            Click::AdminGive(def_id) => {
                let cmd = ld2_protocol::cmd::AdminGiveAsk {
                    target: self.admin_target.clone(),
                    def_id,
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::AdminAsk),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::AdminRemove(item_ref) => {
                let cmd = ld2_protocol::cmd::AdminRemoveAsk {
                    target: self.admin_target.clone(),
                    item_ref,
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::AdminAsk),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::AdminTarget(target) => {
                self.admin_target = target;
                self.store.admin_target_items = None;
                self.admin_target_request = true;
                self.pump_admin_target(cx);
            }
            // M24: очистка цели — свои предметы уходят событиями
            // снапшота, чужие (не-self) перечитываем после ack.
            Click::AdminClear(target) => {
                let cmd = ld2_protocol::cmd::AdminClear { target };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::AdminClear);
                        if self.admin_target != "self" {
                            self.admin_target_request = true;
                        }
                    }
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::AdminGod(enabled) => {
                let cmd = ld2_protocol::cmd::AdminGodMode { enabled };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::AdminGod),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::CloseDetail => self.store.entity_detail = None,
            Click::SwitchChar => self.open_exit(ExitKind::SwitchChar, cx),
        }
    }

    // ── элементы ───────────────────────────────────────────────────────────

    /// M14/F15: **TitleBar** — своя верхняя строка окна (системный
    /// скрыт): слева название приложения внутри drag-региона (тянем
    /// окно через `Window::start_window_move`), справа — настройки и
    /// WindowControl ✕. На macOS слева резерв под «светофор».
    fn titlebar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let settings_open = self.settings_open;
        let in_world = self.store.screen == Screen::Game;
        let before_login = self.store.screen != Screen::Login;
        let left_pad = if matches!(host_os(), gpui::OperatingSystem::Mac) {
            76.0
        } else {
            12.0
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_1()
            .pl(px(left_pad))
            .bg(rgb(pal.panel))
            .border_b_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .id("ld2-titlebar-drag")
                    .flex()
                    .flex_grow_1()
                    .items_center()
                    .cursor_pointer()
                    // M14/F23: нативный hit-test caption — на Windows
                    // окно тянется через HTCAPTION (start_window_move
                    // там — no-op трейта); на macOS этот метод —
                    // заглушка, работает on_drag ниже.
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(t(lang, Key::AppTitle)),
                    )
                    // Drag-регион: кнопки лежат вне его и не мешают.
                    .on_drag(TitlebarDrag, |_value, _offset, window, acx| {
                        window.start_window_move();
                        acx.new(|_| EmptyView)
                    }),
            )
            // M24: админ-панель — только role=admin и в мире
            // (сервер гейтит повторно).
            .when(
                in_world
                    && self
                        .store
                        .session
                        .as_ref()
                        .is_some_and(|s| s.role == "admin"),
                |d| {
                    d.child(
                        IconButton::new("ld2-titlebar-admin", IconName::Shield)
                            .tooltip(t(lang, Key::AdminPanelTitle))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.on_click(Click::AdminOpen, cx);
                                cx.notify();
                            })),
                    )
                },
            )
            // Настройки (перенесены из шапки).
            .child(
                div()
                    .when(settings_open, |d| d.rounded_md().bg(rgb(pal.accent)))
                    .child(
                        IconButton::new("ld2-titlebar-settings", IconName::Settings)
                            .tooltip(t(lang, Key::SettingsBtn))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.on_click(Click::ToggleSettings, cx);
                                cx.notify();
                            })),
                    ),
            )
            // M14/F16: кнопки выхода вместо меню шапки — смена
            // персонажа (в мире) и дисконнект (везде, кроме входа).
            .when(in_world, |d| {
                d.child(
                    IconButton::new("ld2-titlebar-switch", IconName::Users)
                        .tooltip(t(lang, Key::BtnSwitchChar))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.on_click(Click::SwitchChar, cx);
                            cx.notify();
                        })),
                )
            })
            .when(!before_login, |d| {
                d.child(
                    IconButton::new("ld2-titlebar-logout", IconName::LogOut)
                        .tooltip(t(lang, Key::BtnLogout))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.on_click(Click::Logout, cx);
                            cx.notify();
                        })),
                )
            })
            // M14/F23 (Windows): системные кнопки окна _ □ ✕ — ely
            // рисует их в стиле платформы (caption-кнопки, hover даёт
            // snap-макеты Win11). На macOS свои не нужны: системные
            // «светофоры» видны сквозь прозрачный тайтлбар.
            .when(!matches!(host_os(), gpui::OperatingSystem::Mac), |d| {
                d.child(WindowControls::new("ld2-titlebar-controls"))
            })
    }

    fn header(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let update = self.update_available.clone();
        let lang = self.lang;
        // M14/F1: статус сервера и текущая локация — прямо в шапке.
        let location = self
            .store
            .snapshot
            .as_ref()
            .map(|s| (s.location.name.clone(), s.location.flags.clone()));

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .bg(rgb(pal.panel))
            .border_b_1()
            .border_color(rgb(pal.border))
            .child(
                // Левая группа: (в мире) локация и бейджи — название
                // приложения уехало в TitleBar (M14/F15).
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(location.map(|(name, flags)| {
                        // M14/F15: название локации — зелёное, если
                        // безопасная, красное, если небезопасная (pvp).
                        let pvp = flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
                        let name_color = if pvp { pal.danger } else { pal.success };
                        // M15/F25: локация в 2 строки — 1) название,
                        // 2) тип локации (бейджи Safe/PvP…).
                        let name_row = div().flex().flex_row().items_center().gap_1().child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(name_color))
                                .child(format!("· {name}")),
                        );
                        let mut badges_row = div().flex().flex_row().items_center().gap_1();
                        for (tone, token) in location_badges(&flags) {
                            let color = match tone {
                                BadgeTone::Warn => pal.warn,
                                BadgeTone::Success => pal.success,
                                BadgeTone::Muted => pal.muted,
                            };
                            badges_row =
                                badges_row.child(badge(badge_label(lang, &token), color, pal));
                        }
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(name_row)
                            .child(badges_row)
                    }))
                    // M15/F25: разделитель и кнопка окна персонажа рядом
                    // с локацией (только где есть снапшот — в мире).
                    .children(
                        self.store
                            .snapshot
                            .as_ref()
                            .map(|_| div().w(px(1.0)).h(px(20.0)).bg(rgb(pal.border))),
                    )
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::BtnCharacter),
                            pal.panel,
                            pal.text,
                            Click::ToggleCharacter,
                        )
                    }))
                    // M16/F29b: кнопка рюкзака рядом с «Персонаж».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::SheetInventory),
                            pal.panel,
                            pal.text,
                            Click::ToggleInventory,
                        )
                    }))
                    // M18/F36: кнопка квестов рядом с «Рюкзак».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::QuestsTab),
                            pal.panel,
                            pal.text,
                            Click::ToggleQuests,
                        )
                    }))
                    // M21: кнопка профессий рядом с «Квесты».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::ProfessionsTab),
                            pal.panel,
                            pal.text,
                            Click::ToggleProfessions,
                        )
                    })),
            )
            .child(
                // Правая группа: обновление, язык, настройки и меню
                // шапки (M14/F1). Статус сервера — в статус-баре игры
                // (заказчик 2026-11-20); отметка роли убрана совсем.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(update.map(|v| {
                        btn(
                            cx,
                            tf(lang, Key::UpdateBadge, &[("{v}", &format!("v{v}"))]),
                            pal.success,
                            pal.accent_text,
                            Click::OpenReleases,
                        )
                    })),
            )
    }

    fn login_screen(&mut self, window: &Window, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let error = self.store.login_error.clone();
        // M14/F21: живое чтение состояния на каждый рендер — Caps из
        // поля окна (обновляется каждым ModifiersChanged), раскладка —
        // из TIS-источника ввода.
        let caps_on = window.capslock().on;
        let (layout_id, layout_name) = {
            let layout = cx.keyboard_layout();
            (layout.id().to_string(), layout.name().to_string())
        };
        let ru_layout = is_cyrillic_layout(&layout_id, &layout_name);
        div()
            // M14/F21: смена модификаторов (Caps/Shift/Ctrl) → перерисовка:
            // предупреждения читаются в рендере из window/keyboard_layout.
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .size_full()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(22.0))
                    .child(t(lang, Key::LoginTitle)),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(Input::new(self.ti(&self.login_input))),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(PasswordInput::new(self.ti(&self.password_input))),
            )
            // M16/F29j: «запомнить пароль» — пароль живёт только в
            // keyring; флаг — в конфиге (сохраняется сразу).
            .child({
                let me = cx.entity();
                let on = self.config.remember_password;
                div().w(px(320.0)).child(
                    Checkbox::new("ld2-login-remember", on)
                        .label(t(lang, Key::RememberPassword))
                        .on_change(move |on, _, acx| {
                            me.update(acx, |root, cx| {
                                root.config.remember_password = on;
                                if let Err(e) = root.config_store.save(&root.config) {
                                    tracing::warn!("не удалось сохранить конфиг: {e:#}");
                                }
                                cx.notify();
                            })
                        }),
                )
            })
            // M14/F21: предупреждения Caps/раскладка — под полями ввода.
            .children(caps_on.then(|| {
                div()
                    .w(px(320.0))
                    .text_size(px(12.0))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::WarnCaps))
            }))
            .children(ru_layout.then(|| {
                div()
                    .w(px(320.0))
                    .text_size(px(12.0))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::WarnRuLayout))
            }))
            .child(btn(
                cx,
                t(lang, Key::BtnLogin).to_string(),
                pal.accent,
                pal.accent_text,
                Click::Login,
            ))
            .child(btn(
                cx,
                t(lang, Key::BtnRegister).to_string(),
                pal.panel,
                pal.text,
                Click::OpenRegister,
            ))
            .children(error.map(|e| div().text_color(rgb(pal.danger)).child(e)))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child("demo/demo123 · demo2/demo123"),
            )
    }

    /// Окно настроек (ФР-19) — как отдельный экран с кнопкой закрытия.
    /// M14/F14: тело настроек для диалога `settings_dialog` — без
    /// заголовка и кнопки «Закрыть» (их даёт Dialog: крестик/Esc/скрим).
    /// Фиксированный размер даёт определённую высоту для скролла тела
    /// и влезает в минимум окна 960×640.
    fn settings_content(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let theme = self.config.theme.clone();
        let limit = self.config.chat_limit;
        let notify = self.config.notify;
        let host_ok = host_allowed(&self.config.host, RELEASE);
        // Размер задаёт карточка settings_layer (760×560); здесь —
        // растяжение на остаток, чтобы тело скроллилось внутри.
        let mut root = div().flex().flex_col().gap_3().flex_grow_1().min_h_0();

        // M14/F9: вкладки настроек — «Основные» и «Горячие клавиши»
        // (11 строк ремаппинга растут на своей вкладке и скроллятся).
        let active = match self.settings_tab {
            SettingsTab::General => "general",
            SettingsTab::Fonts => "fonts",
            SettingsTab::Connection => "connection",
            SettingsTab::Hotkeys => "hotkeys",
        };
        // M14/F12: иконки вкладок (метки без изменений).
        let choices = vec![
            Choice::new("general", t(lang, Key::SettingsGeneral)).icon(IconName::SlidersHorizontal),
            Choice::new("fonts", t(lang, Key::SettingsFonts)).icon(IconName::Type),
            Choice::new("connection", t(lang, Key::SettingsConnection)).icon(IconName::Server),
            Choice::new("hotkeys", t(lang, Key::SettingsHotkeys)).icon(IconName::Keyboard),
        ];
        let settings_this = cx.entity();
        root = root.child(Tabs::new("ld2-settings-tabs", choices, active).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                settings_this.update(acx, |root, cx| {
                    root.settings_tab = match value.as_str() {
                        "fonts" => SettingsTab::Fonts,
                        "connection" => SettingsTab::Connection,
                        "hotkeys" => SettingsTab::Hotkeys,
                        _ => SettingsTab::General,
                    };
                    // Прокрутка новой вкладки — с начала.
                    root.settings_scroll.scroll_to_item(0);
                    cx.notify();
                });
            },
        ));
        // M14/F9: тело — только содержимое активной вкладки.
        let body =
            match self.settings_tab {
                SettingsTab::General => {
                    let mut body = div().flex().flex_col().gap_3();

                    // Тема.
                    body = body.child(row(
                        pal,
                        t(lang, Key::SettingsTheme).to_string(),
                        vec![
                            btn(
                                cx,
                                t(lang, Key::ThemeDark).to_string(),
                                if theme == "dark" {
                                    pal.accent
                                } else {
                                    pal.panel
                                },
                                pal.text,
                                Click::Theme("dark"),
                            ),
                            btn(
                                cx,
                                t(lang, Key::ThemeLight).to_string(),
                                if theme == "light" {
                                    pal.accent
                                } else {
                                    pal.panel
                                },
                                pal.text,
                                Click::Theme("light"),
                            ),
                        ],
                    ));

                    // M14/F8: язык (перенесён из шапки) — зеркало строки «Тема».
                    body = body.child(row(
                        pal,
                        t(lang, Key::Language).to_string(),
                        vec![
                            btn(
                                cx,
                                "RU",
                                if lang.code() == "ru" {
                                    pal.accent
                                } else {
                                    pal.panel
                                },
                                pal.text,
                                Click::Lang("ru"),
                            ),
                            btn(
                                cx,
                                "EN",
                                if lang.code() == "en" {
                                    pal.accent
                                } else {
                                    pal.panel
                                },
                                pal.text,
                                Click::Lang("en"),
                            ),
                        ],
                    ));

                    // M14/F13: стиль строк боя в журнале: цвета — ColorPicker,
                    // жирность урона — Вкл/Выкл (зеркало «Уведомлений»).
                    body = body.child(color_picker_row(
                        cx,
                        pal,
                        lang,
                        ColorRow {
                            label: Key::SettingsLogDamageColor,
                            id: "ld2-log-damage-color",
                            value: &self.config.log_damage_color,
                            open: self.color_picker_open == Some("ld2-log-damage-color"),
                            set: |root, v| root.config.log_damage_color = v,
                        },
                    ));
                    body = body.child(color_picker_row(
                        cx,
                        pal,
                        lang,
                        ColorRow {
                            label: Key::SettingsLogAttackColor,
                            id: "ld2-log-attack-color",
                            value: &self.config.log_attack_color,
                            open: self.color_picker_open == Some("ld2-log-attack-color"),
                            set: |root, v| root.config.log_attack_color = v,
                        },
                    ));
                    // M14/F14: флаг — слайдер-переключатель (Ely Switch).
                    body = body.child(row(
                        pal,
                        t(lang, Key::SettingsDamageBold).to_string(),
                        vec![div().child({
                            let this = cx.entity();
                            Switch::new("ld2-log-damage-bold", self.config.log_damage_bold)
                                .on_change(move |on, _window, acx| {
                                    this.update(acx, |root, cx| {
                                        root.on_click(Click::LogDamageBold(on), cx);
                                        cx.notify();
                                    });
                                })
                        })],
                    ));

                    // Лимит истории чата.
                    body = body.child(row(
                        pal,
                        t(lang, Key::SettingsChatLimit).to_string(),
                        vec![
                            btn(cx, "−500", pal.panel, pal.text, Click::ChatLimit(-500)),
                            div().px_2().child(format!("{limit}")),
                            btn(cx, "+500", pal.panel, pal.text, Click::ChatLimit(500)),
                        ],
                    ));

                    // Уведомления (D7).
                    // M14/F14: флаг — слайдер-переключатель (Ely Switch).
                    body = body.child(row(
                        pal,
                        t(lang, Key::SettingsNotify).to_string(),
                        vec![div().child({
                            let this = cx.entity();
                            Switch::new("ld2-notify", notify).on_change(move |on, _window, acx| {
                                this.update(acx, |root, cx| {
                                    root.on_click(Click::SetNotify(on), cx);
                                    cx.notify();
                                });
                            })
                        })],
                    ));

                    // Мат-фильтр исходящих (заказчик 2026-11-20).
                    body = body.child(row(
                        pal,
                        t(lang, Key::MuteFilterLabel).to_string(),
                        vec![div().child({
                            let this = cx.entity();
                            Switch::new("ld2-mute-filter", self.config.chat_mute_filter).on_change(
                                move |on, _window, acx| {
                                    this.update(acx, |root, cx| {
                                        root.on_click(Click::SetMuteFilter(on), cx);
                                        cx.notify();
                                    });
                                },
                            )
                        })],
                    ));

                    // Путь конфига (ФР-19: импорт/экспорт — правка файла вручную).
                    if let Ok(dirs) = ld2_core::config::project_dirs() {
                        body = body.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(pal.muted))
                                .child(dirs.config_dir().display().to_string()),
                        );
                    }

                    body
                }
                // M14/F14: шрифты — отдельная вкладка (глобальный масштаб,
                // шрифт чата и шрифта лога).
                SettingsTab::Fonts => {
                    let mut body = div().flex().flex_col().gap_3();
                    // M11.5: размер шрифта (НФР: 1.0…1.4, min 12px) — Slider Ely
                    // пишет Theme.font_scale (отклонение M10 «font_scale не
                    // применяется» снято).
                    body = body.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .w(px(130.0))
                                    .text_color(rgb(pal.muted))
                                    .child(t(lang, Key::SettingsFont)),
                            )
                            .child(slider_cell({
                                let this = cx.entity();
                                let value = self.config.font_scale as f64;
                                Slider::new("ld2-font-scale", value)
                                    .range(1.0, 1.4)
                                    .step(0.05)
                                    .on_change(move |v, _window, acx| {
                                        this.update(acx, |root, cx| {
                                            root.config.font_scale = v as f32;
                                            cx.global_mut::<Theme>().font_scale = v as f32;
                                            cx.refresh_windows();
                                            cx.notify();
                                            root.save_config();
                                        });
                                    })
                            }))
                            .child(value_readout(pal, format!("{:.2}", self.config.font_scale))),
                    );

                    // M14/F8: шрифт сообщений чата и строк лога — отдельные px.
                    body = body.child(font_px_row(
                        cx,
                        pal,
                        lang,
                        Key::SettingsChatFont,
                        "ld2-chat-font-px",
                        self.config.chat_font_px,
                        |root, v| root.config.chat_font_px = v,
                    ));
                    body = body.child(font_px_row(
                        cx,
                        pal,
                        lang,
                        Key::SettingsLogFont,
                        "ld2-log-font-px",
                        self.config.log_font_px,
                        |root, v| root.config.log_font_px = v,
                    ));
                    body
                }
                // M14/F11: адрес сервера — read-only; поле ввода
                // раскрывается действием «Подключиться к другому».
                SettingsTab::Connection => {
                    let mut body = div().flex().flex_col().gap_3();
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(rgb(pal.muted))
                                    .child(t(lang, Key::SettingsHost)),
                            )
                            .child(div().text_size(px(14.0)).child(self.config.host.clone()))
                            .children((!host_ok).then(|| {
                                div()
                                    .text_color(rgb(pal.danger))
                                    .child(t(lang, Key::SettingsWsDenied))
                            })),
                    );
                    // M19 (§15): режим discovery — домен (пусто = прямой URL).
                    body =
                        body.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_color(rgb(pal.muted))
                                        .child(t(lang, Key::SettingsDomain)),
                                )
                                .child(div().text_size(px(14.0)).child(
                                    self.config.domain.clone().unwrap_or_else(|| "—".into()),
                                )),
                        );
                    if self.host_editing {
                        body = body.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgb(pal.muted))
                                        .child(t(lang, Key::SettingsHost)),
                                )
                                .child(
                                    div()
                                        .w(px(320.0))
                                        .child(Input::new(self.ti(&self.host_input))),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgb(pal.muted))
                                        .child(t(lang, Key::SettingsDomain)),
                                )
                                .child(
                                    div()
                                        .w(px(320.0))
                                        .child(Input::new(self.ti(&self.domain_input))),
                                )
                                .child(btn(
                                    cx,
                                    t(lang, Key::SettingsApply).to_string(),
                                    pal.accent,
                                    pal.accent_text,
                                    Click::ApplyHost,
                                )),
                        );
                    } else {
                        body = body.child(btn(
                            cx,
                            t(lang, Key::SettingsChangeHost).to_string(),
                            pal.panel,
                            pal.text,
                            Click::EditHost,
                        ));
                    }
                    body
                }
                SettingsTab::Hotkeys => {
                    let mut body = div().flex().flex_col().gap_3();
                    // Горячие клавиши: только отображение (редактирование/применение
                    // навигации — M10.4 вместе с game-навигацией).
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(t(lang, Key::SettingsHotkeys)),
                            )
                            // M14/F4: все ремаппингуемые действия — из одного
                            // вектора (навигация/цикл + боевые 1–6), без match
                            // по индексу.
                            .children(HOTKEY_ROWS.iter().map(|row| {
                                let value = (row.get)(&self.config.hotkeys).to_string();
                                hotkey_row(cx, lang, row, &value)
                            })),
                    );

                    body
                }
            };

        // M14/F9: тело активной вкладки — в скролле; заголовок,
        // вкладки и кнопка «Закрыть» остаются вне прокрутки.
        root = root.child(scrollable_tab(
            "ld2-settings-scroll",
            &self.settings_scroll,
            body,
        ));

        root
    }
}

/// Текущая ОС — для разрешения `secondary-` в Keystroke::parse_with_os.
/// M14/F21: русская/кириллическая раскладка по TIS-id и локализованному
/// имени (macOS: `com.apple.keylayout.Russian` + «Русская»/«Russian»;
/// Linux: id `ru`). Чистая функция — юнит-тест.
fn is_cyrillic_layout(id: &str, name: &str) -> bool {
    let id = id.to_lowercase();
    let name = name.to_lowercase();
    id == "ru" || id.contains("russ") || name.contains("russ") || name.contains("русск")
}

fn host_os() -> gpui::OperatingSystem {
    if cfg!(target_os = "macos") {
        gpui::OperatingSystem::Mac
    } else if cfg!(windows) {
        gpui::OperatingSystem::Windows
    } else {
        gpui::OperatingSystem::Linux
    }
}

/// M15/F25e: закрыть окно по mouse-up на scrim можно, только если
/// жест **начался** на scrim (настоящий «клик мимо») и это не
/// завершение перетаскивания/resize (gpui ещё держит active_drag).
pub(crate) fn scrim_closes_on_up(scrim_down: bool, has_active_drag: bool) -> bool {
    scrim_down && !has_active_drag
}

/// M15/F25d: верхний левый угол карточки окна персонажа — центр
/// вьюпорта + смещение drag (absolute-позиционирование как в F14:
/// без марджинов, «половинных» сдвигов центрирования нет).
pub(crate) fn char_card_origin(vw: f32, vh: f32, w: f32, h: f32, delta: (f32, f32)) -> (f32, f32) {
    (
        ((vw - w) / 2.0).max(0.0) + delta.0,
        ((vh - h) / 2.0).max(0.0) + delta.1,
    )
}

/// M15/F25c: кламп размера карточки окна персонажа — min 560×400,
/// max = вьюпорт − 40 px (но не уже min).
pub(crate) fn clamp_char_size(w: f32, h: f32, vw: f32, vh: f32) -> (f32, f32) {
    (
        w.clamp(560.0, (vw - 40.0).max(560.0)),
        h.clamp(400.0, (vh - 40.0).max(400.0)),
    )
}

/// Нажатие → спецификация (`alt+up`) для записи в config (ФР-19).
fn spec_from_keystroke(ks: &gpui::Keystroke) -> Option<String> {
    let name = ks.key.to_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut spec = String::new();
    if ks.modifiers.alt {
        spec.push_str("alt+");
    }
    if ks.modifiers.control {
        spec.push_str("ctrl+");
    }
    if ks.modifiers.shift {
        spec.push_str("shift+");
    }
    if ks.modifiers.platform {
        spec.push_str("cmd+");
    }
    spec.push_str(&name);
    Some(spec)
}

/// Совпадает ли нажатие со спецификацией хоткея (`alt+up`, `tab`, …).
/// Спецификации из config.toml парсятся gpui-синтаксисом (`alt-up`).
fn keystroke_matches(spec: &str, ks: &gpui::Keystroke) -> bool {
    let normalized = spec.replace('+', "-");
    // gpui-pre 0.3.10: parse требует &'static str — динамические строки
    // из config.toml парсятся через parse_with_os (документированный путь).
    match gpui::Keystroke::parse_with_os(&normalized, host_os()) {
        Ok(want) => {
            want.modifiers.alt == ks.modifiers.alt
                && want.modifiers.control == ks.modifiers.control
                && want.modifiers.shift == ks.modifiers.shift
                && want.modifiers.platform == ks.modifiers.platform
                && want.modifiers.function == ks.modifiers.function
                // M15: одна и та же физическая клавиша на латинице и
                // ЙЦУКЕН («c» на русской раскладке приходит как «с» —
                // charactersIgnoringModifiers отдаёт символ раскладки).
                && (want.key.eq_ignore_ascii_case(&ks.key)
                    || keys_physically_equal(&want.key, &ks.key))
        }
        Err(_) => false,
    }
}

/// M15: раскладки QWERTY ↔ ЙЦУКЕН (26 букв; одиночные символы хоткеев).
const QWERTY_CYRILLIC: [(char, char); 26] = [
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
];

/// M15: приводит одиночный символ к «физическому» виду — кириллица
/// ЙЦУКЕН → латиница QWERTY, регистр не важен; прочие символы — как есть.
fn physical_key_char(ch: char) -> char {
    let lower = ch.to_ascii_lowercase();
    if lower.is_ascii_alphabetic() {
        return lower;
    }
    QWERTY_CYRILLIC
        .iter()
        .find(|(_, cyr)| *cyr == lower)
        .map(|(lat, _)| *lat)
        .unwrap_or(lower)
}

/// M15: совпадение одиночных клавиш с учётом раскладки: «c» ↔ «с».
fn keys_physically_equal(want: &str, got: &str) -> bool {
    let (mut w, mut g) = (want.chars(), got.chars());
    let (Some(w1), Some(g1), None) = (w.next(), g.next(), w.next()) else {
        return false;
    };
    if g.next().is_some() {
        return false;
    }
    physical_key_char(w1) == physical_key_char(g1)
}

/// Строка настройки: подпись слева, контролы справа.
fn row(pal: &Palette, label: String, controls: Vec<Div>) -> Div {
    let mut r = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .child(div().child(label));
    let mut controls = controls.into_iter();
    if let Some(first) = controls.next() {
        r = r.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(first)
                .children(controls),
        );
    }
    r
}

/// Строка хоткея: `HotkeyInput` Ely — запись нажатия и очистка Backspace
/// (ФР-19). Приоритет над навигацией даёт сам виджет (фокус-режим),
/// ручная capture-машинка M10 упразднена; значение хранится в config.toml
/// форматом `alt+up` (round-trip через Keystroke::parse_with_os).
/// M14/F4: строка настройки хоткея — id виджета, ключ l10n и доступ к
/// спецификации в конфиге (геттер/сеттер вместо match по индексу).
struct HotkeyRowSpec {
    id: &'static str,
    label: Key,
    get: fn(&Hotkeys) -> &str,
    set: fn(&mut Hotkeys, String),
}

/// Все ремаппингуемые действия: навигация/цикл (ФР-19) + боевые
/// панели (M14/F4, цифры 1–6 — Esc занят диалогами).
const HOTKEY_ROWS: [HotkeyRowSpec; 15] = [
    HotkeyRowSpec {
        id: "ld2-hk-nav-up",
        label: Key::HkNavUp,
        get: |hk| hk.nav_up.as_str(),
        set: |hk, v| hk.nav_up = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-down",
        label: Key::HkNavDown,
        get: |hk| hk.nav_down.as_str(),
        set: |hk, v| hk.nav_down = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-left",
        label: Key::HkNavLeft,
        get: |hk| hk.nav_left.as_str(),
        set: |hk, v| hk.nav_left = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-right",
        label: Key::HkNavRight,
        get: |hk| hk.nav_right.as_str(),
        set: |hk, v| hk.nav_right = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-cycle",
        label: Key::HkCycleTarget,
        get: |hk| hk.cycle_target.as_str(),
        set: |hk, v| hk.cycle_target = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-attack",
        label: Key::Attack,
        get: |hk| hk.action_attack.as_str(),
        set: |hk, v| hk.action_attack = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-power-strike",
        label: Key::PowerStrike,
        get: |hk| hk.action_power_strike.as_str(),
        set: |hk, v| hk.action_power_strike = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-cleave",
        label: Key::HkCleave,
        get: |hk| hk.action_cleave.as_str(),
        set: |hk, v| hk.action_cleave = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-flee",
        label: Key::BtnFlee,
        get: |hk| hk.action_flee.as_str(),
        set: |hk, v| hk.action_flee = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-rune",
        label: Key::HkRune,
        get: |hk| hk.action_rune.as_str(),
        set: |hk, v| hk.action_rune = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-consumable",
        label: Key::HkConsumable,
        get: |hk| hk.action_consumable.as_str(),
        set: |hk, v| hk.action_consumable = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-character",
        label: Key::HkOpenCharacter,
        get: |hk| hk.open_character.as_str(),
        set: |hk, v| hk.open_character = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-backpack",
        label: Key::HkOpenBackpack,
        get: |hk| hk.open_backpack.as_str(),
        set: |hk, v| hk.open_backpack = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-quests",
        label: Key::HkOpenQuests,
        get: |hk| hk.open_quests.as_str(),
        set: |hk, v| hk.open_quests = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-professions",
        label: Key::HkOpenProfessions,
        get: |hk| hk.open_professions.as_str(),
        set: |hk, v| hk.open_professions = v,
    },
];

fn hotkey_row(cx: &mut Context<'_, Root>, lang: Lang, row: &HotkeyRowSpec, spec: &str) -> Div {
    let want = gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).ok();
    let this = cx.entity();
    let set = row.set;
    div()
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .text_size(px(13.0))
        .child(
            div()
                .flex_grow_1()
                .text_color(rgb(0x7f848e))
                .child(t(lang, row.label)),
        )
        .child(
            HotkeyInput::new(row.id, want).on_change(move |ks, _window, acx| {
                let spec = ks
                    .as_ref()
                    .and_then(spec_from_keystroke)
                    .unwrap_or_default();
                this.update(acx, |root, cx| {
                    set(&mut root.config.hotkeys, spec);
                    root.save_config();
                    cx.notify();
                });
            }),
        )
}

/// M14/F3/F9: содержимое с обычной прокруткой — паттерн `log_block`
/// (ScrollHandle в полях Root, без stick-to-bottom): контейнер занимает
/// остаток высоты, список прокручивается внутри. Используется вкладками
/// листа («Навыки»/«Профессии») и телом настроек.
fn scrollable_tab(id: &'static str, handle: &ScrollHandle, body: Div) -> Div {
    div().flex().flex_col().flex_grow_1().min_h_0().child(
        ScrollArea::new(id)
            .vertical()
            .track_scroll(handle)
            .flex_grow_1()
            .min_h_0()
            .child(body),
    )
}

/// M14/F8: строка настройки «размер шрифта, px» — Slider 10…20 с шагом 1;
/// пишет поле конфига и перерисовывает окно (значение — абсолютные px).
fn font_px_row(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    label: Key,
    id: &'static str,
    value: f32,
    set: fn(&mut Root, f32),
) -> Div {
    let this = cx.entity();
    div()
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .child(
            div()
                .w(px(130.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, label)),
        )
        .child(slider_cell(
            Slider::new(id, f64::from(value))
                .range(10.0, 20.0)
                .step(1.0)
                .on_change(move |v, _window, acx| {
                    this.update(acx, |root, cx| {
                        set(root, v as f32);
                        cx.refresh_windows();
                        cx.notify();
                        root.save_config();
                    });
                }),
        ))
        .child(value_readout(pal, format!("{} px", value.round() as i32)))
}

/// M14/F13: hex из настроек → RGB (парсер Ely — те же формы, что
/// принимает `is_hex_color` в sanitize конфига); битый → `None`
/// (вызывающий берёт дефолт). В F14 перенесено сюда из `game.rs` —
/// нужно и для свотча в настройках.
fn hex_to_u32(text: &str) -> Option<u32> {
    let rgba = parse_hex(text).ok()?;
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Some((byte(rgba.r) << 16) | (byte(rgba.g) << 8) | byte(rgba.b))
}

/// M14/F14: строка настройки цвета — подпись, **свотч-кнопка** (по
/// нажатию открывается/закрывается ColorPicker под строкой) и hex.
/// во всю ширину (он инлайн-блок: превью + плоскость + рейки + hex-поля;
/// тело настроек скроллится — F9). Значение в конфиге — hex; запись —
/// `hex(Hsla::to_rgb)`, чтение — `parse_hex` (те же формы, что
/// принимает `is_hex_color` в sanitize).
/// M14/F14: параметры строки цвета (пакет — иначе clippy
/// `too_many_arguments` при 8 аргументах).
struct ColorRow<'a> {
    label: Key,
    id: &'static str,
    value: &'a str,
    open: bool,
    set: fn(&mut Root, String),
}

fn color_picker_row(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    spec: ColorRow<'_>,
) -> Div {
    let ColorRow {
        label,
        id,
        value,
        open,
        set,
    } = spec;
    let this = cx.entity();
    // sanitize гарантирует валидный hex; запасной — серый, без паники.
    let rgba = parse_hex(value).unwrap_or_else(|_| parse_hex("#808080").expect("запасной цвет"));
    let hsla = Hsla::from(rgba);
    let swatch = hex_to_u32(value).unwrap_or(0x80_80_80);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, label)),
        )
        .child(
            // M14/F14: свотч-кнопка — пикер открывается по нажатию.
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .id(format!("ld2-swatch-{id}"))
                        .cursor_pointer()
                        .w(px(44.0))
                        .h(px(24.0))
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(pal.border))
                        .bg(rgb(swatch))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener({
                                let key = id;
                                move |root, _, _, cx| {
                                    root.toggle_color_picker(key);
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .child(
                    div()
                        .flex_grow_1()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.muted))
                        .child(value.to_string()),
                )
                .when(open, |row| {
                    row.child(btn(
                        cx,
                        t(lang, Key::BtnClose).to_string(),
                        pal.panel,
                        pal.text,
                        Click::TogglePicker(id),
                    ))
                }),
        )
        .when(open, |row| {
            row.child(
                ColorPicker::new(id, hsla)
                    .opaque()
                    .on_change(move |c, _window, acx| {
                        let hex = hex(c.to_rgb());
                        this.update(acx, |root, cx| {
                            set(root, hex);
                            root.save_config();
                            cx.refresh_windows();
                            cx.notify();
                        });
                    }),
            )
        })
}

/// M14/F8 (фикс смоука): слайдер Ely сам ширины не имеет — `Track`
/// рендерится `flex_none` с одним padding, а все его дети абсолютно
/// позиционированы (внешняя ширина ≈ thumb, ≈0 полезной площади: не
/// виден и не кликается). Обёртка — flex-**колонка**: поперечная ось
/// растягивает слайдер на всю ширину ячейки, а `size_full()` у Ely
/// получает реально определённую область.
fn slider_cell(slider: Slider) -> Div {
    div().flex().flex_col().flex_grow_1().child(slider)
}

/// M14/F8: показ текущего значения настройки справа от ползунка.
fn value_readout(pal: &Palette, text: String) -> Div {
    div()
        .w(px(56.0))
        .text_size(px(13.0))
        .text_color(rgb(pal.text))
        .child(text)
}

/// Кнопка: статичные цвета + hover, действие — [`Click`].
fn btn(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    fg: u32,
    click: Click,
) -> Div {
    btn_hover(cx, label, bg, bg, fg, click)
}

/// Кнопка с произвольным действием (для листа: замыкание вместо [`Click`]).
pub(crate) fn act_btn(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    fg: u32,
    f: impl FnOnce(&mut Root, &mut Context<'_, Root>) + 'static,
) -> Div {
    let cell = RefCell::new(Some(f));
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(bg)))
        .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
        .child(label.into())
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if let Some(f) = cell.borrow_mut().take() {
                    f(this, cx);
                }
                cx.notify();
            }),
        )
}

/// M18/F38: кнопка действия с **реальной** подсветкой наведения
/// (в отличие от `act_btn`, где hover не меняет фон) — реплики
/// персонажа в окне диалога.
pub(crate) fn act_btn_hover(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    hover_bg: u32,
    fg: u32,
    f: impl FnOnce(&mut Root, &mut Context<'_, Root>) + 'static,
) -> Div {
    let cell = RefCell::new(Some(f));
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(hover_bg)))
        .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
        .child(label.into())
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if let Some(f) = cell.borrow_mut().take() {
                    f(this, cx);
                }
                cx.notify();
            }),
        )
}

/// Кнопка с отдельным цветом наведения.
fn btn_hover(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    hover_bg: u32,
    fg: u32,
    click: Click,
) -> Div {
    let label: SharedString = label.into();
    div()
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(hover_bg)))
        .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
        .child(label)
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.on_click(click.clone(), cx);
                cx.notify();
            }),
        )
}

/// Ключ текста статуса соединения (паритет `ui::screens::status_text`).
fn status_key(status: &TransportStatus) -> Key {
    match status {
        TransportStatus::Connecting => Key::StatusConnecting,
        TransportStatus::Online => Key::StatusOnline,
        TransportStatus::Reconnecting { .. } => Key::StatusReconnecting,
        TransportStatus::AuthExpired => Key::StatusAuthExpired,
        TransportStatus::Stopped => Key::StatusStopped,
        // M19 (§15): адрес не разрешился (discovery) — свой текст.
        TransportStatus::ResolveFailed(_) => Key::StatusResolveFailed,
    }
}

/// Цвет и ключ статуса сервера для бейджа в шапке (M14/F1): Online →
/// success, Connecting/Reconnecting → warn, AuthExpired/Stopped → danger.
/// Чистая функция (без состояния) — юнит-тест в конце файла.
fn status_color(pal: &Palette, status: &TransportStatus) -> (u32, Key) {
    let color = match status {
        TransportStatus::Online => pal.success,
        TransportStatus::Connecting | TransportStatus::Reconnecting { .. } => pal.warn,
        TransportStatus::AuthExpired
        | TransportStatus::Stopped
        | TransportStatus::ResolveFailed(_) => pal.danger,
    };
    (color, status_key(status))
}

/// Текст статуса соединения (с деталью переподключения).
fn status_text(lang: Lang, status: &TransportStatus) -> String {
    let base = t(lang, status_key(status));
    match status {
        TransportStatus::Reconnecting { attempt, delay } => {
            format!("{base} (попытка {attempt}, {delay:?})")
        }
        _ => base.to_string(),
    }
}

/// Тон бейджа (M14/F1): предупреждение/успех/нейтральный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadgeTone {
    Warn,
    Success,
    Muted,
}

/// Бейджи у названия локации в шапке: всегда либо `pvp` (warn), либо
/// `safe` (success) — по флагу локации; прочие флаги — muted-бейджи с их
/// сырым текстом. Токены локализует рендер ([`badge_label`]).
/// Чистая функция — юнит-тест в конце файла.
fn location_badges(flags: &[String]) -> Vec<(BadgeTone, String)> {
    let pvp = flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
    let mut out = vec![if pvp {
        (BadgeTone::Warn, "pvp".to_string())
    } else {
        (BadgeTone::Success, "safe".to_string())
    }];
    for flag in flags {
        let known = flag.eq_ignore_ascii_case("pvp") || flag.eq_ignore_ascii_case("safe");
        if !known {
            out.push((BadgeTone::Muted, flag.clone()));
        }
    }
    out
}

/// M14/F16: охраняемая зона — нет PvP-флага (вне мира риска нет → true).
/// Чистая функция — юнит-тест в конце файла.
fn guarded_zone(flags: &[String]) -> bool {
    !flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"))
}

/// Подпись бейджа: известные токены → ключи l10n, прочее — как есть.
fn badge_label(lang: Lang, token: &str) -> String {
    match token {
        "pvp" => t(lang, Key::BadgePvp).to_string(),
        "safe" => t(lang, Key::BadgeSafe).to_string(),
        other => other.to_string(),
    }
}

/// Плашка в шапке: рамка и текст в тоне, фон — цвет фона окна.
fn badge(label: String, color: u32, pal: &Palette) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(color))
        .bg(rgb(pal.bg))
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .child(label)
}

/// Бейдж статуса сервера в шапке (M14/F1): цветная точка + текст + RTT
/// (раньше жил в нижнем status_bar, из render убран).
pub(crate) fn status_badge(
    pal: &Palette,
    lang: Lang,
    status: &TransportStatus,
    rtt: Option<u64>,
) -> Div {
    let (color, _) = status_color(pal, status);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(color)))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.text))
                .child(status_text(lang, status)),
        )
        .children(rtt.map(|ms| {
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!("{ms} мс"))
        }))
}

/// Меню шапки (M14/F1, Ely `Menu`): сменить персонажа, выйти из
/// аккаунта, закрыть приложение. Пункты вне контекста отключены:
/// «сменить персонажа» — только в мире, «выйти» — только после входа.
impl Root {
    /// M11.2: on_close диалога — мутация состояния Root и перерисовка
    /// (диалог рендерится из состояния, поэтому «закрыть» = снять флаг).
    pub(crate) fn dismiss(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity();
        move |_window, acx| {
            this.update(acx, |root, rcx| {
                f(root);
                rcx.notify();
            });
        }
    }

    /// M14/F15: закрыть настройки (общий путь для Esc/скрима/✕ Ely).
    fn close_settings(&mut self) {
        self.settings_open = false;
        self.color_picker_open = None;
    }

    /// M14/F16: открыть диалог выхода с таймером (20 с). Настройки
    /// закрываются — диалог выхода всегда верхний.
    fn open_exit(&mut self, kind: ExitKind, cx: &mut Context<'_, Self>) {
        self.settings_open = false;
        self.color_picker_open = None;
        self.exit_timer = Some((kind, self.now_ms() + EXIT_DELAY_MS));
        cx.notify();
    }

    /// M14/F16: выполнить выход — по истечении таймера или «Выйти сейчас».
    fn perform_exit(&mut self, kind: ExitKind) {
        self.exit_timer = None;
        match kind {
            ExitKind::SwitchChar => self.store.back_to_characters(),
            ExitKind::Logout => self.logout(),
        }
    }

    /// M14/F15: настройки — снова Ely `Dialog` (решение пользователя):
    /// Esc/скрим/крестик закрывают, карточка центрируется с анимацией;
    /// тело карточки скроллится самим Dialog'ом (440 px — контент
    /// адаптируется, фикс-размер больше не нужен).
    pub(crate) fn settings_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let title = t(self.lang, Key::SettingsTitle).to_string();
        let close = self.dismiss(cx, |root| root.close_settings());
        Some(
            Dialog::new("ld2-settings", title, close)
                .child(self.settings_content(cx, pal))
                .into_any_element(),
        )
    }

    /// M14/F16: диалог выхода с 20-секундным анимированным таймером
    /// (полоса обновляется тиком 50 мс из tick). В охраняемой зоне
    /// (нет PvP-флага) — кнопка «Выйти сейчас» с мгновенным выходом.
    pub(crate) fn exit_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let (kind, deadline) = self.exit_timer?;
        let now = self.now_ms();
        let left_ms = (deadline - now).max(0);
        let frac = (left_ms as f32 / EXIT_DELAY_MS as f32).clamp(0.0, 1.0);
        let secs = (left_ms + 999) / 1000;
        let guarded = self
            .store
            .snapshot
            .as_ref()
            .map(|s| guarded_zone(&s.location.flags))
            .unwrap_or(true);
        let lang = self.lang;
        let title = match kind {
            ExitKind::SwitchChar => t(lang, Key::BtnSwitchChar),
            ExitKind::Logout => t(lang, Key::BtnLogout),
        }
        .to_string();
        let close = self.dismiss(cx, |root| root.exit_timer = None);
        let cancel = act_btn(
            cx,
            t(lang, Key::CancelBtn),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.exit_timer = None;
            },
        );
        let now_btn = guarded.then(|| {
            act_btn(
                cx,
                t(lang, Key::ExitNowBtn),
                pal.danger,
                0xffffff,
                |this, _cx| {
                    if let Some((kind, _)) = this.exit_timer.take() {
                        this.perform_exit(kind);
                    }
                },
            )
        });
        // Полоса: success → warn → danger по мере убывания.
        let bar_color = if frac > 0.5 {
            pal.success
        } else if frac > 0.25 {
            pal.warn
        } else {
            pal.danger
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().child(tf(lang, Key::ExitCountdown, &[("{s}", &secs.to_string())])))
            .child(
                div()
                    .w_full()
                    .h(px(8.0))
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(frac))
                            .rounded_md()
                            .bg(rgb(bar_color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(cancel)
                    .children(now_btn),
            );
        Some(
            Dialog::new("ld2-exit", title, close)
                .child(body)
                .into_any_element(),
        )
    }

    /// M11.2: диспетчер всплывающих диалогов (Ely `Dialog` поверх контента,
    /// ADR-004): экран → приоритеты game > social > sheet.
    pub(crate) fn dialog_layer(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if self.settings_open || self.exit_timer.is_some() {
            return None;
        }
        // M13.3: диалог регистрации живёт на экране входа (D35) — вне
        // гейта active_dialog (тот смотрит только на экраны мира/персонажей).
        if self.register_open {
            return self.register_dialog(cx, pal);
        }
        // Гейт экрана и приоритет — ядро (E2E-тест M11.6); внутри домена
        // порядок полей зеркалит AppStore::active_dialog.
        let kind = self
            .store
            .active_dialog(self.create_open, self.delete_target.is_some())?;
        match kind {
            ld2_core::store::Dialog::Death
            | ld2_core::store::Dialog::Examine
            | ld2_core::store::Dialog::Loot => self.game_dialog(cx, pal),
            ld2_core::store::Dialog::PartyInvite
            | ld2_core::store::Dialog::GuildInvite
            | ld2_core::store::Dialog::Trade => self.social_dialog(cx, pal),
            ld2_core::store::Dialog::Inscribe => self.sheet_dialog(cx, pal),
            ld2_core::store::Dialog::Create | ld2_core::store::Dialog::Delete => {
                self.chars_dialog(cx, pal)
            }
        }
    }
}

impl Render for Root {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl gpui::IntoElement {
        let pal = palette(&self.config.theme);
        // M16/F29f: вьюпорт кэшируется — базовый размер окна
        // клампится под экран при открытии (в обработчиках нет Window).
        let vp = window.viewport_size();
        self.viewport = (f32::from(vp.width), f32::from(vp.height));
        // Автофокус поля логина на первом кадре (экран входа).
        if self.first_frame {
            self.first_frame = false;
            self.init_inputs(window, cx);
            if self.store.screen == Screen::Login {
                let handle = self.ti(&self.login_input).read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(pal.bg))
            .text_color(rgb(pal.text))
            // M18/F41: клик мимо поля ввода снимает с него фокус
            // (capture-фаза корня — до bubble-фокусировки самого поля:
            // клик по полю вернёт фокус ему, клик мимо оставит снятым).
            // Без этого фокус «залипал» в чате и гейт typing_focused
            // глушил все хоткеи окон после набора текста.
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.typing_focused(window, cx) {
                    tracing::debug!("клик мимо поля → снятие фокуса");
                    window.blur(cx);
                    cx.notify();
                }
            }))
            .child(self.titlebar(cx, &pal))
            .child(self.header(cx, &pal))
            .child(
                // Экран — на остаток высоты между шапкой и статус-баром:
                // size_full внутри даёт 100 % этого контейнера, а не окна
                // (иначе низ уходил за границу, баг M11).
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_h_0()
                    .child(match self.store.screen {
                        Screen::Login => self.login_screen(window, cx, &pal),
                        Screen::Characters => self.characters_screen(cx, &pal),
                        Screen::Game => self.game_screen(cx, &pal),
                    })
                    // Статус-бар логина/выбора: автор+версия в центре,
                    // статус сервера справа (у игры — свой бар с чатом).
                    .when(self.store.screen != Screen::Game, |d| {
                        d.child(self.meta_status_bar(cx, &pal))
                    }),
            )
            // M14/F1: статус сервера живёт в шапке (status_badge).
            // M11.2: всплывающий диалог поверх контента (скрим + панель).
            // M14/F17: карточка навыка — под игровыми диалогами,
            // но над контентом (настройки/выход рисуются позже).
            .when_some(self.skill_info_dialog(cx, &pal), |d, dialog| {
                d.child(dialog)
            })
            // M16/F29b: общий scrim всплывающих окон — под карточками
            // (персонаж и рюкзак могут быть открыты одновременно).
            .when_some(self.popup_scrim(cx), |d, layer| d.child(layer))
            // M15/F25b: окно персонажа — всплывающий диалог;
            // игровые диалоги/настройки/выход рисуются позже — выше.
            .when_some(self.character_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M16/F29b: окно рюкзака (открывается вместе с персонажем).
            .when_some(self.backpack_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M18/F36: окно квестов — поверх рюкзака (порядок слоёв =
            // порядок закрытия по Esc/scrim).
            .when_some(self.quests_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M18/F38: окно диалога NPC — верхний слой группы попапов
            // (модалки смерти/осмотра/луута рисуются позже — выше).
            .when_some(self.npc_window(window, cx, &pal), |d, layer| d.child(layer))
            // M18/F53: описание предмета и окно заточки — попапы
            // (карточка с шапкой и крестиком, без затемнения).
            // Порядок слоёв = порядок Esc/scrim: заточка — верхняя.
            .when_some(self.item_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            .when_some(self.upgrade_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M18: окно сокетов (инкрустация через меню оружия) —
            // popup-диалог, как другие окна (карточка с крестиком).
            .when_some(self.socket_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M20: окно выбора количества — поверх окна сокетов.
            .when_some(self.quantity_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M24: админ-панель — под окном диалога (execute-диалог
            // количества должен открываться поверх).
            .when_some(self.admin_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M20: окно игрового диалога (заготовка) — поверх окна
            // количества.
            .when_some(self.dialog_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M21: окно профессий — поверх окна диалога.
            .when_some(self.professions_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M23: окно подятия предмета — верхний слой группы попапов.
            .when_some(self.pickup_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            .when_some(self.dialog_layer(cx, &pal), |d, dialog| d.child(dialog))
            // M14/F14: настройки — диалог поверх экрана (скрим + Esc).
            .when_some(self.settings_dialog(cx, &pal), |d, layer| d.child(layer))
            .when_some(self.exit_dialog(cx, &pal), |d, dialog| d.child(dialog))
            // R-dnd: подтверждение выброса (поверх всего).
            .when_some(self.drop_confirm_dialog(cx, &pal), |d, dialog| {
                d.child(dialog)
            })
            // M16/F29h: системные уведомления — вверху по центру
            // (ely ToastViewport показывает внизу справа и не
            // позиционируется — заменён; тайминг у Toaster).
            .child(self.toast_stack(window, cx, &pal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F33: открытие окна сбрасывает его контекстное меню —
    /// M18/F55: окно описания предмета «резиновое» — ширина от доли
    /// вьюпорта (28 %, clamp 320..480), а не прежние фиксированные
    /// 520 px; остальные окна от вьюпорта не зависят.
    #[test]
    fn item_popup_width_follows_viewport() {
        // Типичный экран: 28 % от 1600 ≈ 448 — уже прежних 520.
        let (w, h) = Root::popup_base_size(PopupKind::Item, (1600.0, 900.0));
        assert!((w - 448.0).abs() < 0.01, "28% от 1600 = {w}");
        assert_eq!(h, 420.0, "высота — константа, докрутит resize-grip");

        // Широкий экран не даёт окну раздуться, узкий — не схлопывает.
        assert_eq!(
            Root::popup_base_size(PopupKind::Item, (3000.0, 900.0)).0,
            480.0,
            "верхняя граница"
        );
        assert_eq!(
            Root::popup_base_size(PopupKind::Item, (800.0, 600.0)).0,
            320.0,
            "нижняя граница"
        );
        // Viewport ещё не отрисован (0×0) — clamp даёт валидный размер.
        assert_eq!(
            Root::popup_base_size(PopupKind::Item, (0.0, 0.0)).0,
            320.0,
            "до первой отрисовки"
        );

        assert_eq!(
            Root::popup_base_size(PopupKind::Character, (1000.0, 700.0)),
            (760.0, 760.0),
            "персонаж — константа"
        );
    }

    /// иначе меню, открытое в момент закрытия, появлялось бы само
    /// при следующем открытии (stale-запрос `.manual` + гаснущий
    /// keyed state). M24 (заказчик): окно подятия рисует общий
    /// `backpack_grid` — при открытии ЛЮБОГО окна сбрасываются ОБА
    /// меню, иначе в луте самооткрывалось меню рюкзака.
    #[test]
    fn opening_popup_clears_both_context_menus() {
        let mut bp = Some("bp".to_string());
        let mut eq = Some("eq".to_string());
        clear_popup_menu(&mut bp, &mut eq);
        assert!(bp.is_none(), "меню рюкзака сброшено при открытии");
        assert!(eq.is_none(), "меню персонажа сброшено при открытии");
        // Повторное открытие — без паники и без лишних эффектов.
        bp = Some("bp".to_string());
        eq = Some("eq".to_string());
        clear_popup_menu(&mut bp, &mut eq);
        assert!(bp.is_none() && eq.is_none());
    }

    /// M14/F4: боевые хоткеи — дефолтные цифры 1–6 парсятся и
    /// распознаются так же, как навигационные спецификации.
    #[test]
    fn hotkey_spec_round_trip_covers_action_digits() {
        for spec in ["1", "2", "3", "4", "5", "6"] {
            let ks =
                gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).expect(spec);
            assert_eq!(
                spec_from_keystroke(&ks).as_deref(),
                Some(spec),
                "round-trip {spec}"
            );
            assert!(keystroke_matches(spec, &ks), "матч {spec}");
            // Цифра не должна ловиться чужим спецификациям.
            assert!(!keystroke_matches("alt+up", &ks), "не путаем с {spec}");
        }
    }

    /// M14/F4: панель действий настраивается на 13 строк — у каждой
    /// свой id и дефолт из конфига; сеттер пишет только своё поле.
    #[test]
    fn hotkey_rows_cover_nav_and_six_actions() {
        assert_eq!(
            HOTKEY_ROWS.len(),
            15,
            "навигация/цикл + 6 действий + окна персонажа/рюкзака/квестов/профессий"
        );
        let ids: std::collections::HashSet<_> = HOTKEY_ROWS.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), HOTKEY_ROWS.len(), "id строк уникальны");

        let hk = Hotkeys::default();
        assert_eq!((HOTKEY_ROWS[0].get)(&hk), "alt+up", "навигация");
        assert_eq!((HOTKEY_ROWS[4].get)(&hk), "tab", "цикл целей");
        assert_eq!((HOTKEY_ROWS[5].get)(&hk), "1", "атака");
        assert_eq!((HOTKEY_ROWS[6].get)(&hk), "2", "мощный удар");
        assert_eq!((HOTKEY_ROWS[7].get)(&hk), "3", "раскол (зарезервирован)");
        assert_eq!((HOTKEY_ROWS[8].get)(&hk), "4", "побег (не Esc: диалоги)");
        assert_eq!((HOTKEY_ROWS[9].get)(&hk), "5", "руна");
        assert_eq!((HOTKEY_ROWS[10].get)(&hk), "6", "расходник");
        assert_eq!((HOTKEY_ROWS[11].get)(&hk), "c", "окно персонажа (M15/F25)");
        assert_eq!((HOTKEY_ROWS[12].get)(&hk), "i", "окно рюкзака (M16/F29b)");
        assert_eq!((HOTKEY_ROWS[13].get)(&hk), "j", "окно квестов (M18/F36)");
        assert_eq!((HOTKEY_ROWS[14].get)(&hk), "p", "окно профессий (M21)");

        // Сеттер переписывает только своё поле.
        let mut hk = Hotkeys::default();
        (HOTKEY_ROWS[5].set)(&mut hk, "f5".into());
        assert_eq!(hk.action_attack, "f5");
        assert_eq!(hk.action_flee, "4", "остальные не тронуты");
        assert_eq!((HOTKEY_ROWS[5].get)(&hk), "f5");
    }

    /// M15/F25e: жест с шапки (drag) не закрывает окно по завершении.
    #[test]
    fn scrim_close_requires_gesture_started_on_scrim() {
        assert!(
            scrim_closes_on_up(true, false),
            "настоящий клик мимо (down+up на scrim) закрывает"
        );
        assert!(
            !scrim_closes_on_up(false, false),
            "жест начат не на scrim → не закрываем"
        );
        assert!(
            !scrim_closes_on_up(true, true),
            "завершение перетаскивания (active_drag) не закрывает"
        );
    }

    /// M15/F25d: позиция карточки — центр + смещение, без половинности.
    #[test]
    fn char_card_origin_centers_and_offsets() {
        assert_eq!(
            char_card_origin(1200.0, 800.0, 760.0, 560.0, (0.0, 0.0)),
            (220.0, 120.0),
            "по центру"
        );
        assert_eq!(
            char_card_origin(1200.0, 800.0, 760.0, 560.0, (50.0, -30.0)),
            (270.0, 90.0),
            "смещение от центра"
        );
        assert_eq!(
            char_card_origin(400.0, 300.0, 760.0, 560.0, (0.0, 0.0)),
            (0.0, 0.0),
            "карточка больше вьюпорта → в угол"
        );
    }

    /// M15/F25c: кламп размера карточки окна персонажа.
    #[test]
    fn char_size_clamps_to_min_and_viewport() {
        assert_eq!(
            clamp_char_size(100.0, 100.0, 1200.0, 800.0),
            (560.0, 400.0),
            "min"
        );
        assert_eq!(
            clamp_char_size(760.0, 560.0, 1200.0, 800.0),
            (760.0, 560.0),
            "норма"
        );
        assert_eq!(
            clamp_char_size(5000.0, 5000.0, 1200.0, 800.0),
            (1160.0, 760.0),
            "max = вьюпорт − 40"
        );
        assert_eq!(
            clamp_char_size(900.0, 700.0, 500.0, 300.0),
            (560.0, 400.0),
            "вьюпорт меньше min → min"
        );
    }

    /// M15: хоткей «c» работает на латинице И на русской раскладке
    /// (система отдаёт символ раскладки: «с»).
    #[test]
    fn hotkey_matches_across_layouts() {
        let en = gpui::Keystroke::parse_with_os("c", host_os()).expect("спек парсится");
        assert!(keystroke_matches("c", &en), "латиница ↔ латиница");

        let mut ru = en.clone();
        ru.key = "с".into();
        assert!(keystroke_matches("c", &ru), "спек «c» ← нажатие на русской");

        // Многосимвольные спеки и «соседние» клавиши не ломаются.
        let tab = gpui::Keystroke::parse_with_os("tab", host_os()).expect("tab");
        assert!(keystroke_matches("tab", &tab));
        assert!(!keys_physically_equal("tab", "t"));
        assert!(!keys_physically_equal("c", "x"), "чужая клавиша");
    }

    /// M15/F25i: `TYPING_FIELDS` покрывает все TextInput Root, а экран
    /// игры — только его собственные поля: залипший фокус пароля после
    /// Login → Game не блокирует хоткеи.
    #[test]
    fn typing_fields_cover_every_input_and_gate_game() {
        const ALL: [&str; 16] = [
            "login",
            "password",
            "reg_login",
            "reg_email",
            "reg_password",
            "reg_password2",
            "host",
            "domain",
            "guild",
            "chat",
            "chat_filter",
            "delete_name",
            "create_name",
            "admin_search",
            "admin_inv_search",
            "admin_player",
        ];
        for field in TYPING_FIELDS {
            assert!(
                ALL.contains(&field.name),
                "неизвестное поле {:?} на экране {:?}",
                field.name,
                field.screen
            );
        }
        for name in ALL {
            assert!(
                TYPING_FIELDS.iter().any(|f| f.name == name),
                "поле {name} не проверяется ни на одном экране"
            );
        }
        let game: Vec<&str> = TYPING_FIELDS
            .iter()
            .filter(|f| f.screen == Screen::Game)
            .map(|f| f.name)
            .collect();
        assert_eq!(
            game,
            [
                "chat",
                "chat_filter",
                "guild",
                "host",
                "domain",
                "admin_search",
                "admin_inv_search",
                "admin_player",
            ],
            "игровой экран проверяет только свои поля"
        );
    }

    /// M14/F4: гейт «Мощного удара» — владение мечом 5 (skills.md §4).
    #[test]
    fn power_strike_needs_sword_rank_five() {
        use ld2_protocol::types::SkillSnapshot;
        let skill = |id: &str, rank: u32| SkillSnapshot {
            id: id.to_string(),
            name: None,
            description: None,
            group: None,
            rank,
            xp: 0,
            xp_to_next: 120,
        };
        assert!(!game::power_strike_open(&[]), "навыков нет");
        assert!(
            !game::power_strike_open(&[skill("sword", 4)]),
            "ранг 4 — закрыто"
        );
        assert!(
            game::power_strike_open(&[skill("sword", 5)]),
            "ранг 5 — открыто"
        );
        assert!(
            !game::power_strike_open(&[skill("axe", 10)]),
            "чужой навык не открывает"
        );
    }

    /// M14/F1: цвет и ключ статуса — по роли соединения (3 тона).
    #[test]
    fn status_color_follows_transport_status() {
        let pal = palette("dark");
        let reconnect = TransportStatus::Reconnecting {
            attempt: 2,
            delay: Duration::from_secs(3),
        };
        assert_eq!(
            status_color(&pal, &TransportStatus::Online),
            (pal.success, Key::StatusOnline)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::Connecting),
            (pal.warn, Key::StatusConnecting)
        );
        assert_eq!(
            status_color(&pal, &reconnect),
            (pal.warn, Key::StatusReconnecting)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::AuthExpired),
            (pal.danger, Key::StatusAuthExpired)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::Stopped),
            (pal.danger, Key::StatusStopped)
        );
        // Светлая тема — те же роли цветов.
        let light = palette("light");
        assert_eq!(
            status_color(&light, &TransportStatus::Online).0,
            light.success
        );
        assert_ne!(
            status_color(&light, &TransportStatus::Stopped).0,
            light.success
        );
    }

    /// M14/F21: детект русской раскладки по id/имени источника ввода.
    #[test]
    fn cyrillic_layout_detected() {
        assert!(is_cyrillic_layout("com.apple.keylayout.Russian", "Русская"));
        assert!(is_cyrillic_layout(
            "com.apple.keylayout.Russian-PC",
            "Russian"
        ));
        assert!(is_cyrillic_layout("ru", "QWERTY"), "Linux/xkb: id ru");
        assert!(is_cyrillic_layout("com.apple.keylayout.ABC", "русская"));
        assert!(!is_cyrillic_layout("com.apple.keylayout.ABC", "ABC"));
        assert!(!is_cyrillic_layout("", ""), "пустой источник — не русская");
    }

    /// M14/F1: бейджи локации — PvP по флагу, иначе Safe; прочие — muted.
    /// M14/F16: охраняемая зона — нет PvP-флага (вне мира — тоже).
    #[test]
    fn guarded_zone_classifies_flags() {
        assert!(guarded_zone(&[]));
        assert!(guarded_zone(&["safe".into()]));
        assert!(!guarded_zone(&["pvp".into()]));
        assert!(!guarded_zone(&["safe".into(), "pvp".into()]));
    }

    #[test]
    fn location_badges_classifies_flags() {
        assert_eq!(
            location_badges(&[]),
            vec![(BadgeTone::Success, "safe".to_string())]
        );
        assert_eq!(
            location_badges(&["safe".into()]),
            vec![(BadgeTone::Success, "safe".to_string())]
        );
        assert_eq!(
            location_badges(&["pvp".into()]),
            vec![(BadgeTone::Warn, "pvp".to_string())]
        );
        // Прочие флаги — muted-бейджи после основного, без дублей pvp/safe.
        assert_eq!(
            location_badges(&["pvp".into(), "dark".into()]),
            vec![
                (BadgeTone::Warn, "pvp".to_string()),
                (BadgeTone::Muted, "dark".to_string())
            ]
        );
        assert_eq!(
            location_badges(&["safe".into(), "no_teleport".into()]),
            vec![
                (BadgeTone::Success, "safe".to_string()),
                (BadgeTone::Muted, "no_teleport".to_string())
            ]
        );
    }

    /// M14/F1: подписи бейджей локализуются, неизвестные — как есть.
    #[test]
    fn badge_label_maps_known_tokens() {
        assert_eq!(badge_label(Lang::Ru, "pvp"), "PvP");
        assert_eq!(badge_label(Lang::En, "pvp"), "PvP");
        assert_eq!(badge_label(Lang::Ru, "safe"), "Безопасно");
        assert_eq!(badge_label(Lang::En, "safe"), "Safe");
        assert_eq!(badge_label(Lang::Ru, "dark"), "dark");
    }

    /// Запись хоткея обязана давать спецификацию, парсящуюся обратно
    /// (config.toml ↔ gpui Keystroke, ФР-19).
    #[test]
    fn hotkey_spec_round_trip() {
        for spec in ["alt+up", "tab", "shift+left", "ctrl+f5", "alt+left"] {
            let ks =
                gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).expect(spec);
            assert_eq!(
                spec_from_keystroke(&ks).as_deref(),
                Some(spec),
                "round-trip {spec}"
            );
            assert!(keystroke_matches(spec, &ks), "матч {spec}");
        }
        assert!(!keystroke_matches(
            "alt+up",
            &gpui::Keystroke::parse_with_os("up", host_os()).unwrap()
        ));
        assert!(!keystroke_matches(
            "bogus+key",
            &gpui::Keystroke::parse_with_os("up", host_os()).unwrap()
        ));
    }
}
