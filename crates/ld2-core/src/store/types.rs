//! Типы состояния клиента (`AppStore` и вспомогательные) — вынесены
//! из `store.rs` (RF4a). Поведение не менялось: только перенос.

use super::*;

/// Экран приложения (ФР-01, ФР-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Characters,
    Game,
}

/// Команда, ожидающая ack (корреляция ошибок формы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Login,
    /// `cmd/auth.register` — ошибки формы регистрации (M13, D35): поле +
    /// текст сервера, маппинг кодов — [`crate::register::register_field_for`].
    Register,
    Create,
    Select,
    Delete,
    /// `location.move` — ошибки (закрытый выход) → `world_error`.
    Move,
    /// `combat.attack` → `world_error`.
    Attack,
    /// `entity.info` → `entity_detail`.
    Examine,
    /// M35: `char.inspect` → `inspect` (карточка чужого персонажа).
    Inspect,
    /// M23: `entity.info` → `pickup` (окно подятия предмета — лут
    /// трупа отдельно от модалки осмотра).
    PickupInfo,
    /// `npc.talk` → `npc_dialog` (M18/F35).
    NpcTalk,
    /// `npc.reply` → новый узел в `npc_dialog` (M18/F37).
    NpcReply,
    /// `shop.list` → `npc_dialog.shop` (M18/F39).
    ShopList,
    /// `shop.buy` — изменения приходят событиями (M18/F39).
    ShopBuy,
    /// `char.xp_invest` → `pool_left` в снапшоте.
    Invest,
    /// `item.equip`/`item.unequip` — ack несёт обновлённый `char` (§7).
    Equip,
    Unequip,
    /// `item.use`/`item.drop` — результат приходит событиями (§8).
    Drop,
    /// `upgrade.preview` → payload превью в `upgrade.preview`.
    Preview,
    /// `upgrade.start` → результат событием (ФР-10).
    Upgrade,
    /// M18/F54: `item.use` камня усиления → ack `{stone_uid, quality}`.
    Sharpen,
    /// `socket.insert`/`socket.extract` → ack несёт посох/руну (§7).
    Socket,
    /// Каст из hotbar (ФР-09) — ошибки в мир (`E_NOT_ENOUGH_MP`…).
    Cast,
    /// `cmd/crime.log` → журнал нарушений в правую панель (ФР-11).
    CrimeLog,
    /// `craft.learn` → ack {profession} обновляет снапшот (ФР-08).
    Learn,
    /// `chat.send` — ack несёт только ошибки (лимиты §5), события — отдельно.
    Chat,
    /// `chat.delete` (M14/F19) — ошибки прав/лимиты → notice.
    ChatDelete,
    /// `craft.start`/`craft.recipes` — результат крафта событиями (ФР-08).
    Craft,
    /// `quest.accept|turn_in|abandon` — результат событиями (ФР-12).
    Quest,
    /// `party.*` — ack несёт `{party}` на create/accept, остальное — события.
    Party,
    /// `trade.*` — состояние только событиями `evt/trade/*` (ФР-15).
    Trade,
    /// `guild.browse` → ack {guilds[]} — список для вкладки (ФР-16).
    GuildBrowse,
    /// `guild.*` — ack несёт `{guild}` на create/join, остальное — события.
    Guild,
    /// `cmd/dialog.choice`/`cmd/dialog.submit` — игровые диалоги
    /// (M20, заготовка): ack — только ошибки.
    DialogChoice,
    DialogSubmit,
    /// `cmd/resource.gather` (M20/F20b) — ошибки (нет ресурса/ранг/
    /// инструмент) → world_error.
    Gather,
    /// `cmd/craft.rank_up` (M21) — ack {profession}; ошибки тостом.
    CraftRankUp,
    /// `cmd/craft.reset` (M21) — сброс профессии: убрать из снапшота.
    ProfessionReset,
    /// `cmd/corpse.harvest` (M22) — старт разделки; ошибки тостом.
    Harvest,
    /// M24: `cmd/admin.items` → `admin_catalog` (каталог админ-окна).
    AdminCatalog,
    /// M24: `cmd/admin.give.ask`/`admin.remove.ask` — ack только
    /// ошибки (откроется execute-диалог количества).
    AdminAsk,
    /// M24: `cmd/admin.god_mode` → ack `{enabled}` → `admin_god`.
    AdminGod,
    /// M24: `cmd/admin.target.info` → `admin_target_items`.
    AdminTarget,
    /// M24: `cmd/admin.clear` — очистка цели (ack только ошибки).
    AdminClear,
    /// M26: `cmd/map.world` → ack {locations, offset, total, root} —
    /// чанки карты мира (насос в tick клиента).
    MapWorld,
}

