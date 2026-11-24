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
mod badges;
mod chars;
mod chat;
mod chrome;
mod clicks;
mod dialog;
mod drag;
mod helpers;

pub(crate) use drag::{BpHold, ItemDragGhost, MapPan, PopupDrag, PopupResize, TitlebarDrag};
mod game;
mod hotkeys;
mod lifecycle;
mod pickup;
mod popups;
mod professions;
mod progress;
mod quantity;
mod quests;
mod register;
mod screens;
mod settings;
mod sheet;
mod social;
mod tick;
mod typing;
mod view;

pub(crate) use typing::TYPING_FIELDS;
mod popup_types;

pub(crate) use badges::{
    badge, badge_label, guarded_zone, location_badges, status_badge, BadgeTone,
};
pub(crate) use helpers::{char_card_origin, clamp_char_size, scrim_closes_on_up};
pub(crate) use popup_types::{
    DialogTextFields, ExitKind, PopupKind, PopupState, SendSlot, EXIT_DELAY_MS,
};

use helpers::{host_os, is_cyrillic_layout, physical_key_char, spec_from_keystroke};

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

use ely::theme::{Mode, Theme};

/// Release-сборка: внешний `ws://` запрещён (НФР безопасность, guard M10.1).
const RELEASE: bool = !cfg!(debug_assertions);

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
    /// M28: показывать в рюкзаке все надетые сумки (настройка).
    SetBackpackShowAllBags(bool),
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
    /// M28/B4: выбранная сумка (uid) — её содержимое показывается
    /// вторым разделом окна рюкзака.
    backpack_bag: Option<String>,
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
            backpack_bag: None,
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

    // ── элементы ───────────────────────────────────────────────────────────
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
