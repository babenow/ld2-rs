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
use ld2_protocol::Command;

mod admin;
mod backpack;
mod chars;
mod chat;
mod chrome;
mod dialog;
mod game;
mod hotkeys;
mod pickup;
mod popups;
mod professions;
mod quantity;
mod quests;
mod register;
mod screens;
mod settings;
mod sheet;
mod social;
mod tick;

pub(crate) use settings::hex_to_u32;
use settings::SettingsTab;

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

/// M26: панорама карты мира — перетаскивание внутри окна карты
/// (левый drag без действия; общий payload как у PopupDrag).
#[derive(Debug)]
struct MapPan;

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
    /// M26: окно карты мира (весь мир, зум/панорама) — заказчик.
    Map,
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
            PopupKind::Map => "ld2-map",
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
    /// M26: окно карты мира — popup (зум/панорама, заказчик).
    map_popup: PopupState,
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
    /// M26: масштаб окна карты (колесо мыши;1.0 — базовый).
    map_zoom: f32,
    /// M26: сдвиг окна карты (панорама); `None` — центр на текущей
    /// (пересчитывается при каждом открытии).
    map_pan: Option<(f32, f32)>,
    /// M26: последняя позиция курсора при панораме (дельта).
    map_drag_last: Option<(f32, f32)>,
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

/// Куда положить ошибку отправки команды.
#[derive(Clone, Copy)]
enum SendSlot {
    Notice,
    World,
    Login,
    Create,
    Select,
}

impl Root {
    /// Отправить команду серверу, вернув ошибку связи текстом.
    fn try_send<C: Command>(&mut self, cmd: &C) -> Result<(), String> {
        self.transport
            .send(cmd)
            .map(|_| ())
            .map_err(|e| format!("нет связи: {e}"))
    }

    /// Отправить команду серверу; при обрыве связи — сообщение в нужный слот.
    /// `true` — команда ушла, можно обновлять локальное состояние.
    fn send_or_notice<C: Command>(&mut self, cmd: &C, slot: SendSlot) -> bool {
        match self.try_send(cmd) {
            Ok(()) => true,
            Err(msg) => {
                match slot {
                    SendSlot::Notice => self.store.notice = Some(msg),
                    SendSlot::World => self.store.world_error = Some(msg),
                    SendSlot::Login => self.store.login_error = Some(msg),
                    SendSlot::Create => self.store.create_error = Some(msg),
                    SendSlot::Select => self.store.select_error = Some(msg),
                }
                false
            }
        }
    }

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
            map_zoom: 1.0,
            map_pan: None,
            map_drag_last: None,
            map_popup: PopupState::new((980.0, 660.0)),
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
        if self.send_or_notice(&cmd, SendSlot::Login) {
            self.store.on_login_sent(&login)
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
        let _ = self.try_send(&ld2_protocol::cmd::AuthLogout {});
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
        if self.send_or_notice(&cmd, SendSlot::World) {
            self.store.on_world_sent(Act::Move)
        }
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
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Move)
                }
            }
            Click::OpenReleases => cx.open_url(ld2_core::update_check::RELEASES_PAGE),
            // ── M10.4: игровой экран ──
            Click::Attack(id) => {
                let cmd = ld2_protocol::cmd::CombatAttack { target_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Attack);
                    // M14/F4: любое боевое действие → глобальный кулдаун.
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Attack);
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            Click::Examine(id) => {
                let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Examine)
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
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::NpcTalk);
                } else {
                    tracing::debug!("cmd/npc.talk не ушла: нет связи");
                }
            }
            Click::PartyInvite(player_id) => {
                let cmd = ld2_protocol::cmd::PartyInvite { player_id };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_sheet_sent(Act::Party)
                }
            }
            Click::TradeOpen(player_id) => {
                let cmd = ld2_protocol::cmd::TradeOpen { player_id };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_sheet_sent(Act::Trade)
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
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Cast);
                    self.store.begin_global_cooldown(self.now_ms());
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
                if self.send_or_notice(&cmd, SendSlot::World) {
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
            }
            Click::PowerStrike => {
                let cmd = ld2_protocol::cmd::SkillUse {
                    skill_id: "power_strike".into(),
                    target_id: self.store.target.clone(),
                };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Cast);
                    self.store.begin_global_cooldown(self.now_ms());
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
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Gather)
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
                if self.send_or_notice(&cmd, SendSlot::World) {
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
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminAsk)
                }
            }
            Click::AdminRemove(item_ref) => {
                let cmd = ld2_protocol::cmd::AdminRemoveAsk {
                    target: self.admin_target.clone(),
                    item_ref,
                };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminAsk)
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
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminClear);
                    if self.admin_target != "self" {
                        self.admin_target_request = true;
                    }
                }
            }
            Click::AdminGod(enabled) => {
                let cmd = ld2_protocol::cmd::AdminGodMode { enabled };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminGod)
                }
            }
            Click::CloseDetail => self.store.entity_detail = None,
            Click::SwitchChar => self.open_exit(ExitKind::SwitchChar, cx),
        }
    }

    // ── элементы ───────────────────────────────────────────────────────────
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

    /// M14/F16: выполнить выход — по истечении таймера или «Выйти сейчас».
    fn perform_exit(&mut self, kind: ExitKind) {
        self.exit_timer = None;
        match kind {
            ExitKind::SwitchChar => self.store.back_to_characters(),
            ExitKind::Logout => self.logout(),
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
            // M26: окно карты мира — поверх квестов (заказчик: кнопка
            // в шапке + клавиша «m»).
            .when_some(self.map_dialog(window, cx, &pal), |d, layer| d.child(layer))
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
    use super::hotkeys::keystroke_matches;
    use super::settings::HOTKEY_ROWS;
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
            16,
            "навигация/цикл + 6 действий + окна персонажа/рюкзака/квестов/профессий/карты"
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
        assert_eq!((HOTKEY_ROWS[15].get)(&hk), "m", "окно карты (M26)");

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