/// Открытая модалка улучшения (ФР-10): предмет + превью от сервера.
#[derive(Debug, Clone, PartialEq)]
pub struct UpgradeView {
    pub item_uid: String,
    pub item_name: String,
    /// payload `upgrade.preview`: `{preview_id, delta, cost, chance}`.
    pub preview: Option<serde_json::Value>,
    /// M18/F53: последний результат `evt/upgrade/result` (модалка не
    /// закрывается — показываем итог и позволяем улучшать дальше).
    pub result: Option<serde_json::Value>,
}

/// M18/F54: выбранный камень усиления — активен, пока сервер его не
/// снимет (последняя штука потрачена) или игрок не закроет окно.
#[derive(Debug, Clone, PartialEq)]
pub struct SharpenStone {
    pub uid: String,
    pub name: String,
    /// Качество в процентных пунктах (`stats.quality`; −5/0/+5).
    pub quality: i64,
}

/// Цветовой тип строки журнала мира (ФР-17; палитра §7.4 — в UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    System,
    Location,
    /// Нападение персонажа (исходящий урон).
    CombatOut,
    /// Урон по персонажу (входящий).
    CombatIn,
    Magic,
    Loot,
    Crime,
    Quest,
    Xp,
    Chat,
    /// R-чат (заказчик 2026-11-20): журнал — сообщения каналов своим
    /// цветом; Chat — канал локации (по умолчанию/старый сервер).
    ChatGlobal,
    ChatParty,
    ChatGuild,
    ChatWhisper,
    ChatSystem,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub kind: LogKind,
    pub text: String,
    /// M25: момент добавления (epoch, мс) — истечение журнала.
    pub at: i64,
}

/// Посещённая локация (ФР-03: кэш для истории; граф — v1.1, D9).
#[derive(Debug, Clone, PartialEq)]
pub struct VisitedLocation {
    pub id: String,
    pub name: String,
}

/// Побочные эффекты — исполняет слой приложения (не редьюсер).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Сохранить refresh-токен в keyring и запомнить логин (ФР-01, А4).
    PersistSession {
        login: String,
        refresh_token: String,
    },
    /// Токен отозван/истёк — убрать из keyring под указанным логином.
    ForgetSession { login: String },
    /// Прокинуть refresh-токен в транспорт (реконнекты без логина, §4.5).
    TransportToken(String),
    /// OS-уведомление (D7, feature-flag `desktop.notify` в настройках).
    Notify(NotifyKind),
    /// R11: дельта неприменима — запросить полный снапшот (§12.4).
    /// Исполнитель (root) шлёт `cmd/state.resync` с текущей версией.
    ResyncState,
}

/// События для системных уведомлений (D7: смерть, приглашения, whisper,
/// результат улучшения). Локализуется в `App` — стор не знает языка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyKind {
    Death,
    PartyInvite,
    GuildInvite,
    TradeOpen,
    Whisper,
    Upgrade,
}

/// M25: активный стан — всплывающее окно с прогрессбаром времени.
#[derive(Debug, Clone, PartialEq)]
pub struct StunView {
    pub title: String,
    /// Срок стана (epoch, мс).
    pub until_ms: i64,
    /// Момент наложения (для прогрессбара).
    pub started_ms: i64,
}

/// Отложенное удаление персонажа (M11.7, D20/ADR-005): клиент шлёт
/// `char.delete` по таймеру; смена экрана (вход в игру/логаут) — отмена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    pub char_id: String,
    pub name: String,
    /// Эпоха в секундах ([`local_epoch`]): когда уходит `char.delete`.
    pub deadline: i64,
}

/// Снятие шкур (M22): прогресс-бар разделки на клиенте — старт
/// (epoch-мс отправки команды), действие и имя животного.
#[derive(Debug, Clone, PartialEq)]
pub struct HarvestProgress {
    pub started_ms: i64,
    /// `skin` | `gut`.
    pub action: String,
    pub name: String,
    /// Труп-источник (M24): `evt/harvest/result` гасит бар только при
    /// совпадении — итог прерванного переключением действия чужого
    /// трупа новый бар не трогает.
    pub entity_id: String,
}

// ── AppStore и типы, на которые он ссылается напрямую (RF4a, часть 2) ──

#[derive(Debug)]
pub struct AppStore {
    pub screen: Screen,
    pub session: Option<SessionConfig>,
    pub snapshot: Option<StateSnapshot>,
    pub transport: TransportStatus,
    pub pending: Option<Act>,
    /// Ошибки форм (текст сервера — русский, §9).
    pub login_error: Option<String>,
    /// Ошибка формы регистрации (M13): поле для inline-подсветки + текст
    /// (сервера §9 или клиентской проверки D34 — локализует UI).
    pub register_error: Option<crate::register::RegisterError>,
    pub create_error: Option<String>,
    pub select_error: Option<String>,
    /// Ошибки действий мира (move/attack/examine), ФР-03.
    pub world_error: Option<String>,
    /// Модалка смерти (ФР-06): закрывается только подтверждением.
    pub death: Option<ld2_protocol::evt::CharDeath>,
    /// Описание предмета (ФР-07 «показать описание»).
    pub item_detail: Option<serde_json::Value>,
    /// Модалка улучшения (ФР-10).
    pub upgrade: Option<UpgradeView>,
    /// M18/F54: активный камень усиления.
    pub sharpen_stone: Option<SharpenStone>,
    /// M18/F55: окно заточки открыто (экран списка показан) — гейт
    /// рендера. Переживает расход последнего камня, чтобы «назад»
    /// возвращал к списку, а не закрывал окно; снимается только при
    /// закрытии окна (Esc/крестик/скрим).
    pub sharpen_list: bool,
    /// Открытая инкрустация (ФР-09): uid посоха.
    pub socketing: Option<String>,
    /// M20: открытый игровой диалог (заготовка: страницы/ввод из
    /// `evt/dialog/open`; окно — `PopupKind::Dialog`).
    pub dialog: Option<ld2_protocol::evt::DialogOpen>,
    /// Снятие шкур (M22): прогресс активной разделки (5 с) для
    /// прогресс-бара; ставится клиентом при отправке, чистится по
    /// `evt/harvest/result` или ошибке.
    pub harvest: Option<HarvestProgress>,
    /// M17/F30a: открытый выбор оружия под инкрустацию руны (uid руны).
    pub inscribe: Option<String>,
    /// Смещение часов сервера (evt/time.sync): server = local + offset.
    pub time_offset: i64,
    /// Язык клиента (заказчик 2026-11-20: журнал — переводы каналов
    /// чата); меняется настройкой языка.
    pub lang: Lang,
    /// R16 (§1.1): последние замеры смещения — медиана (один сэмпл
    /// скачет от сетевого джиттера и скачков системных часов).
    pub time_samples: std::collections::VecDeque<i64>,
    /// M24: фаза тикера зарядов рун (шкала `now_ms`) — прогрессбар
    /// заполнения рун; ставится по снапшоту и по приросту зарядов
    /// в `evt/item/state` (тикер шлёт патч сокетов — такт виден).
    pub rune_recharge_phase_ms: Option<i64>,
    /// Выбранная цель (ФР-05): клик по строке / Tab.
    pub target: Option<String>,
    /// Кулдауны активных навыков: skill_id → until epoch (ФР-05).
    pub combat_cooldowns: BTreeMap<String, i64>,
    /// M17/F30g: кулдауны предметов `item:<def_id>` → (until мс,
    /// полная длительность мс) — таймер и прогресс-бар кнопок hotbar.
    pub item_cooldowns: BTreeMap<String, (i64, i64)>,
    /// Глобальный кулдаун боевых действий (M14/F4): epoch **мс**, до
    /// которого заблокированы кнопки панели — клиентская блокировка
    /// 1 с (сервер не меняется; приоритет — max(основной, глобальный)).
    pub global_cooldown_until: i64,
    /// Журнал нарушений из `cmd/crime.log` (ФР-11).
    pub crime_log: Vec<CrimeLogEntry>,
    /// Журнал уже загружен (вкладка «Статусы» запрашивает один раз).
    pub crime_log_loaded: bool,
    /// Подтверждение «забрать лут трупа ИГРОКА» (ФР-11): (uid, name).
    pub loot_confirm: Option<(String, String)>,
    /// Трекер квестов (ФР-12): id, максимум 3, рендер с фильтром по наличию.
    pub quest_tracker: Vec<String>,
    /// История чата по каналам (ФР-13): `channel → сообщения` (ring).
    pub chat: std::collections::HashMap<String, std::collections::VecDeque<ChatMessage>>,
    /// Активный канал чата (вкладка в UI, ФР-13).
    pub chat_channel: String,
    /// Адресат последнего whisper (ФР-13: «ответить»).
    pub last_whisper_from: Option<String>,
    /// Группа (ФР-14): полное состояние из `evt/party/updated`/снапшота.
    pub party: Option<PartyState>,
    /// Входящее приглашение в группу (поле `invite`, §11) — модалка ответа.
    pub party_invite: Option<PartyInviteView>,
    /// Сделка — моя перспектива `you/them` (ФР-15, версия — истина сервера).
    pub trade: Option<ld2_protocol::evt::TradeState>,
    /// Гильдия (ФР-16): состояние из `evt/guild/updated`/снапшота.
    pub guild: Option<GuildState>,
    /// Входящее приглашение в гильдию (поле `invite`, §11).
    pub guild_invite: Option<GuildInviteView>,
    /// Список гильдий из `cmd/guild.browse` (поиск/вступление).
    pub guild_browse: Vec<serde_json::Value>,
    /// Журнал гильдии (§8 `evt/guild/log`).
    pub guild_log: Vec<GuildLogEntry>,
    /// Список гильдий уже запрошен (вкладка грузит один раз).
    pub guild_browse_loaded: bool,
    /// Лимит истории чата на канал (НФР §8; из `config.toml`, ФР-19).
    pub chat_limit: usize,
    /// Последний измеренный RTT, мс (НФР: RTT-индикатор).
    pub rtt_ms: Option<u64>,
    /// Рецепты крафта из `cmd/craft.recipes` (ФР-08).
    pub recipes: Vec<serde_json::Value>,
    /// Рецепты уже запрошены (вкладка грузит один раз).
    pub recipes_loaded: bool,
    /// Нейтральное сообщение (создан/удалён/транспорт).
    pub notice: Option<String>,
    /// R3: min_client с последнего session.start (для подписи) и
    /// жёсткий флаг «обновление обязательно» — вход в мир заблокирован
    /// (char.select не отправляется, chars.rs).
    pub min_client_required: Option<String>,
    pub forced_update: bool,
    /// Логин последней попытки входа — предзаполнение формы.
    pub last_login: Option<String>,
    /// Журнал мира (ФР-17): ring-буфер [`LOG_LIMIT`].
    pub log: VecDeque<LogEntry>,
    /// История посещённых локаций (ФР-03), свежие в конце.
    pub visited: Vec<VisitedLocation>,
    /// Результат `entity.info` — модалка осмотра (включая лут трупа).
    pub entity_detail: Option<serde_json::Value>,
    /// M35: публичный лист чужого персонажа (ПКМ-осмотр).
    pub inspect: Option<ld2_protocol::types::CharInspectView>,
    /// M23: данные окна подятия предмета (payload `entity.info`:
    /// лут трупа + блок «Внутри»); гейт рендера — `Root::pickup_popup`.
    pub pickup: Option<serde_json::Value>,
    /// M23: лут трупа обновился на сервере (разделка/новый труп) —
    /// клиент перечитает `pickup`, пока открыто окно подятия.
    pub pickup_dirty: bool,
    /// M24: каталог предметов админ-окна (вариант A — один запрос,
    /// кэш на сессию, фильтрация на клиенте).
    pub admin_catalog: Option<Vec<serde_json::Value>>,
    /// M26: карта-граф мира — накопленные локации (чанки
    /// `cmd/map.world`; порядок сервера стабилен по id).
    pub map_locations: Vec<serde_json::Value>,
    /// M26: прогресс загрузки карты: следующий offset / всего.
    pub map_offset: usize,
    pub map_total: usize,
    /// M26: запрос следующего чанка (насос: apply_ack ставит при
    /// `offset < total`).
    pub map_request: bool,
    /// M26: корень раскладки (id спавн-локации) — из ack.
    pub map_root: String,
    /// M24: состояние «режим бога» (эхо `cmd/admin.god_mode`).
    pub admin_god: bool,
    /// M25: активный стан (окно с прогрессбаром; истекает в tick).
    pub stun: Option<StunView>,
    /// M24: содержимое выбранной цели правой колонки (кроме self —
    /// тот приходит снапшотом).
    pub admin_target_items: Option<Vec<serde_json::Value>>,
    /// M18/F35: открытое окно диалога NPC (реплики + квесты).
    pub npc_dialog: Option<NpcDialogView>,
    /// M11.7: отложенное удаление персонажа (D20) — не больше одного.
    pub pending_delete: Option<PendingDelete>,
    /// M11.7: цель текущего `char.delete` — снять карточку по ack.
    pub deleting_char: Option<String>,
    /// Логин текущей сессии — для ключа keyring (`<host>:<login>`).
    pub(super) session_login: Option<String>,
}

/// Входящее приглашение в группу (§8 `evt/party/updated` c `invite`,
/// §11 — новое опциональное поле).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct PartyInviteView {
    pub invite_id: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub from_name: String,
}

/// Входящее приглашение в гильдию (§11 — новое опциональное поле).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct GuildInviteView {
    pub guild_id: String,
    #[serde(default)]
    pub guild_name: String,
    #[serde(default)]
    pub from_name: String,
}

/// Результат разбора ввода чата (ФР-13: слэш-команды).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatDraft {
    /// Отправить `cmd/chat.send`.
    Send {
        channel: String,
        to: Option<String>,
        text: String,
    },
    /// Неизвестная команда — ошибка тостом, в чат НЕ отправляется (ФР-13).
    Unknown(String),
}

/// Профиль персонажа (D3: Воин/Лучник/Маг — wire-значения для
/// `char.create`; M17/F30b: «Вор» → «Лучник»). Общий для обоих
/// клиентов (D12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    #[default]
    Warrior,
    Archer,
    Mage,
}

/// M18/F34: пол персонажа — wire-значения `cmd/char.create.gender`
/// (`male`/`female`); общий для клиентов (D12), задаётся при создании.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gender {
    #[default]
    Male,
    Female,
}
