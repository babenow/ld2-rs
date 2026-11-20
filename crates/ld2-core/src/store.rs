//! `AppStore` — единое состояние клиента, собираемое **только** из событий
//! сервера (ТЗ §6.2). UI — проекция стора; редьюсер чистый и тестируется без
//! egui (ТЗ §15.5: «UI-фича без reducer-теста не мёржится»).
//!
//! Побочные эффекты (keyring, токен в транспорт) возвращаются из [`AppStore::apply`]
//! и исполняются слоем приложения — редьюсер остаётся детерминированным.

use std::collections::{BTreeMap, VecDeque};

use ld2_protocol::evt::{
    CharDeath, CharStats, CharXp, ChatMessage, CombatCooldown, CombatHit, CorpseCreated,
    CorpseLooted, CrimeAdded, CrimeRemoved, CrimeStatusUpdate, EntityDespawn, EntityHp, EntityMove,
    EntitySpawn, ErrorNotice, GuildLogEntry, ItemAdded, ItemDropped, ItemExpired,
    ItemRemovalReason, ItemRemoved, LocationChanged, MagicCast, PatchOp, PatchOpKind, StateDelta,
    StateSnapshot, TimeSync as TimeSyncEvent,
};
use ld2_protocol::types::{
    CharacterSummary, CrimeLogEntry, CrimeStatus, GuildState, Hp, LocationSnapshot, PartyState,
    QuestState,
};
use ld2_protocol::{ErrorCode, SessionConfig};

use crate::l10n::{t, Key, Lang};
use crate::transport::{IngressEvent, TransportStatus};

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

/// Ёмкость журнала мира (НФР §8: лог локации ≤ 10 000; M3 — 1000).
const LOG_LIMIT: usize = 1000;
/// Каналы чата из §5 (порядок вкладок UI).
pub const CHAT_CHANNELS: [&str; 6] = ["system", "location", "global", "party", "guild", "whisper"];
/// Ёмкость истории посещённых.
const VISITED_LIMIT: usize = 50;

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
    /// M23: данные окна подятия предмета (payload `entity.info`:
    /// лут трупа + блок «Внутри»); гейт рендера — `Root::pickup_popup`.
    pub pickup: Option<serde_json::Value>,
    /// M23: лут трупа обновился на сервере (разделка/новый труп) —
    /// клиент перечитает `pickup`, пока открыто окно подятия.
    pub pickup_dirty: bool,
    /// M24: каталог предметов админ-окна (вариант A — один запрос,
    /// кэш на сессию, фильтрация на клиенте).
    pub admin_catalog: Option<Vec<serde_json::Value>>,
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
    session_login: Option<String>,
}

impl Default for AppStore {
    fn default() -> Self {
        Self::new()
    }
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

/// M18/F37/F42: узел диалога — строка NPC + её видимые ответы (тексты уже
/// отрендерены сервером, `{name}` подставлен).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcNode {
    pub id: String,
    pub text: String,
    /// Квест, который предлагает узел (секция «Квесты» окна).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    #[serde(default)]
    pub replies: Vec<NpcReplyOption>,
}

impl NpcNode {
    /// M18/F42: абсолютные дедлайны кулдауна реплик — по ним UI ведёт
    /// живой отсчёт `(мм:сс)` (ADR-008). Вызывается при разборе payload.
    pub fn stamp_wait_deadlines(&mut self) {
        let now = local_epoch_ms();
        for reply in &mut self.replies {
            if let Some(wait_s) = reply.wait_s {
                reply.wait_until_ms = Some(now + (wait_s as i64) * 1000);
            }
        }
    }
}

/// M18/F37/F42: ответ персонажа, видимый в текущем узле (`id` — ключ
/// `cmd/npc.reply`).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcReplyOption {
    pub id: String,
    pub text: String,
    /// M18/F42: секунды до конца кулдауна целевой строки — ответ
    /// показывается неактивным с таймером (ADR-008); `None` — активен.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<u64>,
    /// M18/F42: абсолютный дедлайн кулдауна (epoch-мс) — ставится при
    /// разборе payload, живой отсчёт таймера в UI (wire не несёт).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_until_ms: Option<i64>,
    /// M18/F43: действие ответа (`"shop"`) — клиент открывает магазин
    /// вместо перехода по ветке; `None` — обычный ответ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// M18/F37: строка транскрипта окна диалога — лента NPC ↔ персонаж.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTranscriptLine {
    /// `true` — реплика персонажа (ответ).
    pub me: bool,
    pub text: String,
}

/// M18/F39: товар магазина в цене активной валютной вкладки.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopItem {
    pub def_id: String,
    pub name: String,
    #[serde(default)]
    pub rarity: String,
    /// Цена за партию в единицах валюты вкладки.
    pub price: i64,
    /// Размер партии (qty из контента, ≥1).
    #[serde(default = "shop_item_qty")]
    pub qty: i64,
    /// M18/F40: полный снапшот — карточка информации по клику строки.
    #[serde(default)]
    pub item: Option<ld2_protocol::types::ItemSnapshot>,
}

fn shop_item_qty() -> i64 {
    1
}

/// M18/F39: валютная вкладка магазина.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopTab {
    /// `def_id` валюты (для баланса и `cmd/shop.buy`).
    pub currency: String,
    pub currency_name: String,
    #[serde(default)]
    pub items: Vec<NpcShopItem>,
}

/// M18/F39: открытый магазин NPC (внутри окна диалога — очищается
/// вместе с `npc_dialog` на всех путях закрытия).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopView {
    #[serde(default)]
    pub tabs: Vec<NpcShopTab>,
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

/// M18/F35/F37/F42: открытое окно диалога NPC — payload ack
/// `cmd/npc.talk` `{dialog: {id, name}, node, quests, actions}`
/// (`greeting` убран — ADR-008); транскрипт накапливает ленту
/// (узлы ↔ ответы персонажа).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcDialogView {
    pub id: String,
    pub name: String,
    /// Текущий узел; `None` — диалог не начат/закончен.
    #[serde(default)]
    pub node: Option<NpcNode>,
    #[serde(default)]
    pub transcript: Vec<NpcTranscriptLine>,
    #[serde(default)]
    pub quests: Vec<ld2_protocol::types::QuestState>,
    /// M18/F38: контекстные кнопки статус-бара от сервера — только
    /// реально существующие системы (`quests`/`train`/`shop`).
    #[serde(default)]
    pub actions: Vec<String>,
    /// M18/F39: открытый магазин (по кнопке — `cmd/shop.list`).
    #[serde(default)]
    pub shop: Option<NpcShopView>,
    /// M18/F39: активная валютная вкладка магазина.
    #[serde(default)]
    pub shop_currency: Option<String>,
    /// M18/F44: обучение у этого NPC (ADR-009) — навыки с ценой.
    #[serde(default)]
    pub training: Vec<NpcTraining>,
    /// M21: профессии этого NPC с ценой (изучение — craft.learn).
    #[serde(default)]
    pub professions: Vec<NpcProfession>,
    /// M18/F44: открыта ли панель обучения (реплика `action=train`,
    /// паритет магазина); чистится вместе с окном диалога.
    #[serde(default)]
    pub training_open: bool,
    /// M22: открыта ли ИСТОРИЯ диалога (полная лента по кнопке в
    /// статусбаре окна); в самом диалоге — только текущая реплика.
    #[serde(default)]
    pub history_open: bool,
}

/// M18/F44: обучение навыку у NPC в диалоге (ADR-009) — повышение ранга
/// стоит очков опыта (пул) + платы из контента.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTraining {
    pub skill: String,
    /// Плата очками опыта (пул квестов).
    pub points: i64,
    /// Плата золотом (монеты); 0 — без золота.
    #[serde(default)]
    pub gold: i64,
    /// Плата предметами.
    #[serde(default)]
    pub items: Vec<NpcTrainingItem>,
}

/// M21: профессия, которой обучает NPC в диалоге — цена золотом
/// (`price_profession`); изучение — `cmd/craft.learn`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcProfession {
    pub id: String,
    /// Цена обучения золотом; старый сервер без поля →0.
    #[serde(default)]
    pub gold: i64,
}

/// M18/F44: предмет платы за обучение.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTrainingItem {
    pub def_id: String,
    #[serde(default = "shop_item_qty")]
    pub qty: i64,
}

impl NpcDialogView {
    /// Разбор payload ack (`None` — нет `dialog`; лента — корневой узел,
    /// `quests`/`actions` без поля → пусто; `greeting` больше нет —
    /// ADR-008).
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        let dialog = payload.get("dialog")?;
        let mut node = match payload.get("node") {
            Some(value) if !value.is_null() => {
                serde_json::from_value::<NpcNode>(value.clone()).ok()
            }
            _ => None,
        };
        // M18/F42: дедлайны для живого отсчёта таймеров реплик.
        node.as_mut().map(NpcNode::stamp_wait_deadlines);
        let mut transcript = Vec::new();
        if let Some(node) = &node {
            transcript.push(NpcTranscriptLine {
                me: false,
                text: node.text.clone(),
            });
        }
        Some(Self {
            id: dialog["id"].as_str()?.into(),
            name: dialog["name"].as_str()?.into(),
            node,
            transcript,
            quests: serde_json::from_value(payload.get("quests")?.clone()).unwrap_or_default(),
            actions: serde_json::from_value(payload.get("actions").cloned().unwrap_or_default())
                .unwrap_or_default(),
            shop: None,
            shop_currency: None,
            training: serde_json::from_value(payload.get("training").cloned().unwrap_or_default())
                .unwrap_or_default(),
            professions: serde_json::from_value(
                payload.get("professions").cloned().unwrap_or_default(),
            )
            .unwrap_or_default(),
            // M18/F44: панель обучения закрыта — открывает реплика.
            training_open: false,
            // M22: история закрыта — открывает кнопка в статусбаре.
            history_open: false,
        })
    }
}

/// M11: какой всплывающий диалог активен. Приоритет и гейт экрана — здесь
/// же их читает UI-диспетчер gpui-клиента (E2E-тест вехи M11.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    /// ФР-06: смерть — закрывается только серверным подтверждением.
    Death,
    Examine,
    Loot,
    PartyInvite,
    GuildInvite,
    Trade,
    /// M17/F30a: выбор оружия под инкрустацию руны/самоцвета.
    Inscribe,
    Create,
    Delete,
}

impl AppStore {
    /// Активный диалог (M11.2): экран персонажей — создание/удаление;
    /// игра — смерть > осмотр > лут > группа > гильдия > сделка >
    /// выбор цели инкрустации (M17/F30a); окно сокетов (инкрустация
    /// через меню оружия) — popup, вне диспетчера (M18); вне игры/
    /// персонажей — None. `create_open` и `delete_open` — флаги UI
    /// (Root), остальное — состояние store.
    ///
    /// M18/F53: описание предмета и окно заточки — попапы (вне
    /// диспетчера модалок, как окно персонажа и диалог NPC).
    pub fn active_dialog(&self, create_open: bool, delete_open: bool) -> Option<Dialog> {
        match self.screen {
            Screen::Characters => {
                if create_open {
                    Some(Dialog::Create)
                } else if delete_open {
                    Some(Dialog::Delete)
                } else {
                    None
                }
            }
            Screen::Game => {
                if self.death.is_some() {
                    Some(Dialog::Death)
                } else if self.entity_detail.is_some() {
                    Some(Dialog::Examine)
                } else if self.loot_confirm.is_some() {
                    Some(Dialog::Loot)
                } else if self.party_invite.is_some() {
                    Some(Dialog::PartyInvite)
                } else if self.guild_invite.is_some() {
                    Some(Dialog::GuildInvite)
                } else if self.trade.is_some() {
                    Some(Dialog::Trade)
                } else if self.inscribe.is_some() {
                    // M17/F30a: окно «Куда инкрустировать…».
                    Some(Dialog::Inscribe)
                } else {
                    // M18/F38: диалог NPC — попап (вне диспетчера модалок).
                    None
                }
            }
            _ => None,
        }
    }

    /// M11.7 (D20): отложить `char.delete` на `delay_s` секунд от `now`
    /// (замещает прежнее немедленное подтверждение удаления).
    pub fn schedule_delete(&mut self, char_id: String, name: String, now: i64, delay_s: i64) {
        self.pending_delete = Some(PendingDelete {
            char_id,
            name,
            deadline: now + delay_s,
        });
    }

    /// M11.7: отмена (вход в игру, логаут, явный отказ — D20).
    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// M11.7: срок истёк — `Some(char_id)`, состояние снимается (одноразово).
    pub fn tick_delete(&mut self, now: i64) -> Option<String> {
        let due = self
            .pending_delete
            .as_ref()
            .is_some_and(|p| now >= p.deadline);
        if due {
            self.pending_delete.take().map(|p| p.char_id)
        } else {
            None
        }
    }
}

impl AppStore {
    pub fn new() -> Self {
        Self {
            screen: Screen::Login,
            session: None,
            snapshot: None,
            transport: TransportStatus::Connecting,
            pending: None,
            login_error: None,
            register_error: None,
            create_error: None,
            select_error: None,
            world_error: None,
            death: None,
            item_detail: None,
            upgrade: None,
            sharpen_stone: None,
            sharpen_list: false,
            socketing: None,
            dialog: None,
            harvest: None,
            inscribe: None,
            item_cooldowns: BTreeMap::new(),
            time_offset: 0,
            time_samples: VecDeque::new(),
            lang: Lang::Ru,
            rune_recharge_phase_ms: None,
            target: None,
            combat_cooldowns: BTreeMap::new(),
            global_cooldown_until: 0,
            crime_log: Vec::new(),
            crime_log_loaded: false,
            loot_confirm: None,
            quest_tracker: Vec::new(),
            chat: std::collections::HashMap::new(),
            chat_channel: "location".into(),
            last_whisper_from: None,
            party: None,
            party_invite: None,
            trade: None,
            guild: None,
            guild_invite: None,
            guild_browse: Vec::new(),
            guild_log: Vec::new(),
            guild_browse_loaded: false,
            chat_limit: 5_000,
            rtt_ms: None,
            recipes: Vec::new(),
            recipes_loaded: false,
            notice: None,
            min_client_required: None,
            forced_update: false,
            last_login: None,
            log: VecDeque::new(),
            visited: Vec::new(),
            entity_detail: None,
            pickup: None,
            pickup_dirty: false,
            admin_catalog: None,
            admin_god: false,
            stun: None,
            admin_target_items: None,
            npc_dialog: None,
            pending_delete: None,
            deleting_char: None,
            session_login: None,
        }
    }

    // ── события транспорта (единственный вход состояния) ────────────────────

    /// Применение ingress-события. Возвращает эффекты для слоя приложения.
    pub fn apply(&mut self, ev: IngressEvent) -> Vec<Effect> {
        match ev {
            IngressEvent::Status(status) => self.apply_status(status),
            IngressEvent::Ack(ack) => self.apply_ack(ack),
            IngressEvent::Event(env) => self.apply_event(&env.msg_type, env.payload),
            IngressEvent::CommandTimeout { msg_type, .. } => {
                self.notice = Some(format!("нет ответа сервера: {msg_type}"));
                self.pending = None;
                vec![]
            }
            IngressEvent::CommandFailed { msg_type, .. } => {
                self.notice = Some(format!("связь потеряна: {msg_type}"));
                self.pending = None;
                vec![]
            }
            IngressEvent::Rtt(ms) => {
                // НФР §8: RTT-индикатор в header.
                self.rtt_ms = Some(ms);
                vec![]
            }
        }
    }

    fn apply_status(&mut self, status: TransportStatus) -> Vec<Effect> {
        let auth_expired = matches!(status, TransportStatus::AuthExpired);
        let stopped = matches!(status, TransportStatus::Stopped);
        // M19 (§15): discovery-отказ — понятная причина без подключения
        // (текст по ключу; ретраи транспорта продолжаются).
        if let TransportStatus::ResolveFailed(code) = &status {
            use crate::l10n::{t, Key, Lang};
            let key = match code {
                crate::transport::ResolveFailCode::Unreachable => Key::DiscUnreachable,
                crate::transport::ResolveFailCode::BadDoc => Key::DiscBadDoc,
                crate::transport::ResolveFailCode::Contract => Key::DiscContract,
                crate::transport::ResolveFailCode::MinClient => Key::DiscMinClient,
                crate::transport::ResolveFailCode::Insecure => Key::DiscInsecure,
            };
            self.notice = Some(t(Lang::Ru, key).to_string());
            self.transport = status;
            return vec![];
        }
        self.transport = status;
        if auth_expired {
            // §4.5: access/refresh истёк → экран входа, токен из keyring.
            return self.reset_to_login(Some("Сессия истекла — войдите снова".into()));
        }
        if stopped && self.screen != Screen::Login {
            self.notice = Some("Соединение с сервером потеряно".into());
            return self.reset_to_login(None);
        }
        vec![]
    }

    fn apply_ack(&mut self, ack: ld2_protocol::Ack) -> Vec<Effect> {
        // E_AUTH_EXPIRED важнее локальной корреляции (§4.5).
        if ack.error_code() == Some(ErrorCode::AuthExpired) {
            let msg = ack
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "Сессия истекла".into());
            return self.reset_to_login(Some(msg));
        }

        let Some(act) = self.pending.take() else {
            // M18/F41: ack без корреляции (гонка команд) молча теряется —
            // в debug-логе видно, чей ack пришёл не вовремя.
            tracing::debug!(
                "ack {} (seq={}) без pending — пропущен",
                ack.msg_type,
                ack.seq
            );
            return vec![];
        };

        if !ack.is_ok() {
            let msg = ack
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| format!("{:?}", ack.error_code()));
            match act {
                Act::Login => self.login_error = Some(msg),
                Act::Register => {
                    // D35: код §9 → поле формы; текст — серверный (fallback §9).
                    let code = ack
                        .error_code()
                        .unwrap_or(ld2_protocol::ErrorCode::Internal);
                    self.register_error = Some(crate::register::RegisterError {
                        field: crate::register::register_field_for(&code),
                        message: msg,
                        retry_after_s: ack.error.as_ref().and_then(|e| e.retry_after_s),
                    });
                }
                Act::Create => self.create_error = Some(msg),
                Act::Select => self.select_error = Some(msg),
                Act::Delete => {
                    self.deleting_char = None;
                    self.notice = Some(msg);
                }
                Act::Move
                | Act::Attack
                | Act::Examine
                | Act::PickupInfo
                | Act::NpcTalk
                | Act::NpcReply
                | Act::ShopList
                | Act::ShopBuy => self.world_error = Some(msg),
                // M16/F29h: ошибки листа — системные уведомления
                // (тосты), как и весь прочий фидбек.
                Act::Invest
                | Act::Equip
                | Act::Unequip
                | Act::Drop
                | Act::Sharpen
                | Act::Preview
                | Act::Upgrade
                | Act::Socket
                | Act::CrimeLog
                | Act::Learn
                | Act::Craft
                | Act::Quest => self.notice = Some(msg),
                Act::Cast => self.world_error = Some(msg),
                // Лимиты чата (E_RATE_LIMITED §9) и права (E_PERMISSION,
                // M14/F19) — тостом.
                Act::Chat | Act::ChatDelete => self.notice = Some(msg),
                // Соцдомен: ошибки (E_TRADE_STALE, E_PARTY_FULL §9) — тостом.
                Act::Party | Act::Trade | Act::GuildBrowse | Act::Guild => self.notice = Some(msg),
                // Игровые диалоги (M20, заготовка) — ошибки тостом.
                Act::DialogChoice | Act::DialogSubmit => self.notice = Some(msg),
                // Добыча (M20/F20b): отказы — в мир (как Examine/Move).
                Act::Gather => self.world_error = Some(msg),
                // Профессии (M21): ранг/сброс — тосты (у тренера/граница).
                Act::CraftRankUp | Act::ProfessionReset => self.notice = Some(msg),
                // Админ-панель (M24): отказы — тост (каталог/ask/god/цель).
                Act::AdminCatalog
                | Act::AdminAsk
                | Act::AdminGod
                | Act::AdminTarget
                | Act::AdminClear => {
                    self.notice = Some(msg);
                }
                // Снятие шкур (M22): отказ до старта — чистим бар.
                Act::Harvest => {
                    self.harvest = None;
                    self.world_error = Some(msg);
                }
            }
            return vec![];
        }

        match act {
            // Успех логина подтверждён ack'ом; дальше ждём evt/session.start.
            Act::Login => {}
            // Регистрация = немедленный вход (ADR-006): ack ok → тоже
            // ждём evt/session.start; ошибки — в register_error (M13).
            Act::Register => {
                self.register_error = None;
            }
            Act::Select => {}
            Act::Delete => {
                // M11.7: ack — карточка уходит из списка (ФР-02).
                if let Some(id) = self.deleting_char.take() {
                    if let Some(session) = self.session.as_mut() {
                        session.characters.retain(|c| c.id != id);
                    }
                }
                self.notice = Some("Персонаж удалён".into());
            }
            Act::Invest => {
                // §7: ack несёт остаток пула.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Some(pool) = payload["pool_left"].as_i64() {
                        snap.char.xp_pool = pool;
                    }
                }
            }
            Act::Equip | Act::Unequip => {
                // §7: ack несёт обновлённый `char` — события на смену экипировки нет.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(ch) = serde_json::from_value::<ld2_protocol::types::CharacterState>(
                        payload["char"].clone(),
                    ) {
                        snap.char = ch;
                    }
                }
            }
            Act::Move => {
                // ack несёт локацию (§7): применяем сразу, событие дойдёт позже.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(location) =
                        serde_json::from_value::<LocationSnapshot>(payload["location"].clone())
                    {
                        snap.location = location;
                    }
                }
            }
            Act::Attack
            | Act::Examine
            | Act::PickupInfo
            | Act::NpcTalk
            | Act::NpcReply
            | Act::ShopList
            | Act::ShopBuy
            | Act::Drop
            | Act::Upgrade
            | Act::Cast
            | Act::Quest
            | Act::Chat
            | Act::ChatDelete
            | Act::Trade
            | Act::DialogChoice
            | Act::DialogSubmit
            | Act::Gather
            | Act::Harvest
            | Act::AdminAsk
            | Act::AdminGod
            | Act::AdminCatalog
            | Act::AdminTarget
            | Act::AdminClear => {
                // результат событиями (§8); npc.talk/npc.reply
                // обрабатываются ниже отдельными блоками (M18/F35/F37);
                // диалоги (M20) — ack только подтверждает выбор/ввод.
            }
            Act::Party => {
                // §7: party.create/accept несут {party}.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(party) =
                        serde_json::from_value::<PartyState>(payload["party"].clone())
                    {
                        self.party = Some(party);
                    }
                }
            }
            Act::GuildBrowse => {
                // §7: guild.browse → {guilds[]} (идемпотентна, §7).
                if let Some(payload) = ack.payload.as_ref() {
                    if let Some(list) = payload["guilds"].as_array() {
                        self.guild_browse = list.clone();
                    }
                }
            }
            Act::Guild => {
                // §7: guild.create/join несут {guild}.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(guild) =
                        serde_json::from_value::<GuildState>(payload["guild"].clone())
                    {
                        self.guild = Some(guild);
                    }
                }
            }
            Act::ProfessionReset => {
                // M21: после сброса — убрать профессию из снапшота
                // (слот основной освобождается).
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Some(id) = payload["profession_id"].as_str() {
                        snap.char.professions.retain(|p| p.id != id);
                    }
                }
            }
            Act::Craft => {
                // §7: ack несёт список рецептов.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Some(recipes) = payload["recipes"].as_array() {
                        self.recipes = recipes.clone();
                    }
                }
                self.recipes_loaded = true;
            }
            Act::CrimeLog => {
                // §7: ack несёт журнал нарушений целиком.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(entries) =
                        serde_json::from_value::<Vec<CrimeLogEntry>>(payload["entries"].clone())
                    {
                        self.crime_log = entries;
                        self.crime_log_loaded = true;
                    }
                }
            }
            Act::Learn | Act::CraftRankUp => {
                // §7: ack {profession} — добавляем/обновляем в снапшоте.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(prof) = serde_json::from_value::<
                        ld2_protocol::types::ProfessionSnapshot,
                    >(payload["profession"].clone())
                    {
                        if let Some(existing) =
                            snap.char.professions.iter_mut().find(|p| p.id == prof.id)
                        {
                            *existing = prof;
                        } else {
                            snap.char.professions.push(prof);
                        }
                    }
                }
            }
            Act::Sharpen => {
                // M18/F54: сервер выбрал камень — запоминаем для окна
                // заточки (имя берём из снапшота, качество — из ack).
                if let Some(payload) = ack.payload.as_ref() {
                    // Типизированный ack (ld2-protocol, golden §7) —
                    // старая/чужая форма молча игнорируется.
                    let Ok(sel) =
                        serde_json::from_value::<ld2_protocol::StoneSelected>(payload.clone())
                    else {
                        return vec![];
                    };
                    let name = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| {
                            s.char
                                .inventory
                                .iter()
                                .find(|i| i.uid == sel.stone_uid)
                                .map(|i| i.name.clone())
                        })
                        .unwrap_or_else(|| sel.stone_uid.clone());
                    self.sharpen_stone = Some(SharpenStone {
                        uid: sel.stone_uid,
                        name,
                        quality: sel.quality,
                    });
                    // M18/F55: окно открыто — гейт рендера (список).
                    self.sharpen_list = true;
                    // Свежий выбор камня — прошлый результат неактуален.
                    if let Some(upgrade) = self.upgrade.as_mut() {
                        upgrade.result = None;
                        upgrade.preview = None;
                    }
                }
            }
            Act::Preview => {
                if let (Some(upgrade), Some(payload)) =
                    (self.upgrade.as_mut(), ack.payload.as_ref())
                {
                    upgrade.preview = Some(payload.clone());
                    // M18/F55: итог прошлого броска НЕ затираем — он
                    // должен остаться на экране заточки (зелёным/красным)
                    // даже после автопревью. Сбрасывается при открытии
                    // предмета (`open_upgrade`), при смене камня и при
                    // старте следующего улучшения.
                }
            }
            Act::Socket => {
                // §7: ack извлекаемого несёт {staff, rune}, вставки — {staff};
                // M18/F32: цель надета — ack дополнен `char` (бонусы
                // самоцвета/урон руны вступили в силу сразу).
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(ch) = serde_json::from_value::<ld2_protocol::types::CharacterState>(
                        payload["char"].clone(),
                    ) {
                        snap.char = ch;
                    } else {
                        apply_item_patch(&mut snap.char, payload);
                    }
                }
                // M24 фикс: инкрустация стака — сервер разбил стак и
                // прислал НОВЫЙ uid целевой штуки; окно следует за
                // целью (иначе следующая вставка снова шла бы в стак
                // и каждый сокет попадал бы на свою штуку).
                if self.socketing.is_some() {
                    if let Some(staff_uid) = ack
                        .payload
                        .as_ref()
                        .and_then(|p| p["staff"]["uid"].as_str())
                    {
                        self.socketing = Some(staff_uid.to_string());
                    }
                }
            }
            Act::Create => {
                // Аддитивное подтверждение: дополняем список персонажей (§7).
                if let (Some(session), Some(payload)) =
                    (self.session.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(character) =
                        serde_json::from_value::<CharacterSummary>(payload["character"].clone())
                    {
                        if !session.characters.iter().any(|c| c.id == character.id) {
                            session.characters.push(character);
                        }
                    }
                }
                self.notice = Some("Персонаж создан".into());
            }
        }

        // M18/F39: магазин — ack `{shop}` → панель внутри окна диалога.
        if act == Act::ShopList {
            if let (Some(dialog), Some(payload)) = (self.npc_dialog.as_mut(), ack.payload.as_ref())
            {
                if let Ok(shop) = serde_json::from_value::<NpcShopView>(payload["shop"].clone()) {
                    dialog.shop_currency = shop.tabs.first().map(|tab| tab.currency.clone());
                    dialog.shop = Some(shop);
                }
            }
        }
        // M18/F35: диалог NPC — ack `{dialog, node, quests}` → окно (до
        // Examine: там payload перемещается).
        if act == Act::NpcTalk {
            if let Some(payload) = ack.payload.as_ref() {
                self.npc_dialog = NpcDialogView::from_payload(payload);
                // M18/F41: диагностика повторного открытия диалога.
                tracing::debug!(
                    "ack npc.talk (seq={}) → открыт={}, узел={:?}, строк ленты={}",
                    ack.seq,
                    self.npc_dialog.is_some(),
                    self.npc_dialog
                        .as_ref()
                        .and_then(|d| d.node.as_ref().map(|n| n.id.clone())),
                    self.npc_dialog.as_ref().map_or(0, |d| d.transcript.len()),
                );
            } else {
                tracing::debug!("ack npc.talk (seq={}) без payload", ack.seq);
            }
        }
        // M18/F37: ответ персонажа — новый узел (`node: null` — конец);
        // список квестов освежается (приняли/сдали между открытиями).
        if act == Act::NpcReply {
            if let Some(payload) = ack.payload.as_ref() {
                let terminal = payload.get("node").is_none_or(|v| v.is_null());
                if terminal {
                    // M18/F38: конец диалога («Пока») — окно закрывается.
                    self.npc_dialog = None;
                } else if let Some(dialog) = self.npc_dialog.as_mut() {
                    if let Ok(mut node) = serde_json::from_value::<NpcNode>(payload["node"].clone())
                    {
                        // M18/F42: дедлайны для живого отсчёта таймеров.
                        node.stamp_wait_deadlines();
                        dialog.transcript.push(NpcTranscriptLine {
                            me: false,
                            text: node.text.clone(),
                        });
                        dialog.node = Some(node);
                        if let Ok(quests) = serde_json::from_value::<
                            Vec<ld2_protocol::types::QuestState>,
                        >(payload["quests"].clone())
                        {
                            dialog.quests = quests;
                        }
                    }
                }
            }
        }
        // Осмотр: payload ack'а — детали сущности (включая лут трупа, ФР-03/ФР-11).
        if act == Act::Examine {
            self.entity_detail = ack.payload;
        } else if act == Act::PickupInfo {
            // M23: окно подятия — тот же `entity.info`, но в отдельное
            // поле (окно не перетирает модалку осмотра и наоборот).
            self.pickup = ack.payload;
        } else if act == Act::AdminCatalog {
            // M24: каталог админ-окна — `{items:[…]}` → массив.
            self.admin_catalog = ack
                .payload
                .and_then(|p| p.get("items").cloned())
                .and_then(|v| v.as_array().cloned());
        } else if act == Act::AdminGod {
            // M24: эхо состояния тумблера (админ мог переключить
            // в другом окне — сервер истина).
            if let Some(enabled) = ack.payload.as_ref().and_then(|p| p["enabled"].as_bool()) {
                self.admin_god = enabled;
            }
        } else if act == Act::AdminTarget {
            // M24: содержимое цели правой колонки — `{items:[…]}`.
            self.admin_target_items = ack
                .payload
                .and_then(|p| p.get("items").cloned())
                .and_then(|v| v.as_array().cloned());
        }
        vec![]
    }

    fn apply_event(&mut self, msg_type: &str, payload: Option<serde_json::Value>) -> Vec<Effect> {
        let Some(payload) = payload else {
            return vec![];
        };
        match msg_type {
            "evt/time.sync" => {
                // §1.1: server = local + offset — база всех обратных отсчётов.
                let Ok(sync) = serde_json::from_value::<TimeSyncEvent>(payload) else {
                    return vec![];
                };
                let local = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // R16 (§1.1 «КРИЧНО»): медиана последних 5 замеров;
                // перекалибровка — периодический cmd/time.sync (root).
                self.time_samples.push_back(sync.server_ts - local);
                if self.time_samples.len() > 5 {
                    self.time_samples.pop_front();
                }
                let mut sorted: Vec<i64> = self.time_samples.iter().copied().collect();
                sorted.sort_unstable();
                self.time_offset = sorted[sorted.len() / 2];
                vec![]
            }
            "evt/combat/cooldown" => {
                // ФР-05: блокировка действия до until_epoch.
                let Ok(cd) = serde_json::from_value::<CombatCooldown>(payload) else {
                    return vec![];
                };
                if let Some(skill) = cd.skill_id {
                    // M17/F30g: предметные кулдауны — с полной
                    // длительностью для прогресс-бара кнопки.
                    if skill.starts_with("item:") {
                        if let Some(total) = cd.duration_ms {
                            self.item_cooldowns
                                .insert(skill, (cd.until_epoch * 1000, total));
                            return vec![];
                        }
                    }
                    self.combat_cooldowns.insert(skill, cd.until_epoch);
                }
                vec![]
            }
            "evt/session.start" => {
                let Ok(cfg) = serde_json::from_value::<SessionConfig>(payload) else {
                    tracing::warn!("некорректный evt/session.start — игнор");
                    // R21: login-ack уже ok — без notice UI висит молча.
                    self.notice = Some(
                        "⚠ Сервер прислал некорректную конфигурацию сессии — переподключитесь"
                            .into(),
                    );
                    return vec![];
                };
                let refresh = cfg.session.refresh.token.clone();
                // НФР «Обновления»: force-min-version capability сервера (§12).
                let required = cfg.server.min_client.clone();
                let login = self
                    .session_login
                    .clone()
                    .unwrap_or_else(|| self.last_login.clone().unwrap_or_default());
                let was_login_screen = self.screen == Screen::Login;
                self.session = Some(cfg);
                self.login_error = None;
                if was_login_screen {
                    // Новый вход — на выбор персонажей; при реконнекте из
                    // игры экран НЕ меняем (§4.5: session.start повторяется).
                    self.screen = Screen::Characters;
                    self.notice = None;
                }
                // После сброса notice: предупреждение о версии не должно
                // затираться переходом экрана (НФР force-min-version).
                // R3: не только notice — жёсткий флаг блокирует вход в
                // мир (chars.rs не отправляет char.select).
                let forced = required
                    .as_deref()
                    .map(|r| crate::update_check::is_newer(r, env!("CARGO_PKG_VERSION")))
                    .unwrap_or(false);
                self.min_client_required = required.clone();
                self.forced_update = forced;
                if let Some(required) = required {
                    if forced {
                        self.notice = Some(format!(
                            "⚠ Сервер требует клиент v{required} (у вас v{}) — обновитесь",
                            env!("CARGO_PKG_VERSION")
                        ));
                    }
                }
                vec![
                    Effect::PersistSession {
                        login,
                        refresh_token: refresh.clone(),
                    },
                    Effect::TransportToken(refresh),
                ]
            }
            "evt/state/snapshot" => {
                // M24: снапшот — новая фаза такта зарядов (серверную
                // фазу клиент не знает; по патчу тикера самокоррекция).
                self.rune_recharge_phase_ms = Some(local_epoch_ms() + self.time_offset * 1000);
                let Ok(snap) = serde_json::from_value::<StateSnapshot>(payload) else {
                    tracing::warn!("некорректный evt/state/snapshot — игнор");
                    // R21: экран мира без снапшота — молчаливое «пустое
                    // окно»; сообщаем пользователю.
                    self.notice = Some(
                        "⚠ Сервер прислал некорректный снапшот мира — переподключитесь".into(),
                    );
                    return vec![];
                };
                self.push_visited(&snap.location);
                // M14/F18: лог мира персональный — очистка при входе в игру
                // (старые строки сессии не переживают вход; строка «Вход в мир»
                // остаётся первой).
                self.log.clear();
                self.push_log(
                    LogKind::Location,
                    format!("Вход в мир: {}", snap.location.name),
                );
                // §7: снапшот несёт межигровое состояние (пати/гильдия).
                self.party = snap.party.clone();
                self.guild = snap.guild.clone();
                self.snapshot = Some(snap);
                self.screen = Screen::Game;
                self.select_error = None;
                self.world_error = None;
                self.entity_detail = None;
                self.pickup = None;
                self.notice = None;
                vec![]
            }
            // ── события мира (M3: ФР-03, ФР-17) ─────────────────────────────
            "evt/location/changed" => {
                let Ok(changed) = serde_json::from_value::<LocationChanged>(payload) else {
                    return vec![];
                };
                let name = changed.location.name.clone();
                let pvp = changed.location.flags.iter().any(|f| f == "pvp");
                let active: Vec<String> = {
                    let now = local_epoch() + self.time_offset;
                    self.snapshot
                        .as_ref()
                        .map(|s| {
                            filter_active_statuses(&s.char.crime.statuses, now)
                                .iter()
                                .map(|s| s.kind.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                };
                // Смена локации — новый контекст: старые тосты устарели.
                self.notice = None;
                if pvp && !active.is_empty() {
                    // ФР-11: предупреждение при входе в PvP-зону со статусом.
                    self.notice =
                        Some(format!("⚠ PvP-зона: активны статусы {}", active.join(", ")));
                }
                self.push_visited(&changed.location);
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.location = changed.location;
                }
                self.entity_detail = None;
                self.pickup = None;
                self.world_error = None;
                self.push_log(LogKind::Location, format!("Вы вошли в {name}"));
                vec![]
            }
            "evt/entity/spawn" => {
                let Ok(spawn) = serde_json::from_value::<EntitySpawn>(payload) else {
                    return vec![];
                };
                let mut added = None;
                if let Some(snap) = self.snapshot.as_mut() {
                    if snap.location.id == spawn.location_id
                        && !snap
                            .location
                            .entities
                            .iter()
                            .any(|e| e.id == spawn.entity.id)
                    {
                        added = Some(spawn.entity.name.clone());
                        snap.location.entities.push(spawn.entity);
                    }
                }
                if let Some(name) = added {
                    self.push_log(LogKind::System, format!("{name} появляется"));
                }
                vec![]
            }
            "evt/entity/move" => {
                let Ok(moved) = serde_json::from_value::<EntityMove>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == moved.entity_id)
                    {
                        if let Some(hp) = moved.patch.get("hp") {
                            if let Ok(hp) = serde_json::from_value::<Hp>(hp.clone()) {
                                entity.hp = Some(hp);
                            }
                        }
                    }
                }
                vec![]
            }
            "evt/entity/hp" => {
                let Ok(update) = serde_json::from_value::<EntityHp>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == update.entity_id)
                    {
                        entity.hp = Some(update.hp);
                    }
                }
                vec![]
            }
            "evt/entity/despawn" => {
                let Ok(gone) = serde_json::from_value::<EntityDespawn>(payload) else {
                    return vec![];
                };
                let mut name = None;
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(idx) = snap
                        .location
                        .entities
                        .iter()
                        .position(|e| e.id == gone.entity_id)
                    {
                        name = Some(snap.location.entities.remove(idx).name);
                    }
                }
                if let Some(name) = name {
                    self.push_log(LogKind::System, format!("{name} исчезает"));
                }
                // ФР-05: авто-сброс цели при исчезновении.
                if self.target.as_deref() == Some(gone.entity_id.as_str()) {
                    self.target = None;
                }
                vec![]
            }
            "evt/combat/hit" => {
                let Ok(hit) = serde_json::from_value::<CombatHit>(payload) else {
                    return vec![];
                };
                let mine = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.char.id == hit.source);
                self.push_log(
                    if mine {
                        LogKind::CombatOut
                    } else {
                        LogKind::CombatIn
                    },
                    hit.text,
                );
                vec![]
            }
            // M25: моб атаковал — бар атаки (следующая атака/интервал).
            "evt/mob/attack" => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobAttack>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        entity.next_attack_at = Some(ev.next_attack_at);
                        entity.attack_interval = Some(ev.interval_ms);
                    }
                }
                vec![]
            }
            // M25: босс сменил фазу — глиф/имя в списке + журнал.
            "evt/mob/phase" => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobPhase>(payload) else {
                    return vec![];
                };
                let mut mob_name = ev.entity_id.clone();
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        mob_name = entity.name.clone();
                        let casting = entity.boss.as_ref().and_then(|b| b.casting.clone());
                        entity.boss = Some(ld2_protocol::types::BossState {
                            phase: ev.phase,
                            name: ev.name.clone(),
                            glyph: ev.glyph.clone(),
                            casting,
                        });
                    }
                }
                self.push_log(
                    LogKind::System,
                    format!("{mob_name} входит в фазу: {}", ev.name),
                );
                vec![]
            }
            // M25: босс использовал навык — журнал.
            // M25: босс начал каст — журнал и «готовит …» в списке.
            "evt/mob/cast" => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobCast>(payload) else {
                    return vec![];
                };
                let mut boss_name = ev.entity_id.clone();
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        boss_name = entity.name.clone();
                        if let Some(boss) = entity.boss.as_mut() {
                            boss.casting = Some(ev.name.clone());
                        }
                    }
                }
                self.push_log(
                    LogKind::CombatIn,
                    format!("{boss_name} готовит: {}", ev.name),
                );
                vec![]
            }
            // M25: босс использовал навык — журнал, каст завершён.
            "evt/mob/skill" => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::MobSkill>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(entity) = snap
                        .location
                        .entities
                        .iter_mut()
                        .find(|e| e.id == ev.entity_id)
                    {
                        if let Some(boss) = entity.boss.as_mut() {
                            boss.casting = None;
                        }
                    }
                }
                self.push_log(
                    LogKind::CombatIn,
                    format!("{} использует {}", ev.source_name, ev.name),
                );
                vec![]
            }
            "evt/corpse/created" => {
                let Ok(corpse) = serde_json::from_value::<CorpseCreated>(payload) else {
                    return vec![];
                };
                // M23: лут/флаги трупа обновились — освежить окно
                // подятия, если оно открыто.
                self.pickup_dirty = true;
                let mut added = false;
                if let Some(snap) = self.snapshot.as_mut() {
                    match snap
                        .location
                        .entities
                        .iter()
                        .position(|e| e.id == corpse.entity.id)
                    {
                        // Обновление (M22: лут/флаги разделки) — замена.
                        Some(pos) => snap.location.entities[pos] = corpse.entity,
                        None => {
                            added = true;
                            snap.location.entities.push(corpse.entity);
                        }
                    }
                }
                if added {
                    self.push_log(LogKind::Loot, format!("Труп — лута: {}", corpse.loot_count));
                }
                vec![]
            }
            "evt/corpse/looted" => {
                let Ok(looted) = serde_json::from_value::<CorpseLooted>(payload) else {
                    return vec![];
                };
                if looted.was_player {
                    // ФР-11: лут с трупа игрока — статус «Мародёр».
                    self.push_log(
                        LogKind::Crime,
                        format!(
                            "{} собирает лут с трупа ИГРОКА — статус «Мародёр»",
                            looted.by_name
                        ),
                    );
                } else {
                    self.push_log(LogKind::Loot, format!("{} собирает лут", looted.by_name));
                }
                vec![]
            }
            "evt/char/level_up" => {
                // M22: новый уровень — журнал: себе «Вы получили…»,
                // остальным в локации «{имя} получил(а)…».
                let Ok(up) = serde_json::from_value::<ld2_protocol::evt::CharLevelUp>(payload)
                else {
                    return vec![];
                };
                let mine = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.char.id == up.char_id);
                if mine {
                    self.push_log(
                        LogKind::Xp,
                        format!("Вы получили новый уровень! Уровень персонажа: {}", up.level),
                    );
                } else {
                    let verb = if up.gender == "female" {
                        "получила"
                    } else {
                        "получил"
                    };
                    self.push_log(LogKind::Xp, format!("{} {} новый уровень!", up.name, verb));
                }
                vec![]
            }
            "evt/char/xp" => {
                let Ok(xp) = serde_json::from_value::<CharXp>(payload) else {
                    return vec![];
                };
                // M22: опыт персонажа — отдельная строка в журнале.
                if xp.amount > 0 {
                    self.push_log(LogKind::Xp, format!("Опыт +{}", xp.amount));
                }
                // Обновляем лист: навык и остаток пула (ФР-04).
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(pool) = xp.pool_left {
                        snap.char.xp_pool = pool;
                    }
                    if let Some(skill) = snap.char.skills.iter_mut().find(|s| s.id == xp.skill_id) {
                        skill.xp = xp.skill_xp;
                        if let Some(rank) = xp.rank {
                            skill.rank = rank;
                        }
                        if let Some(next) = xp.xp_to_next {
                            skill.xp_to_next = next;
                        }
                    }
                }
                let text = if xp.amount < 0 {
                    // M18/F44: повышение ранга у тренера — очки пула
                    // списаны (amount отрицательный), шкала обнулена.
                    match xp.rank {
                        Some(rank) => {
                            format!("{} — ранг {rank}! (−{} очков)", xp.skill_id, -xp.amount)
                        }
                        None => format!("{} (−{} очков)", xp.skill_id, -xp.amount),
                    }
                } else {
                    match xp.rank {
                        Some(rank) => format!("+{} XP → {} — ранг {rank}!", xp.amount, xp.skill_id),
                        None => format!("+{} XP → {}", xp.amount, xp.skill_id),
                    }
                };
                self.push_log(LogKind::Xp, text);
                vec![]
            }
            "evt/char/stats" => {
                // Патч ресурсов/атрибутов (respawn, зелья) — ФР-06/ФР-07.
                let Ok(stats) = serde_json::from_value::<CharStats>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(hp) = stats.patch.get("hp") {
                        if let Ok(hp) = serde_json::from_value::<Hp>(hp.clone()) {
                            snap.char.hp = hp;
                        }
                    }
                    if let Some(mp) = stats.patch.get("mp") {
                        if let Ok(mp) = serde_json::from_value::<Hp>(mp.clone()) {
                            snap.char.mp = mp;
                        }
                    }
                    if let Some(attrs) = stats.patch.get("attrs").and_then(|v| v.as_object()) {
                        for (key, value) in attrs {
                            if let Some(n) = value.as_i64() {
                                snap.char.attrs.insert(key.clone(), n);
                            }
                        }
                    }
                }
                vec![]
            }
            // M25: дебафф контроля — стан открывает окно с
            // прогрессбаром, остальные эффекты — строка журнала.
            "evt/char/afflict" => {
                let Ok(ev) = serde_json::from_value::<ld2_protocol::evt::CharAfflict>(payload)
                else {
                    return vec![];
                };
                if ev.kind == "stun" {
                    self.stun = Some(StunView {
                        title: ev.title,
                        until_ms: ev.until_ms,
                        started_ms: local_epoch_ms(),
                    });
                } else {
                    let secs = ((ev.until_ms - local_epoch_ms()).max(0) + 999) / 1000;
                    self.push_log(LogKind::CombatIn, format!("{} — {} с", ev.title, secs));
                }
                vec![]
            }
            "evt/char/death" => {
                // ФР-06: сводка утраченного — модалка, закрывается подтверждением.
                // M18: в журнал НЕ выводим — утраченное видно в самой
                // модалке (полный список, со скроллом).
                let Ok(death) = serde_json::from_value::<CharDeath>(payload) else {
                    return vec![];
                };
                self.death = Some(death);
                self.target = None;
                vec![Effect::Notify(NotifyKind::Death)]
            }
            "evt/magic/cast" => {
                let Ok(cast) = serde_json::from_value::<MagicCast>(payload) else {
                    return vec![];
                };
                // D4: свой каст тратит ману — бар обновляем по mp_left (§8);
                // заряды приходят патчем evt/item/state.
                if cast.ok {
                    if let Some(mp_left) = cast.mp_left {
                        if let Some(snap) = self.snapshot.as_mut() {
                            if snap.char.id == cast.by {
                                snap.char.mp.cur = mp_left as i64;
                            }
                        }
                    }
                }
                let text = if cast.ok {
                    format!("{}: {} ({:?})", cast.by_name, cast.spell, cast.what)
                } else {
                    format!("{}: {} — сбой", cast.by_name, cast.spell)
                };
                self.push_log(LogKind::Magic, text);
                vec![]
            }
            "evt/item/added" => {
                let Ok(added) = serde_json::from_value::<ItemAdded>(payload) else {
                    return vec![];
                };
                // Лут приходит сервером уже добавленным — синхронизируем снапшот.
                if let Some(snap) = self.snapshot.as_mut() {
                    if !snap.char.inventory.iter().any(|i| i.uid == added.item.uid) {
                        snap.char.inventory.push(added.item.clone());
                    }
                }
                self.push_log(LogKind::Loot, format!("Получено: {}", added.item.name));
                vec![]
            }
            "evt/item/removed" => {
                let Ok(removed) = serde_json::from_value::<ItemRemoved>(payload) else {
                    return vec![];
                };
                // Название — из инвентаря ДО удаления (заказчик: в
                // журнале имя предмета, не id); надетые тоже в
                // инвентаре — один lookup покрывает оба случая.
                let removed_name = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.char.inventory.iter().find(|i| i.uid == removed.uid))
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| removed.uid.clone());
                // ФР-06/ФР-07: убыль из инвентаря и из слотов экипировки.
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.inventory.retain(|i| i.uid != removed.uid);
                    snap.char
                        .equip
                        .retain(|_, slot| slot.as_deref() != Some(removed.uid.as_str()));
                }
                // M18/F54: активный камень выбыл — снимаем выбор и
                // гасим превью: оно было посчитано с этим камнем, повторный
                // старт без нового камня (`E_UPGRADE_PREVIEW_STALE`/`E_…`)
                // был бы неверным шагом.
                if self
                    .sharpen_stone
                    .as_ref()
                    .is_some_and(|st| st.uid == removed.uid)
                {
                    self.sharpen_stone = None;
                    if let Some(view) = self.upgrade.as_mut() {
                        view.preview = None;
                    }
                }
                // M20: при выбросе строку даёт `evt/item/dropped`
                // («Вы выбросили…»), а не «Потеряно».
                if !matches!(removed.reason, ItemRemovalReason::Dropped) {
                    self.push_log(LogKind::Loot, format!("Потеряно: {removed_name}"));
                }
                vec![]
            }
            "evt/item/dropped" => {
                // M20: выброс предмета — журнал всем в локации; свой
                // char_id → «Вы выбросили…», иначе по полу «выбросил(а)».
                let Ok(dropped) = serde_json::from_value::<ItemDropped>(payload) else {
                    return vec![];
                };
                // Заказчик 2026-11-21: для ВСЕХ игроков рядом — единый
                // формат «{имя} выбросил(а) {qty} {предмет}» (и для себя).
                let qty = dropped.qty;
                let text = if dropped.gender == "female" {
                    format!("{} выбросила {qty} {}", dropped.by_name, dropped.item_name)
                } else {
                    format!("{} выбросил {qty} {}", dropped.by_name, dropped.item_name)
                };
                self.push_log(LogKind::Loot, text);
                vec![]
            }
            "evt/item/expired" => {
                // M20: предмет в локации исчез по TTL.
                let Ok(expired) = serde_json::from_value::<ItemExpired>(payload) else {
                    return vec![];
                };
                self.push_log(LogKind::Loot, format!("{} исчез", expired.name));
                vec![]
            }
            "evt/harvest/result" => {
                // Снятие шкур (M22): чистим бар, пишем в журнал.
                let Ok(result) =
                    serde_json::from_value::<ld2_protocol::evt::HarvestResult>(payload)
                else {
                    return vec![];
                };
                // M24: бар гаснет только по итогу СВОЕГО трупа —
                // при переключении итог прерванного действия чужого
                // трупа новый бар не гасит.
                if self
                    .harvest
                    .as_ref()
                    .is_some_and(|h| h.entity_id == result.entity_id)
                {
                    self.harvest = None;
                }
                if result.ok {
                    // M23: в лут трупа добавились шкуры/мясо — окно
                    // подятия должно увидеть их без переоткрытия.
                    self.pickup_dirty = true;
                    let text = if result.action == "skin" {
                        format!("Вы сняли шкуру с трупа {}", result.name)
                    } else {
                        format!("Вы освежевали труп {}", result.name)
                    };
                    self.push_log(LogKind::Loot, text);
                } else if result.ruined {
                    // M22: разделка прервана взаимодействием с предметом.
                    self.push_log(LogKind::Loot, "Шкура испорчена".to_string());
                }
                vec![]
            }
            "evt/dialog/open" => {
                // M20: игровой диалог (заготовка) — гейт рендера окна
                // (`PopupKind::Dialog`) — наличие состояния, как у NPC.
                let Ok(open) = serde_json::from_value::<ld2_protocol::evt::DialogOpen>(payload)
                else {
                    return vec![];
                };
                self.dialog = Some(open);
                vec![]
            }
            "evt/item/state" => {
                // Патч предмета (upgrade/sockets/qty) — ФР-09/ФР-10.
                #[derive(serde::Deserialize)]
                struct ItemPatch {
                    uid: String,
                    patch: serde_json::Map<String, serde_json::Value>,
                }
                let Ok(update) = serde_json::from_value::<ItemPatch>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(item) = snap.char.inventory.iter_mut().find(|i| i.uid == update.uid)
                    {
                        if let Some(full) = update.patch.get("item") {
                            if let Ok(replaced) = serde_json::from_value::<
                                ld2_protocol::types::ItemSnapshot,
                            >(full.clone())
                            {
                                *item = replaced;
                            }
                        } else if let Some(up) =
                            update.patch.get("upgrade").and_then(|v| v.as_i64())
                        {
                            item.upgrade = up as i32;
                        } else if let Some(sockets) = update.patch.get("sockets") {
                            if let Ok(sockets) = serde_json::from_value::<
                                Vec<ld2_protocol::types::Socket>,
                            >(sockets.clone())
                            {
                                // M24: прирост зарядов — такт тикера
                                // (или новая вставка) — фаза
                                // прогрессбара рун обновляется.
                                let old_charges: i32 = item
                                    .sockets
                                    .as_ref()
                                    .map(|old| {
                                        old.iter()
                                            .filter_map(|s| s.rune.as_ref().map(|r| r.charges))
                                            .sum()
                                    })
                                    .unwrap_or(0);
                                let new_charges: i32 = sockets
                                    .iter()
                                    .filter_map(|s| s.rune.as_ref().map(|r| r.charges))
                                    .sum();
                                item.sockets = Some(sockets);
                                if new_charges > old_charges {
                                    self.rune_recharge_phase_ms =
                                        Some(local_epoch_ms() + self.time_offset * 1000);
                                }
                            }
                        } else if let Some(qty) = update.patch.get("qty").and_then(|v| v.as_i64()) {
                            item.qty = Some(qty);
                        }
                    }
                }
                vec![]
            }
            "evt/upgrade/result" => {
                // ФР-10: результат — событием; модалка закрывается.
                let Ok(result) =
                    serde_json::from_value::<ld2_protocol::evt::UpgradeResult>(payload.clone())
                else {
                    return vec![];
                };
                let name = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| {
                        s.char
                            .inventory
                            .iter()
                            .find(|i| i.uid == result.uid)
                            .map(|i| i.name.clone())
                    })
                    .unwrap_or_else(|| result.uid.clone());
                if result.ok {
                    let n = result.new_upgrade.unwrap_or_default();
                    self.push_log(LogKind::Loot, format!("Улучшение: «{name}» → +{n}"));
                } else if result.item_destroyed == Some(true) {
                    self.push_log(
                        LogKind::Crime,
                        format!("ПРОВАЛ улучшения — «{name}» РАЗРУШЕН"),
                    );
                } else {
                    self.push_log(
                        LogKind::Loot,
                        format!("Провал улучшения: «{name}» (без изменений)"),
                    );
                }
                // M18/F53: модалка не закрывается — сохраняем результат,
                // сбрасываем превью (следующее улучшение запросит новое).
                if let Some(view) = self.upgrade.as_mut() {
                    if view.item_uid == result.uid {
                        view.result = Some(payload.clone());
                        view.preview = None;
                    }
                }
                vec![Effect::Notify(NotifyKind::Upgrade)]
            }
            "evt/quest/progress" | "evt/quest/updated" => {
                #[derive(serde::Deserialize)]
                struct WithQuest {
                    quest: QuestState,
                    #[serde(default)]
                    pool_left: Option<i64>,
                }
                let Ok(w) = serde_json::from_value::<WithQuest>(payload) else {
                    return vec![];
                };
                // ФР-12: замена квеста в снапшоте + пул от сдачи (§7).
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(pool) = w.pool_left {
                        snap.char.xp_pool = pool;
                    }
                    if let Some(q) = snap.char.quests.iter_mut().find(|q| q.id == w.quest.id) {
                        *q = w.quest.clone();
                    } else {
                        snap.char.quests.push(w.quest.clone());
                    }
                }
                // M18/F35: квест взят/сдан — убирается из открытого диалога
                // NPC (реплики и кнопки не врут).
                if let Some(dialog) = self.npc_dialog.as_mut() {
                    if !matches!(w.quest.state, ld2_protocol::types::QuestPhase::Available) {
                        dialog.quests.retain(|q| q.id != w.quest.id);
                    }
                }
                let title = w.quest.title.unwrap_or(w.quest.id);
                self.push_log(
                    LogKind::Quest,
                    format!("Квест «{title}»: {:?}", w.quest.state),
                );
                vec![]
            }
            "evt/craft/result" => {
                let Ok(result) = serde_json::from_value::<ld2_protocol::evt::CraftResult>(payload)
                else {
                    return vec![];
                };
                if result.ok {
                    let made: Vec<String> = result
                        .made
                        .iter()
                        .map(|m| format!("{}×{}", m.uid, m.qty))
                        .collect();
                    self.push_log(
                        LogKind::Loot,
                        format!("Крафт готов: {} → {}", result.recipe_id, made.join(", ")),
                    );
                } else {
                    self.push_log(
                        LogKind::Loot,
                        format!("Крафт провален: {}", result.recipe_id),
                    );
                }
                vec![]
            }
            "evt/craft/profession" => {
                #[derive(serde::Deserialize)]
                struct WithProfession {
                    profession: ld2_protocol::types::ProfessionSnapshot,
                    #[serde(default)]
                    name: Option<String>,
                    #[serde(default)]
                    gain: Option<i64>,
                }
                let Ok(w) = serde_json::from_value::<WithProfession>(payload) else {
                    return vec![];
                };
                let old = self.snapshot.as_ref().and_then(|s| {
                    s.char
                        .professions
                        .iter()
                        .find(|p| p.id == w.profession.id)
                        .map(|p| (p.level, p.rank))
                });
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(p) = snap
                        .char
                        .professions
                        .iter_mut()
                        .find(|p| p.id == w.profession.id)
                    {
                        *p = w.profession.clone();
                    } else {
                        snap.char.professions.push(w.profession.clone());
                    }
                }
                // M22: журнал на русском (имя из контента) — ранг
                // только при смене, опыт и уровень профессии отдельно.
                let name = w.name.clone().unwrap_or_else(|| w.profession.id.clone());
                if old.is_none_or(|(_, rank)| rank != w.profession.rank) {
                    self.push_log(
                        LogKind::Loot,
                        format!("Профессия «{name}»: ранг {}", w.profession.rank),
                    );
                }
                if let Some(gain) = w.gain.filter(|g| *g > 0) {
                    self.push_log(LogKind::Xp, format!("{name}: опыт +{gain}"));
                }
                // Условие устойчиво: если записи в снапшоте ещё не
                // было (old=None) — сравниваем с0, иначе повышение
                // терялось (фикс: строка «+N» приходила, а об уровне —
                // нет).
                if w.profession.level > old.map(|(level, _)| level).unwrap_or(0) {
                    self.push_log(
                        LogKind::Xp,
                        format!("Уровень профессии {name}: {}", w.profession.level),
                    );
                }
                vec![]
            }
            "evt/party/updated" => {
                #[derive(serde::Deserialize)]
                struct WithParty {
                    #[serde(default)]
                    party: Option<PartyState>,
                    #[serde(default)]
                    invite: Option<PartyInviteView>,
                }
                let Ok(w) = serde_json::from_value::<WithParty>(payload) else {
                    return vec![];
                };
                // ФР-14: полное состояние группы + входящее приглашение (§11).
                self.party = w.party;
                if let Some(inv) = w.invite {
                    self.push_log(
                        LogKind::System,
                        format!("Приглашение в группу от {}", inv.from_name),
                    );
                    self.party_invite = Some(inv);
                    return vec![Effect::Notify(NotifyKind::PartyInvite)];
                }
                if self.party.is_some() {
                    self.party_invite = None; // состав обновился — ответ дан
                }
                vec![]
            }
            "evt/trade/state" => {
                let Ok(trade) = serde_json::from_value::<ld2_protocol::evt::TradeState>(payload)
                else {
                    return vec![];
                };
                // ФР-15: версия/зеркало — истина сервера, клиент не пересчитывает.
                // Открытие сделки партнёром — уведомление (D7).
                let opened = self.trade.is_none();
                self.trade = Some(trade);
                if opened {
                    return vec![Effect::Notify(NotifyKind::TradeOpen)];
                }
                vec![]
            }
            "evt/trade/closed" => {
                #[derive(serde::Deserialize)]
                struct Closed {
                    offer_id: String,
                    reason: ld2_protocol::evt::TradeCloseReason,
                }
                let Ok(c) = serde_json::from_value::<Closed>(payload) else {
                    return vec![];
                };
                self.trade = None;
                let text = match c.reason {
                    ld2_protocol::evt::TradeCloseReason::Done => {
                        format!("Сделка {} состоялась", c.offer_id)
                    }
                    ld2_protocol::evt::TradeCloseReason::Cancelled => {
                        format!("Сделка {} отменена", c.offer_id)
                    }
                    _ => format!("Сделка {} не состоялась", c.offer_id),
                };
                self.push_log(LogKind::Loot, text);
                vec![]
            }
            "evt/guild/updated" => {
                #[derive(serde::Deserialize)]
                struct WithGuild {
                    #[serde(default)]
                    guild: Option<GuildState>,
                    #[serde(default)]
                    invite: Option<GuildInviteView>,
                }
                let Ok(w) = serde_json::from_value::<WithGuild>(payload) else {
                    return vec![];
                };
                self.guild = w.guild;
                if let Some(inv) = w.invite {
                    self.push_log(
                        LogKind::System,
                        format!("Приглашение в гильдию «{}»", inv.guild_name),
                    );
                    self.guild_invite = Some(inv);
                    return vec![Effect::Notify(NotifyKind::GuildInvite)];
                }
                vec![]
            }
            "evt/guild/log" => {
                #[derive(serde::Deserialize)]
                struct WithEntry {
                    entry: GuildLogEntry,
                }
                let Ok(w) = serde_json::from_value::<WithEntry>(payload) else {
                    return vec![];
                };
                // §8: журнал гильдии попадает и в системный лог (ФР-16).
                self.push_log(LogKind::System, format!("Гильдия: {}", w.entry.text));
                self.guild_log.push(w.entry);
                vec![]
            }
            "evt/char/buffs" => {
                // M18/F58d (ADR-012 §6): полная замена списка баффов рун.
                let Ok(update) = serde_json::from_value::<ld2_protocol::evt::CharBuffs>(payload)
                else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.buffs = update.buffs.clone();
                }
                if !update.buffs.is_empty() {
                    let titles: Vec<&str> = update.buffs.iter().map(|b| b.title.as_str()).collect();
                    self.push_log(
                        LogKind::System,
                        format!("Бафф: {} ×{}", titles.join(", "), {
                            update.buffs.iter().map(|b| b.stacks).max().unwrap_or(1)
                        }),
                    );
                }
                vec![]
            }
            "evt/crime/status" => {
                let Ok(update) = serde_json::from_value::<CrimeStatusUpdate>(payload) else {
                    return vec![];
                };
                // ФР-11: полная замена списка — клиент НЕ пересчитывает blend (§4.10).
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.crime.statuses = update.statuses.clone();
                }
                let text = update.log_hint.unwrap_or_else(|| {
                    let kinds: Vec<&str> =
                        update.statuses.iter().map(|s| s.kind.as_str()).collect();
                    if kinds.is_empty() {
                        "Статусы преступления сняты".to_string()
                    } else {
                        format!("Статусы: {}", kinds.join(", "))
                    }
                });
                self.push_log(LogKind::Crime, text);
                vec![]
            }
            "evt/crime/added" => {
                let Ok(added) = serde_json::from_value::<CrimeAdded>(payload) else {
                    return vec![];
                };
                // Журнал + обновление бейджей (досерверный срок — как есть).
                self.crime_log.push(added.log_entry.clone());
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char
                        .crime
                        .statuses
                        .retain(|s| s.kind != added.status.kind);
                    snap.char.crime.statuses.push(added.status);
                }
                self.push_log(
                    LogKind::Crime,
                    format!(
                        "{}: +{}с (итого {}с)",
                        added.log_entry.kind, added.log_entry.delta_s, added.log_entry.total_s
                    ),
                );
                vec![]
            }
            "evt/crime/removed" => {
                let Ok(removed) = serde_json::from_value::<CrimeRemoved>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.crime.statuses.retain(|s| s.kind != removed.kind);
                }
                self.push_log(LogKind::Crime, format!("Статус «{}» снят", removed.kind));
                vec![]
            }
            "evt/chat/message" => {
                let Ok(msg) = serde_json::from_value::<ChatMessage>(payload) else {
                    return vec![];
                };
                // M14/F22: реплей истории (маркер replay) не показываем —
                // журнал и чат входят в игру пустыми; кольцо D25 живёт
                // на сервере, совместимость — через extra (§11).
                if msg.extra.get("replay").and_then(|v| v.as_bool()) == Some(true) {
                    return vec![];
                }
                // ФР-13: история per-channel (ring) + адресат для «ответить».
                let my_id = self
                    .snapshot
                    .as_ref()
                    .map(|sn| sn.char.id.clone())
                    .unwrap_or_default();
                let mut effects = vec![];
                if msg.channel == "whisper" && msg.from != my_id {
                    self.last_whisper_from = Some(msg.name.clone());
                    effects.push(Effect::Notify(NotifyKind::Whisper));
                }
                let entry = self.chat.entry(msg.channel.clone()).or_default();
                if entry.len() >= self.chat_limit {
                    entry.pop_front();
                }
                entry.push_back(msg.clone());
                // Заказчик 2026-11-20: в журнале канал — локализованной
                // меткой ([Локация], не [location]) и своим цветом.
                let (kind, label) = match msg.channel.as_str() {
                    "global" => (LogKind::ChatGlobal, t(self.lang, Key::ChGlobal)),
                    "party" => (LogKind::ChatParty, t(self.lang, Key::ChParty)),
                    "guild" => (LogKind::ChatGuild, t(self.lang, Key::ChGuild)),
                    "whisper" => (LogKind::ChatWhisper, t(self.lang, Key::ChWhisper)),
                    "system" => (LogKind::ChatSystem, t(self.lang, Key::ChSystem)),
                    _ => (LogKind::Chat, t(self.lang, Key::ChLocation)),
                };
                self.push_log(kind, format!("[{}] {}: {}", label, msg.name, msg.text));
                effects
            }
            "evt/chat/deleted" => {
                // M14/F19: модератор удалил сообщение — убираем из всех
                // каналов (id один на историю сервера).
                let Ok(del) = serde_json::from_value::<ld2_protocol::evt::ChatDeleted>(payload)
                else {
                    return vec![];
                };
                for ring in self.chat.values_mut() {
                    ring.retain(|m| m.id != Some(del.id));
                }
                vec![]
            }
            "evt/state/delta" => {
                // R11 (§3/§12.4): после state.resync сервер может ответить
                // дельтой — применяем JSON-patch к снапшоту; база не
                // совпала, неприменимый путь или битый результат →
                // полный resync (иначе клиент навсегда на устаревшем
                // состоянии).
                let Ok(delta) = serde_json::from_value::<StateDelta>(payload) else {
                    tracing::warn!("некорректный evt/state/delta → resync");
                    return self.request_resync();
                };
                let Some(snap) = self.snapshot.as_ref() else {
                    return self.request_resync();
                };
                if snap.state_version != delta.from_version {
                    tracing::warn!(
                        "delta: база {} ≠ снапшот {} → resync",
                        delta.from_version,
                        snap.state_version
                    );
                    return self.request_resync();
                }
                let mut value = match serde_json::to_value(snap) {
                    Ok(value) => value,
                    Err(_) => return self.request_resync(),
                };
                if !apply_json_patch(&mut value, &delta.patch) {
                    tracing::warn!("delta: неприменимый патч → resync");
                    return self.request_resync();
                }
                match serde_json::from_value::<StateSnapshot>(value) {
                    Ok(snap) => {
                        self.snapshot = Some(snap);
                        vec![]
                    }
                    Err(_) => {
                        tracing::warn!("delta: патч дал некорректный снапшот → resync");
                        self.request_resync()
                    }
                }
            }
            "evt/error" => {
                // R11 (§8): не-фатальные ошибки сервера — notice;
                // фатальная — notice + resync (восстановление состояния).
                let Ok(err) = serde_json::from_value::<ErrorNotice>(payload) else {
                    return vec![];
                };
                self.notice = Some(format!("⚠ {}: {}", err.code, err.message));
                if err.fatal == Some(true) {
                    return vec![Effect::ResyncState];
                }
                vec![]
            }
            _ => vec![], // неизвестные/ненужные события транспорт уже залогировал
        }
    }

    /// R11: дельта неприменима — полный снапшот (§12.4).
    fn request_resync(&self) -> Vec<Effect> {
        vec![Effect::ResyncState]
    }

    // ── журнал и история (ФР-17, ФР-03) ────────────────────────────────────

    fn push_log(&mut self, kind: LogKind, text: impl Into<String>) {
        if self.log.len() >= LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            kind,
            text: text.into(),
            at: local_epoch_ms(),
        });
    }

    /// M25: удалить строки журнала старше TTL (порядок — от старых);
    /// true — что-то удалено (нужен repaint). `ttl_s <= 0` — ничего
    /// не удаляется (настройка сервера `journal_ttl_s`).
    pub fn expire_log(&mut self, now_ms: i64, ttl_s: i64) -> bool {
        if ttl_s <= 0 {
            return false;
        }
        let limit = ttl_s.saturating_mul(1000);
        let mut removed = false;
        while let Some(front) = self.log.front() {
            if now_ms.saturating_sub(front.at) < limit {
                break;
            }
            self.log.pop_front();
            removed = true;
        }
        removed
    }

    fn push_visited(&mut self, location: &LocationSnapshot) {
        self.visited.retain(|v| v.id != location.id);
        if self.visited.len() >= VISITED_LIMIT {
            self.visited.remove(0);
        }
        self.visited.push(VisitedLocation {
            id: location.id.clone(),
            name: location.name.clone(),
        });
    }

    /// Отправка команды мира: сброс ошибки + фиксация pending (ФР-03).
    pub fn on_world_sent(&mut self, act: Act) {
        self.world_error = None;
        self.pending = Some(act);
    }

    /// Отправка команды листа/инвентаря (ФР-04/ФР-07).
    pub fn on_sheet_sent(&mut self, act: Act) {
        self.pending = Some(act);
    }

    /// M16/F29g: локальный обмен позициями предметов в сетке рюкзака
    /// (после cmd/item.move — порядок сервера подхватится при resync).
    pub fn swap_inventory(&mut self, a: &str, b: &str) {
        let Some(snap) = self.snapshot.as_mut() else {
            return;
        };
        let Some(i) = snap.char.inventory.iter().position(|i| i.uid == a) else {
            return;
        };
        let Some(j) = snap.char.inventory.iter().position(|i| i.uid == b) else {
            return;
        };
        snap.char.inventory.swap(i, j);
    }

    /// Подтверждение смерти (ФР-06): окно «Вы погибли» закрывается
    /// **только** этим действием — молча закрыть нельзя.
    pub fn confirm_death(&mut self) {
        self.death = None;
    }

    /// Открыть модалку улучшения (ФР-10); превью придёт от сервера.
    pub fn open_upgrade(&mut self, item_uid: String, item_name: String) {
        self.upgrade = Some(UpgradeView {
            item_uid,
            item_name,
            preview: None,
            result: None,
        });
    }

    pub fn close_upgrade(&mut self) {
        self.upgrade = None;
    }

    /// Открыть инкрустацию посоха (ФР-09).
    pub fn open_socketing(&mut self, staff_uid: String) {
        tracing::info!("socketing: открытие окна, staff={staff_uid}");
        self.socketing = Some(staff_uid);
    }

    pub fn close_socketing(&mut self) {
        tracing::info!("socketing: закрытие окна (close_socketing)");
        self.socketing = None;
    }

    /// M17/F30a: открыть список оружия под инкрустацию руны.
    pub fn open_inscribe(&mut self, rune_uid: String) {
        self.inscribe = Some(rune_uid);
    }

    pub fn close_inscribe(&mut self) {
        self.inscribe = None;
    }

    /// Выбор цели кликом по строке сущности (ФР-05).
    pub fn select_target(&mut self, entity_id: impl Into<String>) {
        self.target = Some(entity_id.into());
    }

    /// Цикл целей по Tab: первый монстр → следующий → (wrap) (ФР-05).
    /// M14/F4: длительность глобального кулдауна, мс (клиентская
    /// блокировка всех боевых действий: панель, руны, свитки/зелья).
    pub const GLOBAL_COOLDOWN_MS: i64 = 1000;

    /// Начать глобальный кулдаун с `now_ms` (epoch, мс). Уже
    /// действующее окно не укорачивается — берётся максимум.
    pub fn begin_global_cooldown(&mut self, now_ms: i64) {
        self.global_cooldown_until = self
            .global_cooldown_until
            .max(now_ms + Self::GLOBAL_COOLDOWN_MS);
    }

    /// Остаток глобального кулдауна, мс (≥0).
    /// M17/F30g: остаток и полная длительность кулдауна предмета
    /// (`item:<def_id>`); `None` — кулдауна нет.
    pub fn item_cd_bar(&self, def_id: &str, now_ms: i64) -> Option<(i64, i64)> {
        let (until, total) = self.item_cooldowns.get(&format!("item:{def_id}"))?;
        let left = (*until - now_ms).max(0);
        (left > 0).then_some((left, *total))
    }

    pub fn global_cooldown_left(&self, now_ms: i64) -> i64 {
        (self.global_cooldown_until - now_ms).max(0)
    }

    /// M14/F4: готовность боевого действия и показываемый остаток.
    /// Приоритет кулдаунов: **max(основной, глобальный)** — если
    /// основной длиннее, показываем его, иначе глобальный.
    /// Чистая функция — юнит-тест ниже.
    pub fn action_ready(own_cd_left_ms: i64, global_left_ms: i64) -> (bool, i64) {
        let left = own_cd_left_ms.max(global_left_ms);
        (left <= 0, left.max(0))
    }

    pub fn cycle_target(&mut self) {
        let Some(snap) = self.snapshot.as_ref() else {
            self.target = None;
            return;
        };
        let monsters: Vec<&str> = snap
            .location
            .entities
            .iter()
            .filter(|e| e.kind == ld2_protocol::types::EntityKind::Monster)
            .map(|e| e.id.as_str())
            .collect();
        if monsters.is_empty() {
            self.target = None;
            return;
        }
        let next = match self
            .target
            .as_deref()
            .and_then(|t| monsters.iter().position(|m| *m == t))
        {
            Some(pos) => monsters[(pos + 1) % monsters.len()].to_string(),
            None => monsters[0].to_string(),
        };
        self.target = Some(next);
    }

    /// Трекер квестов (ФР-12): переключение, максимум 3.
    pub fn toggle_quest_tracker(&mut self, quest_id: &str) {
        if let Some(pos) = self.quest_tracker.iter().position(|q| q == quest_id) {
            self.quest_tracker.remove(pos);
        } else if self.quest_tracker.len() < 3 {
            self.quest_tracker.push(quest_id.to_string());
        }
    }

    /// Подтверждение «забрать лут трупа игрока» (ФР-11).
    pub fn set_loot_confirm(&mut self, uid: String, name: String) {
        self.loot_confirm = Some((uid, name));
    }

    pub fn clear_loot_confirm(&mut self) {
        self.loot_confirm = None;
    }

    /// Сброс в экран входа с очисткой сессии/персонажа (чистый сброс, DoD M2).
    fn reset_to_login(&mut self, reason: Option<String>) -> Vec<Effect> {
        let login = self
            .session_login
            .clone()
            .or_else(|| self.last_login.clone());
        self.screen = Screen::Login;
        self.session = None;
        self.snapshot = None;
        self.pending = None;
        self.login_error = reason;
        self.create_error = None;
        self.select_error = None;
        self.world_error = None;
        self.death = None;
        self.item_detail = None;
        self.entity_detail = None;
        self.pickup = None;
        self.admin_catalog = None;
        self.admin_god = false;
        self.stun = None;
        self.admin_target_items = None;
        self.upgrade = None;
        self.socketing = None;
        self.dialog = None;
        self.target = None;
        self.loot_confirm = None;
        self.quest_tracker.clear();
        self.npc_dialog = None;
        self.session_login = None;
        self.chat.clear();
        // M14/F18: лог мира персональный — очистка при выходе.
        self.log.clear();
        self.last_whisper_from = None;
        self.party = None;
        self.party_invite = None;
        self.trade = None;
        self.guild = None;
        self.guild_invite = None;
        self.guild_log.clear();
        self.guild_browse_loaded = false;
        self.guild_browse.clear();
        login
            .map(|login| vec![Effect::ForgetSession { login }])
            .unwrap_or_default()
    }

    // ── действия UI (отправка команд фиксируют pending) ─────────────────────

    /// Пользователь нажал «Войти» — команда ушла в транспорт.
    pub fn on_login_sent(&mut self, login: &str) {
        self.last_login = Some(login.to_string());
        self.session_login = Some(login.to_string());
        self.login_error = None;
        self.notice = None;
        self.pending = Some(Act::Login);
    }

    /// Пользователь нажал «Зарегистрироваться» — команда ушла (M13).
    pub fn on_register_sent(&mut self, login: &str) {
        self.last_login = Some(login.to_string());
        self.session_login = Some(login.to_string());
        self.login_error = None;
        self.register_error = None;
        self.notice = None;
        self.pending = Some(Act::Register);
    }

    pub fn on_create_sent(&mut self) {
        self.create_error = None;
        self.pending = Some(Act::Create);
    }

    pub fn on_select_sent(&mut self) {
        self.select_error = None;
        self.pending = Some(Act::Select);
    }

    pub fn on_delete_sent(&mut self, char_id: &str) {
        self.notice = None;
        self.pending = Some(Act::Delete);
        self.deleting_char = Some(char_id.to_string());
    }

    /// Hot-switch: вернуться к выбору персонажей БЕЗ ре-логина (ФР-02).
    /// Снимок персонажа выгружается — следующий вход соберёт чистые сторы (DoD M2).
    /// Журнал и история — общий журнал аккаунта, переживают переключение.
    pub fn back_to_characters(&mut self) {
        self.screen = Screen::Characters;
        self.snapshot = None;
        self.select_error = None;
        self.world_error = None;
        self.entity_detail = None;
        self.pickup = None;
        self.admin_catalog = None;
        self.admin_god = false;
        self.stun = None;
        self.admin_target_items = None;
        self.item_detail = None;
        self.death = None;
        self.upgrade = None;
        self.socketing = None;
        self.dialog = None;
        self.quest_tracker.clear();
        self.npc_dialog = None;
        // ФР-13: переключение персонажа сбрасывает историю чата.
        self.chat.clear();
        // M14/F18: лог мира персональный — очистка при выходе из игры.
        self.log.clear();
        self.last_whisper_from = None;
        self.party = None;
        self.party_invite = None;
        self.trade = None;
        self.guild = None;
        self.guild_invite = None;
        self.guild_log.clear();
        self.notice = None;
    }

    /// Логаут (ФР-01): чистый сессионный сброс + отзыв токена.
    /// Смена сервера в настройках (ФР-19): возврат на экран входа БЕЗ
    /// отзыва keyring-токена (токен старого хоста остаётся валидным).
    pub fn to_login_screen(&mut self) {
        let _ = self.reset_to_login(None);
    }

    pub fn logout(&mut self) -> Vec<Effect> {
        self.reset_to_login(None)
    }

    /// M14/F19: может ли аккаунт писать в system-чат и удалять сообщения.
    /// Роль приходит в `session.start`; старый сервер → `player`.
    pub fn can_moderate(&self) -> bool {
        matches!(
            self.session.as_ref().map(|s| s.role.as_str()),
            Some("admin") | Some("moderator")
        )
    }

    pub fn slot_usage(&self) -> (usize, u32) {
        let used = self
            .session
            .as_ref()
            .map(|s| s.characters.len())
            .unwrap_or(0);
        let limit = self
            .session
            .as_ref()
            .map(|s| s.char_slot_limit)
            .unwrap_or(0);
        (used, limit)
    }
}

/// Локальное время в epoch-секундах.
pub fn local_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Локальное время в миллисекундах epoch (M14/F4: плавный отсчёт
/// кулдаунов на кнопках панели — тик 50 мс).
pub fn local_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Активные статусы на момент `now` (epoch): истёвшие скрываются локально —
/// fallback без событий (ФР-11). Клиент НЕ пересчитывает сроки (§4.10).
pub fn filter_active_statuses(statuses: &[CrimeStatus], now: i64) -> Vec<CrimeStatus> {
    statuses
        .iter()
        .filter(|s| s.expires_at > now)
        .cloned()
        .collect()
}

/// M18/F58f (ADR-012 §6): активные баффы рун — снятые/просроченные не
/// показываются (сервер присылает только активные, но локальный отсчёт
/// может отстать; §1.1 now = локальное + offset).
pub fn filter_active_buffs(
    buffs: &[ld2_protocol::types::BuffState],
    now: i64,
) -> Vec<&ld2_protocol::types::BuffState> {
    buffs.iter().filter(|b| b.expires_at > now).collect()
}

/// Применение `payload` ack'а `socket.*`: `{staff, rune?}` → инвентарь.
fn apply_item_patch(ch: &mut ld2_protocol::types::CharacterState, payload: &serde_json::Value) {
    if let Some(staff) = payload.get("staff").filter(|v| !v.is_null()) {
        if let Ok(staff) =
            serde_json::from_value::<ld2_protocol::types::ItemSnapshot>(staff.clone())
        {
            if let Some(slot) = ch.inventory.iter_mut().find(|i| i.uid == staff.uid) {
                *slot = staff;
            }
        }
    }
    if let Some(rune) = payload.get("rune").filter(|v| !v.is_null()) {
        if let Ok(rune) = serde_json::from_value::<ld2_protocol::types::ItemSnapshot>(rune.clone())
        {
            // M18 (указание заказчика): извлечённый самоцвет может
            // попасть в существующий стак — запись уже в инвентаре:
            // обновляем (qty выросло), иначе — добавляем.
            if let Some(slot) = ch.inventory.iter_mut().find(|i| i.uid == rune.uid) {
                *slot = rune;
            } else {
                ch.inventory.push(rune);
            }
        }
    }
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

/// Разбор ввода чата: `/w Имя текст`, `/p`, `/g`, `/r` (ответ последнему
/// whisper), `/scenario …` (passthrough-сценарии мока); обычный текст —
/// в активный канал. `whisper_to` — адресат для `/r` («ответить»).
pub fn parse_chat_input(input: &str, active_channel: &str, whisper_to: Option<&str>) -> ChatDraft {
    let trimmed = input.trim();
    // Сценарии мока — dev-команда, уходит как текст (мок перехватывает префикс).
    if trimmed.starts_with("/scenario ") {
        return ChatDraft::Send {
            channel: active_channel.to_string(),
            to: None,
            text: trimmed.to_string(),
        };
    }
    let Some(rest) = trimmed.strip_prefix('/') else {
        // Обычный текст — в активный канал.
        return ChatDraft::Send {
            channel: active_channel.to_string(),
            to: None,
            text: trimmed.to_string(),
        };
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or_default().to_ascii_lowercase();
    let arg = parts.next().unwrap_or("").trim();
    match cmd.as_str() {
        // `/w Имя текст` → whisper адресату.
        "w" | "whisper" => {
            let mut it = arg.splitn(2, char::is_whitespace);
            let to = it.next().unwrap_or("").to_string();
            let text = it.next().unwrap_or("").trim().to_string();
            if to.is_empty() || text.is_empty() {
                return ChatDraft::Unknown("/w".into());
            }
            ChatDraft::Send {
                channel: "whisper".into(),
                to: Some(to),
                text,
            }
        }
        // `/r текст` → ответ последнему адресату whisper.
        "r" | "reply" => match whisper_to {
            Some(to) if !arg.is_empty() => ChatDraft::Send {
                channel: "whisper".into(),
                to: Some(to.to_string()),
                text: arg.to_string(),
            },
            _ => ChatDraft::Unknown("/r".into()),
        },
        // `/p` / `/g` → каналы группы/гильдии.
        "p" | "party" if !arg.is_empty() => ChatDraft::Send {
            channel: "party".into(),
            to: None,
            text: arg.to_string(),
        },
        "g" | "guild" if !arg.is_empty() => ChatDraft::Send {
            channel: "guild".into(),
            to: None,
            text: arg.to_string(),
        },
        _ => ChatDraft::Unknown(format!("/{cmd}")),
    }
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

impl Profile {
    pub fn all() -> [Profile; 3] {
        [Profile::Warrior, Profile::Archer, Profile::Mage]
    }

    /// Wire-значение `cmd/char.create.profile`.
    pub fn value(self) -> &'static str {
        match self {
            Profile::Warrior => "warrior",
            Profile::Archer => "archer",
            Profile::Mage => "mage",
        }
    }

    /// Локализованное название профиля.
    pub fn label_key(self) -> crate::l10n::Key {
        use crate::l10n::Key;
        match self {
            Profile::Warrior => Key::ProfileWarrior,
            Profile::Archer => Key::ProfileArcher,
            Profile::Mage => Key::ProfileMage,
        }
    }
}

/// M18/F34: пол персонажа — wire-значения `cmd/char.create.gender`
/// (`male`/`female`); общий для клиентов (D12), задаётся при создании.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gender {
    #[default]
    Male,
    Female,
}

impl Gender {
    pub fn all() -> [Gender; 2] {
        [Gender::Male, Gender::Female]
    }

    /// Wire-значение `cmd/char.create.gender`.
    pub fn value(self) -> &'static str {
        match self {
            Gender::Male => "male",
            Gender::Female => "female",
        }
    }

    /// Локализованное название пола.
    pub fn label_key(self) -> crate::l10n::Key {
        use crate::l10n::Key;
        match self {
            Gender::Male => Key::GenderMale,
            Gender::Female => Key::GenderFemale,
        }
    }
}

/// Заголовок преступления из конфига сервера (§5 kinds), фолбэк — kind.
pub fn crime_kind_title(store: &AppStore, kind: &str) -> String {
    store
        .session
        .as_ref()
        .and_then(|s| s.crime.kinds.iter().find(|k| k.kind == kind))
        .map(|k| k.title.clone())
        .unwrap_or_else(|| kind.to_string())
}

/// R11 (§3): применение JSON-patch к снапшоту. Путь — RFC 6901
/// pointer («/location/entities/0/hp/cur»); `~0`/`~1` — эскейпы.
/// `false` — путь не существует / битое значение (клиент запрашивает
/// полный снапшот вместо тихого рассинхрона).
fn apply_json_patch(value: &mut serde_json::Value, patch: &[PatchOp]) -> bool {
    patch.iter().all(|op| apply_patch_op(value, op))
}

fn apply_patch_op(value: &mut serde_json::Value, op: &PatchOp) -> bool {
    let segments: Vec<String> = op
        .path
        .split('/')
        .skip(1)
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect();
    if segments.is_empty() {
        return false;
    }
    let mut cur = value;
    for seg in &segments[..segments.len() - 1] {
        let next = match cur {
            serde_json::Value::Object(map) => map.get_mut(seg),
            serde_json::Value::Array(arr) => {
                seg.parse::<usize>().ok().and_then(move |i| arr.get_mut(i))
            }
            _ => None,
        };
        let Some(next) = next else {
            return false;
        };
        cur = next;
    }
    let Some(last) = segments.last() else {
        return false;
    };
    match op.op {
        PatchOpKind::Remove => match cur {
            serde_json::Value::Object(map) => map.remove(last).is_some(),
            serde_json::Value::Array(arr) => last
                .parse::<usize>()
                .ok()
                .filter(|i| *i < arr.len())
                .map(|i| arr.remove(i))
                .is_some(),
            _ => false,
        },
        PatchOpKind::Add | PatchOpKind::Replace => {
            let Some(val) = op.value.clone() else {
                return false;
            };
            match cur {
                serde_json::Value::Object(map) => {
                    // replace — только существующий ключ (иначе клиент
                    // промолчал бы о структуре, которой не знал).
                    if op.op == PatchOpKind::Replace && !map.contains_key(last) {
                        return false;
                    }
                    map.insert(last.clone(), val);
                    true
                }
                serde_json::Value::Array(arr) => match last.parse::<usize>() {
                    Ok(i) if i < arr.len() => {
                        arr[i] = val;
                        true
                    }
                    Ok(i) if i == arr.len() && op.op == PatchOpKind::Add => {
                        arr.push(val);
                        true
                    }
                    _ => false,
                },
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M22: журнал опыта и повышения уровня профессии — обе строки
    /// приходят (воспроизводство: «+600» было, «Уровень…» — нет,
    /// когда в снапшоте не было записи профессии).
    #[test]
    fn profession_xp_and_level_up_log_lines() {
        let mut store = in_game();
        let first = serde_json::json!({
            "profession": {
                "id": "skinning", "rank": 1, "level": 14,
                "xp": 18, "xp_to_next": 53, "extra": {}
            },
            "name": "Снятие шкур",
            "gain": 600
        });
        store.apply_event("evt/craft/profession", Some(first));
        let log = |store: &AppStore| -> Vec<String> {
            store.log.iter().map(|l| l.text.clone()).collect()
        };
        let lines = log(&store);
        assert!(
            lines.iter().any(|t| t == "Снятие шкур: опыт +600"),
            "строка опыта: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|t| t == "Уровень профессии Снятие шкур: 14"),
            "строка уровня (записи в снапшоте не было): {lines:?}"
        );
        // Повторное событие без повышения уровня — новой строки нет.
        let second = serde_json::json!({
            "profession": {
                "id": "skinning", "rank": 1, "level": 14,
                "xp": 18, "xp_to_next": 53, "extra": {}
            },
            "name": "Снятие шкур",
            "gain": 10
        });
        store.apply_event("evt/craft/profession", Some(second));
        let lines = log(&store);
        assert_eq!(
            lines
                .iter()
                .filter(|t| t.as_str() == "Уровень профессии Снятие шкур: 14")
                .count(),
            1,
            "уровень логируется один раз: {lines:?}"
        );
    }

    /// M14/F4: глобальный кулдаун — 1 с; повтор не укорачивает окно.
    #[test]
    fn global_cooldown_blocks_one_second_and_never_shrinks() {
        let mut store = AppStore::new();
        assert_eq!(store.global_cooldown_left(1_000), 0, "не начат");

        store.begin_global_cooldown(1_000);
        assert_eq!(
            store.global_cooldown_left(1_000),
            AppStore::GLOBAL_COOLDOWN_MS
        );
        assert_eq!(store.global_cooldown_left(1_500), 500, "спадает");
        assert_eq!(store.global_cooldown_left(2_000), 0, "истёк");

        // Повтор через 500 мс продлевает, но не откатывает окно.
        store.begin_global_cooldown(1_500);
        assert_eq!(store.global_cooldown_until, 2_500);
        // Старое время (гонка тиков) не укорачивает уже действующее.
        store.begin_global_cooldown(1_100);
        assert_eq!(store.global_cooldown_until, 2_500);
    }

    /// M14/F4: показываемый остаток = max(основной, глобальный).
    #[test]
    fn action_ready_takes_longest_cooldown() {
        assert_eq!(AppStore::action_ready(0, 0), (true, 0), "оба свободны");
        assert_eq!(
            AppStore::action_ready(500, 0),
            (false, 500),
            "только основной"
        );
        assert_eq!(
            AppStore::action_ready(0, 700),
            (false, 700),
            "только глобальный"
        );
        assert_eq!(
            AppStore::action_ready(900, 300),
            (false, 900),
            "основной длиннее"
        );
        assert_eq!(
            AppStore::action_ready(300, 900),
            (false, 900),
            "глобальный длиннее"
        );
        assert_eq!(
            AppStore::action_ready(-1, -5),
            (true, 0),
            "истёкшие — готово"
        );
    }
    use ld2_test_kit::fixtures;

    // ─── M11.6: E2E-тест вехи — диспетчер всплывающих диалогов ─────────────
    // Прогон сценария: создание персонажа → мир → смерть → лут → инвайт;
    // тот же AppStore::active_dialog читает UI-диспетчер gpui-клиента.

    #[test]
    fn dialog_dispatch_e2e() {
        let mut s = AppStore::new();
        // Вне игры/персонажей диалогов нет, даже с флагами UI.
        assert_eq!(s.active_dialog(true, true), None);

        // Экран персонажей: создание > удаление.
        s.screen = Screen::Characters;
        assert_eq!(s.active_dialog(false, false), None);
        assert_eq!(s.active_dialog(false, true), Some(Dialog::Delete));
        assert_eq!(s.active_dialog(true, true), Some(Dialog::Create));

        // Мир: флаги персонажей не действуют.
        s.screen = Screen::Game;
        assert_eq!(s.active_dialog(true, false), None);

        // Смерть главнее всего (ФР-06), осмотр/лут — после неё.
        s.death = Some(
            serde_json::from_value(serde_json::json!({
                "victim_char": true,
                "corpse": "c1",
                "respawn": {"location_id": "l1", "delay_s": 0}
            }))
            .expect("death"),
        );
        s.entity_detail = Some(serde_json::json!({"entity": {"name": "x"}}));
        s.loot_confirm = Some(("uid".into(), "Труп".into()));
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Death));
        s.death = None;
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Examine));
        s.entity_detail = None;
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Loot));
        s.loot_confirm = None;

        // Инвайт в группу > гильдия > сделка (ФР-14/15/16).
        s.party_invite = Some(PartyInviteView {
            invite_id: "i1".into(),
            from: "p1".into(),
            from_name: "Игрок".into(),
        });
        s.guild_invite = Some(GuildInviteView {
            guild_id: "g1".into(),
            guild_name: "Г".into(),
            from_name: "Игрок".into(),
        });
        s.trade = Some(
            serde_json::from_value(serde_json::json!({
                "offer_id": "o1", "version": 1,
                "you_gold": 0, "them_gold": 0,
                "you_locked": false, "them_locked": false
            }))
            .expect("trade"),
        );
        assert_eq!(s.active_dialog(false, false), Some(Dialog::PartyInvite));
        s.party_invite = None;
        assert_eq!(s.active_dialog(false, false), Some(Dialog::GuildInvite));
        s.guild_invite = None;
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Trade));
        s.trade = None;

        // Лист: инкрустация (ФР-09); описание предмета и заточка —
        // попапы (M18/F53), вне диспетчера модалок.
        s.item_detail = Some(serde_json::json!({"name": "x"}));
        // M18: окно сокетов — popup (вне диспетчера модалок).
        s.socketing = Some("staff".into());
        assert_eq!(s.active_dialog(false, false), None, "сокетинг — popup");
        s.socketing = None;
        // M17/F30a: выбор цели инкрустации — отдельный диалог.
        s.inscribe = Some("rune_1".into());
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Inscribe));
        s.inscribe = None;
        // M18/F38: диалог NPC — попап: не участвует в диспетчере
        // модалок (модалки смерти/осмотра/луута — выше по приоритету).
        s.npc_dialog = Some(NpcDialogView::default());
        assert_eq!(s.active_dialog(false, false), None);
        s.npc_dialog = None;
        assert_eq!(s.active_dialog(false, false), None);
    }

    // ─── M11.7: таймер удаления (D20) ──────────────────────────────────────

    #[test]
    fn deletion_timer_schedule_tick_cancel() {
        let mut s = AppStore::new();
        s.schedule_delete("c1".into(), "Ким".into(), 1_000, 10);
        assert_eq!(s.tick_delete(1_005), None, "до срока — не уходит");
        assert!(s.pending_delete.is_some());
        assert_eq!(s.tick_delete(1_010), Some("c1".into()), "срок истёк");
        assert!(s.pending_delete.is_none(), "одноразово");
        assert_eq!(s.tick_delete(1_020), None, "повторный тик — пусто");

        s.schedule_delete("c2".into(), "Астра".into(), 2_000, 30);
        s.cancel_delete();
        assert_eq!(s.tick_delete(3_000), None, "отменено — не уходит");
        assert!(s.pending_delete.is_none());

        // Новая запись замещает старую (отложенных — не больше одного).
        s.schedule_delete("c1".into(), "Ким".into(), 3_000, 5);
        s.schedule_delete("c2".into(), "Астра".into(), 3_000, 60);
        assert_eq!(s.tick_delete(3_010), None, "старая заменена");
        assert_eq!(
            s.pending_delete.as_ref().map(|p| p.char_id.as_str()),
            Some("c2")
        );
    }

    // ─── M8: чат (ФР-13) ───────────────────────────────────────────────────

    #[test]
    fn slash_parse_whisper_and_channels() {
        // `/w Имя текст` → whisper адресату.
        assert_eq!(
            parse_chat_input("/w Астра привет!", "location", None),
            ChatDraft::Send {
                channel: "whisper".into(),
                to: Some("Астра".into()),
                text: "привет!".into()
            }
        );
        // `/p` / `/g` → каналы группы/гильдии.
        assert_eq!(
            parse_chat_input("/p идём в лес", "location", None),
            ChatDraft::Send {
                channel: "party".into(),
                to: None,
                text: "идём в лес".into()
            }
        );
        assert_eq!(
            parse_chat_input("/g привет гильдии", "global", None),
            ChatDraft::Send {
                channel: "guild".into(),
                to: None,
                text: "привет гильдии".into()
            }
        );
        // Обычный текст — в активный канал.
        assert_eq!(
            parse_chat_input("просто текст", "location", None),
            ChatDraft::Send {
                channel: "location".into(),
                to: None,
                text: "просто текст".into()
            }
        );
    }

    #[test]
    fn slash_parse_reply_and_unknown() {
        // `/r` с адресатом последнего whisper.
        assert_eq!(
            parse_chat_input("/r тоже привет", "location", Some("Астра")),
            ChatDraft::Send {
                channel: "whisper".into(),
                to: Some("Астра".into()),
                text: "тоже привет".into()
            }
        );
        // Без адресата — ошибка, не отправляется.
        assert_eq!(
            parse_chat_input("/r привет", "location", None),
            ChatDraft::Unknown("/r".into())
        );
        // Неизвестная команда — ошибка тостом (ФР-13), текстом в чат не идёт.
        assert_eq!(
            parse_chat_input("/xyz ага", "location", None),
            ChatDraft::Unknown("/xyz".into())
        );
        // Неполный `/w` — ошибка.
        assert_eq!(
            parse_chat_input("/w Астра", "location", None),
            ChatDraft::Unknown("/w".into())
        );
        // Сценарии мока — passthrough (dev-команда мока).
        assert_eq!(
            parse_chat_input("/scenario death", "global", None),
            ChatDraft::Send {
                channel: "global".into(),
                to: None,
                text: "/scenario death".into()
            }
        );
    }

    /// M14/F19: evt/chat/deleted убирает строку из истории канала;
    /// can_moderate — по роли из session.start (старый сервер → player).
    /// M14/F22: строки с маркером replay не попадают ни в чат, ни в
    /// журнал — прошлая сессия не навязывается новому входу.
    #[test]
    fn replay_marked_messages_not_shown() {
        let mut store = store_with_session();
        let log_before = store.log.len();

        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "id": 7, "channel": "global", "from": "p", "name": "Старый",
                "text": "вчерашнее", "sent_at": "2026-11-08T12:00:00Z",
                "replay": true
            })),
        }));
        assert!(
            !store.chat.contains_key("global"),
            "реплей не создаёт кольцо канала"
        );
        assert_eq!(store.log.len(), log_before, "реплей не пушится в журнал");

        // Обычное сообщение (без маркера) — как раньше.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "id": 8, "channel": "global", "from": "p", "name": "Живой",
                "text": "сейчас", "sent_at": "2026-11-09T12:00:00Z"
            })),
        }));
        assert_eq!(store.chat.get("global").expect("живое сообщение").len(), 1);
        assert_eq!(store.log.len(), log_before + 1, "живое — в журнал идёт");
    }

    #[test]
    fn chat_deleted_event_removes_message() {
        let mut store = store_with_session();
        assert!(!store.can_moderate(), "fixtures: роль player");
        store.session.as_mut().expect("сессия").role = "moderator".into();
        assert!(store.can_moderate(), "модератор может");

        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "id": 42, "channel": "system", "from": "p", "name": "Мод",
                "text": "служебное", "sent_at": "2026-11-09T00:00:00Z"
            })),
        }));
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "id": 43, "channel": "system", "from": "p", "name": "Мод",
                "text": "остаётся", "sent_at": "2026-11-09T00:00:01Z"
            })),
        }));
        let sys = store.chat.get("system").expect("канал system");
        assert_eq!(sys.len(), 2);

        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/deleted".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({ "id": 42 })),
        }));
        let sys = store.chat.get("system").expect("канал system");
        assert_eq!(sys.len(), 1, "удалена только строка с id");
        assert_eq!(sys[0].text, "остаётся");
        assert_eq!(sys[0].id, Some(43), "id пережил удаление соседа");
    }

    #[test]
    fn chat_history_is_ring_per_channel() {
        let mut store = store_with_session();
        let limit = store.chat_limit;
        for i in 0..(limit + 5) {
            store.apply(IngressEvent::Event(ld2_protocol::Envelope {
                v: 1,
                msg_type: "evt/chat/message".into(),
                seq: None,
                ts: None,
                state_version: None,
                payload: Some(serde_json::json!({
                    "channel": "global", "from": "p", "name": "Марк",
                    "text": format!("м {i}"), "sent_at": "2026-11-03T00:00:00Z"
                })),
            }));
        }
        let global = store.chat.get("global").expect("канал global");
        assert_eq!(global.len(), limit, "ring-буфер: лимит из конфига");
        assert_eq!(global.front().unwrap().text, "м 5", "старые вытеснены");
        // Другой канал не тронут.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "channel": "system", "from": "srv", "name": "Сервер",
                "text": "событие", "sent_at": "2026-11-03T00:00:00Z"
            })),
        }));
        assert_eq!(store.chat.get("system").unwrap().len(), 1);
    }

    #[test]
    fn party_invite_opens_pending_and_accept_clears_it() {
        let mut store = store_with_session();
        // Приглашение (поле invite, §11) → модалка ответа.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/party/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "party": serde_json::Value::Null,
                "invite": { "invite_id": "inv_1", "from": "c1", "from_name": "Ким" }
            })),
        }));
        let inv = store.party_invite.as_ref().expect("приглашение");
        assert_eq!(inv.invite_id, "inv_1");
        assert_eq!(inv.from_name, "Ким");
        assert!(store.party.is_none());

        // Принятие: состав группы приходит тем же событием.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/party/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "party": {
                    "id": "pt_1", "leader": "c1",
                    "members": [
                        { "id": "c1", "name": "Ким" },
                        { "id": "c2", "name": "Астра" }
                    ]
                }
            })),
        }));
        let party = store.party.as_ref().expect("группа");
        assert_eq!(party.members.len(), 2);
        assert_eq!(party.leader, "c1");
        assert!(store.party_invite.is_none(), "ответ дан — модалка закрыта");

        // Выход/распускание: party: null.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/party/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({ "party": null })),
        }));
        assert!(store.party.is_none());
    }

    #[test]
    fn trade_state_mirror_version_and_close() {
        let mut store = store_with_session();
        // Зеркальная перспектива: you — своя сторона, them — чужая (ФР-15).
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/trade/state".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "offer_id": "of_c1", "version": 3,
                "you": [ { "uid": "m_1", "type": "ingot_iron", "name": "Слиток",
                            "rarity": "common", "upgrade": 0, "stats": {} } ],
                "them": [],
                "you_gold": 10, "them_gold": 0,
                "you_locked": false, "them_locked": true,
                "value_hint": 15
            })),
        }));
        let trade = store.trade.as_ref().expect("сделка");
        assert_eq!(trade.version, 3, "версия — истина сервера");
        assert_eq!(trade.you.len(), 1);
        assert_eq!(trade.you[0].uid, "m_1");
        assert!(trade.them.is_empty());
        assert!(trade.them_locked && !trade.you_locked);
        assert_eq!(trade.value_hint, Some(15));

        // Завершение: done закрывает окно и пишет в журнал.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/trade/closed".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({ "offer_id": "of_c1", "reason": "done" })),
        }));
        assert!(store.trade.is_none());
        assert!(store.log.iter().any(|e| e.text.contains("состоялась")));
    }

    #[test]
    fn guild_update_invite_and_log() {
        let mut store = store_with_session();
        // Приглашение в гильдию (§11).
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/guild/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "guild": serde_json::Value::Null,
                "invite": { "guild_id": "g_1", "guild_name": "Стальные волки",
                             "from_name": "Ким" }
            })),
        }));
        let inv = store.guild_invite.as_ref().expect("приглашение");
        assert_eq!(inv.guild_name, "Стальные волки");

        // Состояние гильдии + журнал.
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/guild/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "guild": { "id": "g_1", "name": "Стальные волки",
                            "members": [ { "id": "c1", "name": "Ким",
                                           "rank": "master", "online": true } ] }
            })),
        }));
        assert_eq!(store.guild.as_ref().expect("гильдия").members.len(), 1);
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/guild/log".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "entry": { "kind": "rank_change", "text": "Ранг c2 → member",
                            "at": 1793700000 }
            })),
        }));
        assert_eq!(store.guild_log.len(), 1);
        assert!(store.log.iter().any(|e| e.text.contains("Гильдия")));
    }

    /// НФР/§12: min_client capability сервера новее клиента → предупреждение.
    #[test]
    fn newer_min_client_shows_warning() {
        let mut store = AppStore::new();
        store.on_login_sent("demo");
        let mut cfg = fixtures::session_config(8);
        cfg.server.min_client = Some("99.0.0".into());
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::to_value(cfg).unwrap()),
        }));
        let notice = store.notice.expect("предупреждение о версии");
        assert!(notice.contains("99.0.0"), "нужна версия сервера: {notice}");

        // Текущая/старая версия — без предупреждения.
        let mut store = AppStore::new();
        store.on_login_sent("demo");
        let mut cfg = fixtures::session_config(8);
        cfg.server.min_client = Some("0.1.0".into());
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::to_value(cfg).unwrap()),
        }));
        assert!(store.notice.is_none(), "старая версия сервера — тихо");
    }

    /// D7: события уведомлений эмитятся эффектами (флаг применяет App).
    #[test]
    fn notify_effects_emitted_for_d7_events() {
        let mut store = store_with_session();

        // Смерть.
        let effects = store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/char/death".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "victim_char": true, "lost_items": [], "corpse": "cor_self",
                "respawn": { "location_id": "l_village", "delay_s": 0 }
            })),
        }));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Notify(NotifyKind::Death))),
            "смерть → уведомление: {effects:?}"
        );

        // Приглашение в группу.
        let mut store = store_with_session();
        let effects = store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/party/updated".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "party": null,
                "invite": { "invite_id": "inv_1", "from": "c1", "from_name": "Ким" }
            })),
        }));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::Notify(NotifyKind::PartyInvite))));

        // Whisper от другого.
        let mut store = store_with_session();
        let effects = store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "channel": "whisper", "from": "c2", "name": "Астра",
                "text": "эй", "sent_at": "2026-11-04T00:00:00Z"
            })),
        }));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::Notify(NotifyKind::Whisper))));

        // Своё сообщение whisper — без уведомления (нужен снапшот: id «я»).
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/state/snapshot".into(),
            seq: None,
            ts: None,
            state_version: Some(1003),
            payload: Some(
                serde_json::to_value(fixtures::snapshot(
                    1003,
                    &fixtures::character("c1", "Ким"),
                    &fixtures::village(),
                ))
                .expect("снапшот"),
            ),
        }));
        let my_id = store.snapshot.as_ref().unwrap().char.id.clone();
        let effects = store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "channel": "whisper", "from": my_id, "name": "Ким",
                "text": "себе", "sent_at": "2026-11-04T00:00:00Z"
            })),
        }));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Notify(NotifyKind::Whisper))),
            "своё сообщение не уведомляет"
        );
    }

    #[test]
    fn social_state_resets_on_char_switch() {
        let mut store = store_with_session();
        store.party = Some(PartyState {
            id: "pt_1".into(),
            leader: "c1".into(),
            members: vec![],
            extra: Default::default(),
        });
        store.back_to_characters();
        assert!(store.party.is_none(), "пати сброшено при hot-switch");
        assert!(store.guild_browse.is_empty());
    }

    #[test]
    fn chat_resets_on_char_switch_and_whisper_reply_target() {
        let mut store = store_with_session();
        // Whisper от другого игрока запоминает адресата (ФР-13 «ответить»).
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "channel": "whisper", "from": "c2", "name": "Астра",
                "text": "эй", "sent_at": "2026-11-03T00:00:00Z"
            })),
        }));
        assert_eq!(store.last_whisper_from.as_deref(), Some("Астра"));
        assert_eq!(store.chat.get("whisper").unwrap().len(), 1);

        // Переключение персонажа сбрасывает историю (ФР-13).
        store.back_to_characters();
        assert!(store.chat.is_empty(), "история чата сброшена");
        assert!(store.last_whisper_from.is_none());
    }

    fn store_with_session() -> AppStore {
        let mut store = AppStore::new();
        let effects = store.apply(IngressEvent::Status(TransportStatus::Online));
        assert!(effects.is_empty());
        store.on_login_sent("demo");
        let cfg = serde_json::to_value(fixtures::session_config(8)).unwrap();
        let effects = store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(cfg),
        }));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::PersistSession { .. })));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::TransportToken(_))));
        store
    }

    fn snapshot_event() -> IngressEvent {
        let snap = serde_json::to_value(fixtures::snapshot(
            1002,
            &fixtures::character("c1", "Ким"),
            &fixtures::village(),
        ))
        .unwrap();
        IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/state/snapshot".into(),
            seq: None,
            ts: None,
            state_version: Some(1002),
            payload: Some(snap),
        })
    }

    #[test]
    fn session_start_moves_to_characters_with_effects() {
        let store = store_with_session();
        assert_eq!(store.screen, Screen::Characters);
        assert_eq!(store.slot_usage(), (1, 8));
        assert!(store.login_error.is_none());
        assert!(!store.forced_update, "без min_client — вход разрешён");
    }

    #[test]
    fn session_start_min_client_forces_update() {
        let mut store = store_with_session();
        // Сервер требует более новый клиент → вход в мир блокируется.
        let mut cfg = serde_json::to_value(fixtures::session_config(8)).unwrap();
        cfg["server"]["min_client"] = serde_json::json!("99999.0.0");
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(cfg),
        }));
        assert!(store.forced_update);
        assert_eq!(store.min_client_required.as_deref(), Some("99999.0.0"));
        assert!(store.notice.is_some(), "предупреждение видно");

        // Требование равно версии клиента — вход разрешён.
        let mut cfg = serde_json::to_value(fixtures::session_config(8)).unwrap();
        cfg["server"]["min_client"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(cfg),
        }));
        assert!(!store.forced_update);
    }

    #[test]
    fn snapshot_moves_to_game_and_switch_is_clean() {
        let mut store = store_with_session();
        store.apply(snapshot_event());
        assert_eq!(store.screen, Screen::Game);
        assert!(store.snapshot.is_some());

        // DoD M2: смена персонажа = чистый сброс сторов (снимок выгружен).
        store.back_to_characters();
        assert_eq!(store.screen, Screen::Characters);
        assert!(store.snapshot.is_none());

        // Сессия переживает hot-switch — ре-логин не нужен.
        assert!(store.session.is_some());
        store.apply(snapshot_event());
        assert_eq!(store.screen, Screen::Game);
    }

    /// M14/F18: лог мира персональный — не переживает вход и выход.
    #[test]
    fn log_cleared_on_enter_and_exit() {
        let mut store = store_with_session();
        // Вход в игру: лог содержит только строку «Вход в мир».
        store.apply(snapshot_event());
        assert_eq!(store.screen, Screen::Game);
        assert_eq!(
            store.log.len(),
            1,
            "при входе лог очищается — только «Вход в мир»"
        );
        // Накопили строк — выход из игры очищает.
        store.back_to_characters();
        assert!(
            store.log.is_empty(),
            "смена персонажа/выход из игры чистит лог"
        );
        // Повторный вход — снова чисто.
        store.apply(snapshot_event());
        assert_eq!(store.log.len(), 1, "повторный вход начинается заново");
        // Логаут (экран входа) тоже чистит.
        store.logout();
        assert!(store.log.is_empty(), "логаут чистит лог");
    }

    #[test]
    fn login_ack_error_shows_message() {
        let mut store = AppStore::new();
        store.on_login_sent("demo");
        let ack = ld2_protocol::Ack::error(
            1,
            None,
            &ErrorCode::BadCredentials,
            "неверный логин или пароль",
        );
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(
            store.login_error.as_deref(),
            Some("неверный логин или пароль")
        );
        assert_eq!(store.screen, Screen::Login);
        assert!(store.pending.is_none());
    }

    #[test]
    fn auth_expired_resets_session_and_forgets_token() {
        let mut store = store_with_session();
        store.apply(snapshot_event());
        assert_eq!(store.screen, Screen::Game);

        let effects = store.apply(IngressEvent::Status(TransportStatus::AuthExpired));
        assert_eq!(store.screen, Screen::Login);
        assert!(store.session.is_none(), "сессия очищена");
        assert!(store.snapshot.is_none());
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::ForgetSession { .. })),
            "токен отозван"
        );
    }

    #[test]
    fn session_start_on_reconnect_keeps_game_screen() {
        // §4.5: session.start повторяется при resume — игрок не выкидывается.
        let mut store = store_with_session();
        store.apply(snapshot_event());
        let cfg = serde_json::to_value(fixtures::session_config(8)).unwrap();
        store.apply(IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/session.start".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(cfg),
        }));
        assert_eq!(
            store.screen,
            Screen::Game,
            "реконнект не выкидывает из игры"
        );
        assert!(store.session.is_some());
    }

    #[test]
    fn create_ack_appends_character() {
        let mut store = store_with_session();
        store.on_create_sent();
        let mut new_char = fixtures::character_summary();
        new_char.id = "c2".into();
        new_char.name = "Арн".into();
        let ack = ld2_protocol::Ack::ok_with_payload(
            7,
            Some(1003),
            &serde_json::json!({ "character": new_char }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.slot_usage(), (2, 8), "персонаж добавлен в список");
        assert_eq!(store.notice.as_deref(), Some("Персонаж создан"));
    }

    #[test]
    fn delete_ack_error_surfaces_notice() {
        let mut store = store_with_session();
        let id = store.session.as_ref().unwrap().characters[0].id.clone();
        store.on_delete_sent(&id);
        let ack = ld2_protocol::Ack::error(9, None, &ErrorCode::CharLimit, "нельзя удалить");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.notice.as_deref(), Some("нельзя удалить"));
        assert_eq!(store.slot_usage(), (1, 8), "список не изменился");
    }

    /// M11.7: успешный ack удаления снимает карточку (ФР-02).
    #[test]
    fn delete_ack_removes_character_from_list() {
        let mut store = store_with_session();
        assert_eq!(store.slot_usage(), (1, 8), "до удаления");
        let id = store.session.as_ref().unwrap().characters[0].id.clone();
        store.on_delete_sent(&id);
        let ack = ld2_protocol::Ack::ok(10, Some(1004));
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.slot_usage(), (0, 8), "карточка убрана по ack");
        assert_eq!(store.notice.as_deref(), Some("Персонаж удалён"));
        assert!(store.deleting_char.is_none());
    }

    #[test]
    fn transport_stopped_returns_to_login() {
        let mut store = store_with_session();
        store.apply(snapshot_event());
        let effects = store.apply(IngressEvent::Status(TransportStatus::Stopped));
        assert_eq!(store.screen, Screen::Login);
        assert!(store.snapshot.is_none());
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::ForgetSession { .. })));
        assert!(store.notice.is_some(), "пользователь видит причину");
    }

    // ─── M3: мир, журнал, история ──────────────────────────────────────────

    fn evt(ty: &str, payload: serde_json::Value) -> IngressEvent {
        IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: ty.to_string(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(payload),
        })
    }

    fn in_game() -> AppStore {
        let mut store = store_with_session();
        store.apply(snapshot_event());
        assert_eq!(store.screen, Screen::Game);
        store
    }

    #[test]
    fn location_changed_updates_location_visited_and_log() {
        let mut store = in_game();
        assert_eq!(store.visited.len(), 1, "деревня при входе в мир");

        let payload = serde_json::to_value(LocationChanged {
            location: fixtures::forest(),
        })
        .unwrap();
        store.apply(evt("evt/location/changed", payload));

        let snap = store.snapshot.as_ref().unwrap();
        assert_eq!(snap.location.id, "l_forest");
        assert_eq!(store.visited.len(), 2, "история: деревня + лес");
        assert_eq!(store.visited[1].name, "Черный лес");
        let last = store.log.back().expect("запись лога");
        assert_eq!(last.kind, LogKind::Location);
        assert!(last.text.contains("Черный лес"), "{}", last.text);
        assert!(store.entity_detail.is_none());

        // Возврат в деревню: дедуп истории (свежая в конце, без дублей).
        let payload = serde_json::to_value(LocationChanged {
            location: fixtures::village(),
        })
        .unwrap();
        store.apply(evt("evt/location/changed", payload));
        assert_eq!(store.visited.len(), 2, "дедуп по id");
        assert_eq!(store.visited[1].id, "l_village");
    }

    #[test]
    fn combat_hit_logged_by_direction() {
        let mut store = in_game();
        let char_id = "c1".to_string();

        let hit = |source: &str| {
            serde_json::json!({
                "source": source, "source_name": "Ким", "target": "m_3", "target_name": "Волк",
                "kind": "melee", "amount": 7, "crit": false, "miss": false,
                "text": format!("удар от {source}")
            })
        };
        store.apply(evt("evt/combat/hit", hit(&char_id)));
        store.apply(evt("evt/combat/hit", hit("m_3")));

        let kinds: Vec<LogKind> = store.log.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![LogKind::Location, LogKind::CombatOut, LogKind::CombatIn],
            "свой урон — исходящий, чужой — входящий (ФР-05)"
        );
    }

    #[test]
    fn entity_despawn_removes_and_logs() {
        let mut store = in_game();
        assert!(store
            .snapshot
            .as_ref()
            .unwrap()
            .location
            .entities
            .iter()
            .any(|e| e.id == "npc_elder"));

        store.apply(evt(
            "evt/entity/despawn",
            serde_json::json!({ "entity_id": "npc_elder", "location_id": "l_village" }),
        ));
        assert!(
            !store
                .snapshot
                .as_ref()
                .unwrap()
                .location
                .entities
                .iter()
                .any(|e| e.id == "npc_elder"),
            "сущность удалена"
        );
        let last = store.log.back().expect("лог");
        assert!(last.text.contains("Староста"), "{}", last.text);
    }

    #[test]
    fn move_ack_ok_applies_location_and_error_sets_world_error() {
        let mut store = in_game();
        store.on_world_sent(Act::Move);

        let ack = ld2_protocol::Ack::ok_with_payload(
            5,
            Some(1003),
            &serde_json::json!({ "location": fixtures::forest() }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(
            store.snapshot.as_ref().unwrap().location.id,
            "l_forest",
            "ack несёт новую локацию"
        );
        assert!(store.pending.is_none() && store.world_error.is_none());

        store.on_world_sent(Act::Move);
        let ack = ld2_protocol::Ack::error(6, None, &ErrorCode::ExitBlocked, "выход завален");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.world_error.as_deref(), Some("выход завален"));
    }

    #[test]
    fn examine_ack_sets_entity_detail_with_loot() {
        let mut store = in_game();
        store.on_world_sent(Act::Examine);
        let ack = ld2_protocol::Ack::ok_with_payload(
            7,
            None,
            &serde_json::json!({
                "entity": fixtures::forest().entities[2], // cor_9 — труп игрока
                "loot": [ { "uid": "loot:cor_9:0", "type": "weapon", "name": "Кинжал" } ],
                "was_player": true
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let detail = store.entity_detail.as_ref().expect("осмотр");
        assert_eq!(
            detail["was_player"], true,
            "⚠ труп игрока виден при осмотре"
        );
        assert!(detail["loot"].as_array().expect("лут").len() == 1);
        assert!(store.pending.is_none());
    }

    /// M23: ack `entity.info` для окна подятия кладётся в `pickup` и
    /// НЕ перетирает модалку осмотра (`entity_detail`) — окна независимы.
    #[test]
    fn pickup_ack_sets_pickup_not_examine() {
        let mut store = in_game();
        store.entity_detail = Some(serde_json::json!({"entity": {"name": "осмотр"}}));
        store.on_world_sent(Act::PickupInfo);
        let ack = ld2_protocol::Ack::ok_with_payload(
            9,
            None,
            &serde_json::json!({
                "entity": {"id": "cor_1", "name": "Волк", "kind": "corpse"},
                "loot": [ { "uid": "loot:cor_1:0", "type": "hide", "name": "Шкура" } ],
                "inner": [],
                "was_player": false,
                "looted": false
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let pickup = store.pickup.as_ref().expect("окно подятия");
        assert_eq!(pickup["entity"]["id"], "cor_1", "лут трупа");
        assert_eq!(
            store.entity_detail.as_ref().expect("осмотр")["entity"]["name"],
            "осмотр",
            "модалка осмотра не тронута"
        );
        assert!(store.pending.is_none());
        // Ошибка (`E_TARGET_GONE`) — в мир, данные очищаются при
        // смене локации/выходе (как у entity_detail).
        store.pickup = None;
        assert!(store.pickup.is_none());
    }

    /// M18/F35: ack `npc.talk` открывает окно диалога (реплики + квесты);
    /// `evt/quest/updated` с невидимой фазой убирает квест из списка окна.
    #[test]
    fn npc_talk_ack_opens_dialog_and_prunes_quest() {
        let mut store = in_game();
        store.on_world_sent(Act::NpcTalk);
        let ack = ld2_protocol::Ack::ok_with_payload(
            8,
            None,
            &serde_json::json!({
                "dialog": {
                    "id": "npc_elder", "name": "Староста"
                },
                "node": {
                    "id": "l_hello", "text": "Как дела?",
                    "replies": [
                        { "id": "r_work", "text": "Какая работа?", "wait_s": 1740 },
                        { "id": "r_shop", "text": "Покажи свои товары.", "action": "shop" },
                        { "id": "r_bye", "text": "Пока." }
                    ]
                },
                "quests": [ { "id": "q1", "title": "Волчья беда", "state": "available" } ],
                "actions": [ "quests", "train" ],
                "training": [
                    { "skill": "sword", "points": 10, "gold": 100 },
                    { "skill": "fire", "points": 10, "gold": 50,
                      "items": [ { "def_id": "herb", "qty": 1 } ] }
                ]
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let dialog = store.npc_dialog.as_ref().expect("диалог открыт");
        assert_eq!(dialog.name, "Староста");
        // Лента: корневой узел (`greeting` убран — ADR-008).
        assert_eq!(dialog.transcript.len(), 1, "лента: {dialog:?}");
        assert!(!dialog.transcript[0].me && dialog.transcript[0].text == "Как дела?");
        let node = dialog.node.as_ref().expect("узел");
        assert_eq!(node.id, "l_hello");
        assert_eq!(node.replies.len(), 3);
        // M18/F42: `wait_s` парсится — реплика на кулдауне неактивна
        // (ADR-008), без поля — активна.
        assert_eq!(node.replies[0].wait_s, Some(1740), "таймер кулдауна");
        assert_eq!(node.replies[1].wait_s, None, "активная реплика");
        // M18/F43: `action` парсится — реплика откроет магазин.
        assert_eq!(
            node.replies[1].action.as_deref(),
            Some("shop"),
            "действие реплики"
        );
        assert_eq!(node.replies[2].action, None, "у обычной ответа нет");
        // Живой отсчёт: абсолютный дедлайн ≈ now + 1740 с.
        let until = node.replies[0]
            .wait_until_ms
            .expect("дедлайн кулдауна ставится");
        let expected = local_epoch_ms() + 1740 * 1000;
        assert!(
            (until - expected).abs() < 2_000,
            "дедлайн {until} ≈ {expected}"
        );
        assert_eq!(
            node.replies[2].wait_until_ms, None,
            "у активной реплики дедлайна нет"
        );
        assert_eq!(dialog.quests.len(), 1);
        assert_eq!(dialog.actions, vec!["quests", "train"], "actions из ack");
        // M18/F44: обучение парсится (ADR-009) — навык, очки, плата.
        assert_eq!(dialog.training.len(), 2, "обучение: {dialog:?}");
        let train = &dialog.training[0];
        assert_eq!(train.skill, "sword");
        assert_eq!(train.points, 10);
        assert_eq!(train.gold, 100);
        assert!(train.items.is_empty(), "у sword платы предметами нет");
        let fire = &dialog.training[1];
        assert_eq!(fire.skill, "fire");
        assert_eq!(fire.items.len(), 1, "у fire плата предметом");
        assert_eq!(fire.items[0].def_id, "herb");
        assert_eq!(fire.items[0].qty, 1);
        assert_eq!(store.active_dialog(false, false), None, "попап вне модалок");
        assert!(store.pending.is_none());

        // M18/F37: ответ → новый узел в ленте, квесты освежены.
        store.on_world_sent(Act::NpcReply);
        let ack = ld2_protocol::Ack::ok_with_payload(
            9,
            None,
            &serde_json::json!({
                "node": {
                    "id": "l_errand", "text": "Отнеси весть!", "quest": "q1",
                    "replies": [ { "id": "r_errand_bye", "text": "Хорошо." } ]
                },
                "quests": [ { "id": "q1", "title": "Волчья беда", "state": "available" } ]
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let dialog = store.npc_dialog.as_ref().expect("диалог открыт");
        assert_eq!(dialog.transcript.len(), 2, "лента растёт: {dialog:?}");
        assert_eq!(dialog.transcript[1].text, "Отнеси весть!");
        assert_eq!(
            dialog.node.as_ref().expect("узел").quest.as_deref(),
            Some("q1")
        );

        // Квест взят → из диалога исчезает (и в снапшоте стал active).
        store.apply(evt(
            "evt/quest/updated",
            serde_json::json!({ "quest": { "id": "q1", "title": "Волчья беда", "state": "active" } }),
        ));
        assert!(
            store
                .npc_dialog
                .as_ref()
                .expect("диалог открыт")
                .quests
                .is_empty(),
            "принятый квест убран из окна"
        );

        // M18/F38: конец диалога («Пока») — окно закрывается целиком.
        store.on_world_sent(Act::NpcReply);
        let ack = ld2_protocol::Ack::ok_with_payload(
            10,
            None,
            &serde_json::json!({ "node": serde_json::Value::Null, "quests": [] }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert!(
            store.npc_dialog.is_none(),
            "терминальный ответ закрыл диалог"
        );
        assert_eq!(store.active_dialog(false, false), None);
    }

    /// M18/F41: повторное открытие диалога (`cmd/npc.talk`) начинает
    /// разговор заново — лента с приветствия, открытый магазин и
    /// выбранная валюта сброшены.
    #[test]
    fn npc_talk_reopens_resets_dialog_to_start() {
        let mut store = in_game();
        let talk_ack = |seq: u32| {
            ld2_protocol::Ack::ok_with_payload(
                seq,
                None,
                &serde_json::json!({
                    "dialog": {
                        "id": "npc_elder", "name": "Староста"
                    },
                    "node": {
                        "id": "l_hello", "text": "Как дела?",
                        "replies": [ { "id": "r_bye", "text": "Пока." } ]
                    },
                    "quests": [],
                    "actions": [ "shop" ]
                }),
            )
            .unwrap()
        };

        // Первое открытие: корневой узел.
        store.on_world_sent(Act::NpcTalk);
        store.apply(IngressEvent::Ack(talk_ack(20)));
        // Разговор пошёл дальше: ответ → вторая строка ленты.
        store.on_world_sent(Act::NpcReply);
        store.apply(IngressEvent::Ack(
            ld2_protocol::Ack::ok_with_payload(
                21,
                None,
                &serde_json::json!({
                    "node": { "id": "l_errand", "text": "Отнеси весть!",
                              "replies": [ { "id": "r_done", "text": "Хорошо." } ] },
                    "quests": []
                }),
            )
            .unwrap(),
        ));
        // И магазин открыт, и валюта выбрана.
        store.on_world_sent(Act::ShopList);
        store.apply(IngressEvent::Ack(
            ld2_protocol::Ack::ok_with_payload(
                22,
                None,
                &serde_json::json!({
                    "shop": { "tabs": [
                        { "currency": "gold_coin", "currency_name": "Золотая монета",
                          "items": [] },
                        { "currency": "gem_str", "currency_name": "Самоцвет силы",
                          "items": [] }
                    ]}
                }),
            )
            .unwrap(),
        ));
        {
            let dialog = store.npc_dialog.as_mut().expect("диалог открыт");
            dialog.shop_currency = Some("gem_str".into());
            assert_eq!(dialog.transcript.len(), 2, "лента разговора");
            assert!(dialog.shop.is_some(), "магазин открыт");
        }

        // Повторное открытие — всё с начала.
        store.on_world_sent(Act::NpcTalk);
        store.apply(IngressEvent::Ack(talk_ack(23)));
        let dialog = store.npc_dialog.as_ref().expect("диалог открыт");
        assert_eq!(dialog.transcript.len(), 1, "лента заново: {dialog:?}");
        assert_eq!(dialog.transcript[0].text, "Как дела?", "с корневой реплики");
        assert_eq!(dialog.node.as_ref().expect("корень").id, "l_hello");
        assert!(dialog.shop.is_none(), "магазин закрыт");
        assert!(dialog.shop_currency.is_none(), "валюта сброшена");
        assert_eq!(dialog.actions, vec!["shop"], "actions нового ack");
    }

    /// M18/F39: ack `shop.list` открывает панель магазина внутри окна
    /// диалога (активная вкладка — первая).
    #[test]
    fn shop_list_ack_opens_shop_panel() {
        let mut store = in_game();
        let talk = serde_json::json!({
            "dialog": { "id": "npc_elder", "name": "Староста" },
            "node": serde_json::Value::Null,
            "quests": []
        });
        store.npc_dialog = NpcDialogView::from_payload(&talk);
        store.on_world_sent(Act::ShopList);
        let ack = ld2_protocol::Ack::ok_with_payload(
            12,
            None,
            &serde_json::json!({
                "shop": { "tabs": [
                    { "currency": "gold_coin", "currency_name": "Золотая монета",
                      "items": [ { "def_id": "potion_heal", "name": "Зелье лечения",
                                   "rarity": "common", "price": 28, "qty": 1,
                                   "item": { "uid": "shop:potion_heal", "type": "potion",
                                             "name": "Зелье лечения", "rarity": "common",
                                             "upgrade": 0, "stats": {}, "sockets": [],
                                             "qty": 1 } } ] },
                    { "currency": "gem_str", "currency_name": "Самоцвет силы", "items": [] }
                ]}
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let dialog = store.npc_dialog.as_ref().expect("диалог открыт");
        let shop = dialog.shop.as_ref().expect("магазин открыт");
        assert_eq!(shop.tabs.len(), 2);
        assert_eq!(
            dialog.shop_currency.as_deref(),
            Some("gold_coin"),
            "активна первая вкладка"
        );
        assert_eq!(shop.tabs[0].items[0].price, 28);
        // M18/F40: снапшот парсится — карточка по клику строки.
        let snapshot = shop.tabs[0].items[0].item.as_ref().expect("снапшот товара");
        assert_eq!(snapshot.name, "Зелье лечения");
        assert_eq!(snapshot.item_type, "potion");
        // Закрытие диалога чистит магазин (поля внутри вида).
        store.npc_dialog = None;
        assert!(store.npc_dialog.is_none());
    }

    #[test]
    fn corpse_looted_was_player_logs_crime() {
        let mut store = in_game();
        store.apply(evt(
            "evt/corpse/looted",
            serde_json::json!({
                "corpse_id": "cor_9", "by": "c1", "by_name": "Марк", "was_player": true
            }),
        ));
        let last = store.log.back().expect("лог");
        assert_eq!(last.kind, LogKind::Crime);
        assert!(last.text.contains("Мародёр"), "{}", last.text);
    }

    // ─── M4: лист, инвентарь, смерть ────────────────────────────────────────

    #[test]
    fn death_sets_modal_and_confirm_clears_it() {
        let mut store = in_game();
        store.apply(evt(
            "evt/char/death",
            serde_json::json!({
                "victim_char": true,
                "lost_items": [
                    { "uid": "i_17", "name": "Ясеневый посох", "rarity": "rare" },
                    { "uid": "s_1", "name": "Свиток огненного шара", "rarity": "common" }
                ],
                "corpse": "cor_self",
                "respawn": { "location_id": "l_village", "delay_s": 0 }
            }),
        ));
        let death = store.death.as_ref().expect("модалка смерти");
        assert_eq!(death.lost_items.len(), 2, "сводка утраченного (ФР-06)");
        // M18: строки утраты в журнале больше нет — только модалка.
        assert!(
            store.log.iter().all(|e| !e.text.contains("ВЫ ПОГИБЛИ")),
            "в журнале нет строки о гибели"
        );

        // Закрыть можно только подтверждением.
        store.confirm_death();
        assert!(store.death.is_none());
    }

    #[test]
    fn item_removed_clears_inventory_and_equip_slot() {
        let mut store = in_game();
        assert!(store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .any(|i| i.uid == "i_17"));
        assert_eq!(
            store.snapshot.as_ref().unwrap().char.equip.get("main_hand"),
            Some(&Some("i_17".to_string()))
        );

        store.apply(evt(
            "evt/item/removed",
            serde_json::json!({ "uid": "i_17", "reason": "lost_on_death" }),
        ));
        let snap = store.snapshot.as_ref().unwrap();
        assert!(
            !snap.char.inventory.iter().any(|i| i.uid == "i_17"),
            "предмет убран из инвентаря"
        );
        assert!(
            snap.char.equip.get("main_hand").is_none_or(|v| v.is_none()),
            "слот экипировки освобождён"
        );
        // Заказчик: в журнале — ИМЯ предмета, не id.
        let last = store.log.back().expect("строка журнала");
        assert!(
            last.text.contains("Потеряно: Ясеневый посох"),
            "имя предмета в журнале: {}",
            last.text
        );
        assert!(
            !last.text.contains("i_17"),
            "id не должен попадать в журнал: {}",
            last.text
        );
    }

    #[test]
    fn dropped_log_is_uniform_for_everyone() {
        let mut store = in_game();
        // ЧУЖОЙ выброс — «{имя} выбросил …» (не «Вы выбросили»).
        store.apply(evt(
            "evt/item/dropped",
            serde_json::json!({
                "char_id": "other-char",
                "by_name": "Ким",
                "gender": "male",
                "item_name": "Зелье лечения",
                "qty": 2,
                "sent_at": "2026-11-21T00:00:00Z"
            }),
        ));
        let last = store.log.back().expect("строка журнала");
        assert_eq!(last.text, "Ким выбросил 2 Зелье лечения", "{}", last.text);

        // Свой выброс — тот же формат (по имени, заказчик).
        store.apply(evt(
            "evt/item/dropped",
            serde_json::json!({
                "char_id": "c1",
                "by_name": "Ким",
                "gender": "male",
                "item_name": "Зелье лечения",
                "qty": 1,
                "sent_at": "2026-11-21T00:00:00Z"
            }),
        ));
        let last = store.log.back().expect("строка журнала");
        assert_eq!(last.text, "Ким выбросил 1 Зелье лечения", "{}", last.text);
    }

    #[test]
    fn item_added_appends_to_inventory() {
        let mut store = in_game();
        store.apply(evt(
            "evt/item/added",
            serde_json::json!({
                "item": { "uid": "loot_m_3", "type": "material", "name": "Трофея: Волк",
                          "rarity": "common", "upgrade": 0, "stats": {}, "qty": 1 }
            }),
        ));
        assert!(store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .any(|i| i.uid == "loot_m_3"));
        assert_eq!(store.log.back().unwrap().kind, LogKind::Loot);
    }

    #[test]
    fn char_stats_patch_applies_hp_and_mp() {
        let mut store = in_game();
        store.apply(evt(
            "evt/char/stats",
            serde_json::json!({ "patch": { "hp": { "cur": 60, "max": 60 },
                                           "mp": { "cur": 60, "max": 60 } } }),
        ));
        let snap = store.snapshot.as_ref().unwrap();
        assert_eq!(snap.char.hp.cur, 60, "hp после респауна");
        assert_eq!(snap.char.mp.cur, 60, "mp после респауна");
    }

    #[test]
    fn equip_ack_replaces_character_state() {
        let mut store = in_game();
        let mut ch = fixtures::character("c1", "Ким");
        ch.equip.insert("body".into(), Some("loot_m_3".into()));
        store.on_sheet_sent(Act::Equip);
        let ack = ld2_protocol::Ack::ok_with_payload(
            11,
            Some(1010),
            &serde_json::json!({ "char": ch, "item": serde_json::Value::Null }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(
            store.snapshot.as_ref().unwrap().char.equip.get("body"),
            Some(&Some("loot_m_3".to_string())),
            "экипировка применена из ack (нет событий на смену слота)"
        );
        assert!(store.notice.is_none() && store.pending.is_none());
    }

    #[test]
    fn equip_ack_error_goes_to_notice() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Equip);
        let ack = ld2_protocol::Ack::error(12, None, &ErrorCode::ItemGone, "предмета нет");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.notice.as_deref(), Some("предмета нет"));
        assert!(store.world_error.is_none(), "ошибки листа не в мире");
    }

    #[test]
    fn invest_ack_and_xp_event_update_pool_and_skill() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Invest);
        let ack = ld2_protocol::Ack::ok_with_payload(
            13,
            Some(1011),
            &serde_json::json!({ "pool_left": 70 }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.snapshot.as_ref().unwrap().char.xp_pool, 70);

        store.apply(evt(
            "evt/char/xp",
            serde_json::json!({
                "skill_id": "sword", "amount": 50, "skill_xp": 662,
                "rank": 3, "xp_to_next": 480, "pool_left": 70
            }),
        ));
        let snap = store.snapshot.as_ref().unwrap();
        let sword = snap.char.skills.iter().find(|s| s.id == "sword").unwrap();
        assert_eq!(sword.xp, 662, "прогресс навыка обновлён (ФР-04)");
        assert_eq!(snap.char.xp_pool, 70);
        assert_eq!(store.log.back().unwrap().kind, LogKind::Xp);
    }

    // ─── M5: улучшение, инкрустация, патчи предметов ────────────────────────

    #[test]
    fn upgrade_lifecycle_open_preview_result() {
        let mut store = in_game();
        store.open_upgrade("i_17".into(), "Ясеневый посох".into());
        let view = store.upgrade.as_ref().expect("модалка открыта");
        assert!(view.preview.is_none(), "превью ещё не пришло");

        // ack превью заполняет view.
        store.on_sheet_sent(Act::Preview);
        let ack = ld2_protocol::Ack::ok_with_payload(
            21,
            None,
            &serde_json::json!({
                "preview_id": "pv_7",
                "delta": { "upgrade": "+3" },
                "cost": { "gold": 300, "materials": { "rune_dust": 2 } },
                "chance": 0.75
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let view = store.upgrade.as_ref().expect("view");
        assert_eq!(view.item_uid, "i_17");
        assert_eq!(view.preview.as_ref().unwrap()["preview_id"], "pv_7");

        // Результат остаётся в модалке (не закрывается) и пишет лог.
        store.apply(evt(
            "evt/upgrade/result",
            serde_json::json!({
                "uid": "i_17", "ok": true, "new_upgrade": 3,
                "item_destroyed": false, "cost": { "gold": 300 }
            }),
        ));
        assert!(store.upgrade.is_some(), "модалка остаётся открытой");
        assert!(
            store.upgrade.as_ref().unwrap().result.is_some(),
            "результат сохранён"
        );
        let last = store.log.back().expect("лог");
        assert_eq!(last.kind, LogKind::Loot);
        assert!(last.text.contains("+3"), "{}", last.text);
    }

    /// M18/F54: ack выбора камня заполняет активный камень (uid/имя из
    /// снапшота/качество); удаление камня снимает выбор и гасит превью,
    /// чтобы не стартовать по неактуальному `preview_id`.
    /// M18/F55: итог заточки переживает автопревью — после результата
    /// запрашивается новое превью (для повторного улучшения), и итог
    /// обязан остаться на экране зелёным/красным, а не исчезнуть.
    #[test]
    fn sharpen_result_survives_new_preview() {
        let mut store = in_game();
        store.open_upgrade("i_17".into(), "Ясеневый посох".into());
        let ack_preview = |n: u32| {
            ld2_protocol::Ack::ok_with_payload(
                n,
                None,
                &serde_json::json!({
                    "preview_id": format!("pv_{n}"),
                    "delta": { "upgrade": "+3" },
                    "cost": { "gold": 100 }
                }),
            )
            .unwrap()
        };

        // Первое превью → результат.
        store.on_sheet_sent(Act::Preview);
        store.apply(IngressEvent::Ack(ack_preview(41)));
        store.apply(evt(
            "evt/upgrade/result",
            serde_json::json!({
                "uid": "i_17", "ok": true, "new_upgrade": 3,
                "item_destroyed": false, "cost": { "gold": 100 }
            }),
        ));
        assert!(
            store.upgrade.as_ref().unwrap().result.is_some(),
            "итог есть"
        );
        assert!(
            store.upgrade.as_ref().unwrap().preview.is_none(),
            "превью сброшено под автопревью"
        );

        // Автопревью после результата (M18/F55): итог обязан пережить.
        store.on_sheet_sent(Act::Preview);
        store.apply(IngressEvent::Ack(ack_preview(42)));

        let view = store.upgrade.as_ref().expect("окно открыто");
        assert!(
            view.preview.is_some(),
            "новое превью принято — повторная заточка доступна"
        );
        assert!(
            view.result.is_some(),
            "итог прошлого броска не затёрт новым превью"
        );
    }

    /// M18/F55: окно заточки переживает расход последнего камня —
    /// иначе «назад» (гейт был `stone || upgrade`) закрывал окно вовсе
    /// (жалоба M18/F55). Гейт — отдельный флаг `sharpen_list`.
    #[test]
    fn sharpen_window_outlives_spent_stone() {
        let mut store = in_game();
        assert!(!store.sharpen_list, "окно по умолчанию закрыто");

        // Выбор камня открывает окно (экран списка).
        store.on_sheet_sent(Act::Sharpen);
        let ack = ld2_protocol::Ack::ok_with_payload(
            51,
            None,
            &serde_json::json!({ "stone_uid": "i_17", "quality": 5 }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert!(store.sharpen_list, "окно открыто выбором камня");

        // Последний камень потрачен → сервер снимает выбор...
        store.apply(evt(
            "evt/item/removed",
            serde_json::json!({ "uid": "i_17", "reason": "consumed" }),
        ));
        assert!(store.sharpen_stone.is_none(), "камень снят");
        assert!(
            store.sharpen_list,
            "окно обязано пережить расход камня — иначе «назад» его закроет"
        );

        // «Назад» к списку закрывает только карточку предмета.
        store.open_upgrade("i_17".into(), "Ясеневый посох".into());
        store.close_upgrade();
        assert!(store.upgrade.is_none(), "карточка предмета закрыта");
        assert!(store.sharpen_list, "список доступен — окно живо");

        // Закрытие окна (крестик/Esc/скрим) — флаг снимается.
        store.sharpen_list = false;
        assert!(!store.sharpen_list, "окно закрыто");
    }

    #[test]
    fn sharpen_stone_set_by_ack_and_cleared_on_removed() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Sharpen);
        let ack = ld2_protocol::Ack::ok_with_payload(
            31,
            None,
            &serde_json::json!({ "stone_uid": "i_17", "quality": 5 }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));

        let stone = store.sharpen_stone.as_ref().expect("камень выбран");
        assert_eq!(stone.uid, "i_17");
        assert_eq!(stone.quality, 5, "качество из ack");
        assert_eq!(stone.name, "Ясеневый посох", "имя из снапшота");

        // Превью посчитано с камнем — запоминаем, что оно есть.
        store.open_upgrade("i_17".into(), "Ясеневый посох".into());
        store.on_sheet_sent(Act::Preview);
        let ack = ld2_protocol::Ack::ok_with_payload(
            32,
            None,
            &serde_json::json!({ "preview_id": "pv_1", "delta": { "upgrade": "+3" } }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert!(store.upgrade.as_ref().unwrap().preview.is_some());

        // Камень выбыл → выбор снят, превью погашено (старт был бы
        // неверным шагом без нового камня).
        store.apply(evt(
            "evt/item/removed",
            serde_json::json!({ "uid": "i_17", "reason": "consumed" }),
        ));
        assert!(
            store.sharpen_stone.is_none(),
            "выбор камня снят по удалению"
        );
        assert!(
            store.upgrade.as_ref().unwrap().preview.is_none(),
            "превью погашено вместе с камнем"
        );
        assert!(store.upgrade.is_some(), "окно заточки само не закрывается");
    }

    #[test]
    fn upgrade_failure_destroys_and_logs_crime() {
        let mut store = in_game();
        store.open_upgrade("i_17".into(), "Ясеневый посох".into());
        store.apply(evt(
            "evt/upgrade/result",
            serde_json::json!({
                "uid": "i_17", "ok": false, "item_destroyed": true, "cost": {}
            }),
        ));
        assert!(store.upgrade.is_some());
        let last = store.log.back().expect("лог");
        assert_eq!(
            last.kind,
            LogKind::Crime,
            "разрушение — в журнал преступлений/потерь"
        );
        assert!(last.text.contains("РАЗРУШЕН"), "{}", last.text);
    }

    #[test]
    fn item_state_patch_updates_upgrade_and_sockets() {
        let mut store = in_game();
        // Патч уровня (+N).
        store.apply(evt(
            "evt/item/state",
            serde_json::json!({ "uid": "i_17", "patch": { "upgrade": 3 } }),
        ));
        let item = store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .find(|i| i.uid == "i_17")
            .unwrap();
        assert_eq!(item.upgrade, 3, "уровень обновлён");

        // Патч целым предметом (сокеты после инкрустации).
        let mut full = fixtures::staff_item();
        full.sockets.as_mut().unwrap()[1].rune = Some(ld2_protocol::types::RuneInfo {
            class: None,
            damage: None,
            def_id: None,
            name: "Руна льда".into(),
            spell: "ice_shard".into(),
            charges: 5,
            max: 5,
            glyph: None,
            detail: None,
            rarity: None,
        });
        store.apply(evt(
            "evt/item/state",
            serde_json::json!({ "uid": "i_17", "patch": { "item": full } }),
        ));
        let item = store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .inventory
            .iter()
            .find(|i| i.uid == "i_17")
            .unwrap();
        assert_eq!(
            item.sockets.as_ref().unwrap()[1]
                .rune
                .as_ref()
                .unwrap()
                .name,
            "Руна льда",
            "сокет применён"
        );
    }

    #[test]
    fn socket_ack_applies_staff_and_rune() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Socket);
        // Ack извлечения: посох обновлён + руна добавлена в инвентарь.
        let mut staff = fixtures::staff_item();
        staff.sockets.as_mut().unwrap()[0].rune = None;
        let rune = fixtures::rune_item("rune_i_17_0");
        let ack = ld2_protocol::Ack::ok_with_payload(
            22,
            Some(1020),
            &serde_json::json!({ "staff": staff, "rune": rune }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let snap = store.snapshot.as_ref().unwrap();
        let staff = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == "i_17")
            .unwrap();
        assert!(
            staff.sockets.as_ref().unwrap()[0].rune.is_none(),
            "сокет пуст"
        );
        assert!(
            snap.char.inventory.iter().any(|i| i.uid == "rune_i_17_0"),
            "руна в инвентаре"
        );
        assert!(store.notice.is_none());
    }

    /// M18/F32: ack с `char` (цель была надета) применяет атрибуты —
    /// бонус самоцвета виден сразу, без переодевания.
    #[test]
    fn socket_ack_with_char_applies_attrs() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Socket);
        let mut ch = fixtures::character("c1", "Ким");
        ch.attrs.insert("str".into(), 13);
        let ack = ld2_protocol::Ack::ok_with_payload(
            23,
            Some(1021),
            &serde_json::json!({ "staff": fixtures::staff_item(), "char": ch }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        let snap = store.snapshot.as_ref().unwrap();
        assert_eq!(
            snap.char.attrs.get("str"),
            Some(&13i64),
            "бонус самоцвета применён из `char` ack"
        );
        assert!(store.notice.is_none());
    }

    #[test]
    fn cast_error_goes_to_world_error_not_sheet() {
        let mut store = in_game();
        store.on_world_sent(Act::Cast);
        let ack = ld2_protocol::Ack::error(23, None, &ErrorCode::NotEnoughMp, "не хватает маны");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.world_error.as_deref(), Some("не хватает маны"));
        assert!(store.notice.is_none(), "каст — не лист");
    }

    // ─── M6: время, цель, кулдауны, статусы ─────────────────────────────────

    fn delta_event(from_version: u64, name: &str) -> IngressEvent {
        IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/state/delta".into(),
            seq: None,
            ts: None,
            state_version: Some(from_version + 1),
            payload: Some(serde_json::json!({
                "from_version": from_version,
                "to_version": from_version + 1,
                "patch": [
                    { "op": "replace", "path": "/location/name", "value": name }
                ]
            })),
        })
    }

    #[test]
    fn state_delta_applies_patch() {
        let mut store = store_with_session();
        store.apply(snapshot_event());
        assert!(store.snapshot.is_some());

        let effects = store.apply(delta_event(1002, "Тестовое имя"));
        assert!(effects.is_empty(), "патч применён без resync: {effects:?}");
        let snap = store.snapshot.as_ref().expect("снапшот");
        assert_eq!(snap.location.name, "Тестовое имя");
    }

    #[test]
    fn state_delta_mismatch_requests_resync() {
        let mut store = store_with_session();
        store.apply(snapshot_event());

        // Чужая база (версия снапшота 1002) → авто-resync.
        let effects = store.apply(delta_event(999, "Чужая"));
        assert!(effects.iter().any(|e| matches!(e, Effect::ResyncState)));

        // Неприменимый путь (replace на отсутствующий ключ) → авто-resync.
        let bad = IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/state/delta".into(),
            seq: None,
            ts: None,
            state_version: Some(1003),
            payload: Some(serde_json::json!({
                "from_version": 1002,
                "to_version": 1003,
                "patch": [
                    { "op": "replace", "path": "/location/no_such_key", "value": "x" }
                ]
            })),
        });
        let effects = store.apply(bad);
        assert!(effects.iter().any(|e| matches!(e, Effect::ResyncState)));
    }

    #[test]
    fn evt_error_sets_notice_and_fatal_requests_resync() {
        let mut store = store_with_session();
        let err = |fatal: bool| {
            IngressEvent::Event(ld2_protocol::Envelope {
                v: 1,
                msg_type: "evt/error".into(),
                seq: None,
                ts: None,
                state_version: None,
                payload: Some(serde_json::json!({
                    "code": "E_MAINTENANCE",
                    "message": "перезагрузка",
                    "fatal": fatal
                })),
            })
        };
        let effects = store.apply(err(false));
        assert!(store.notice.is_some(), "notice показан");
        assert!(effects.is_empty(), "не-фатальная — без resync");

        let effects = store.apply(err(true));
        assert!(store.notice.is_some());
        assert!(effects.iter().any(|e| matches!(e, Effect::ResyncState)));
    }

    #[test]
    fn time_sync_takes_median_of_ring() {
        let mut store = store_with_session();
        let local = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Медиана пяти замеров {1080, 1090, 1100, 1110, 1300} = 1100.
        for delta in [1100, 1090, 1110, 1080, 1300] {
            let sync = IngressEvent::Event(ld2_protocol::Envelope {
                v: 1,
                msg_type: "evt/time.sync".into(),
                seq: None,
                ts: None,
                state_version: None,
                payload: Some(serde_json::json!({ "server_ts": local + delta })),
            });
            store.apply(sync);
        }
        assert!(
            (1095..=1105).contains(&store.time_offset),
            "медиана кольца: {}",
            store.time_offset
        );
    }

    fn chat_event(channel: &str) -> IngressEvent {
        IngressEvent::Event(ld2_protocol::Envelope {
            v: 1,
            msg_type: "evt/chat/message".into(),
            seq: None,
            ts: None,
            state_version: None,
            payload: Some(serde_json::json!({
                "channel": channel,
                "from": "Ким",
                "name": "Ким",
                "text": "привет",
                "sent_at": "2026-11-20T00:00:00Z"
            })),
        })
    }

    #[test]
    fn chat_log_localizes_channel_and_colors_by_kind() {
        let mut store = store_with_session();
        // en: [Location], не [location].
        store.lang = Lang::En;
        store.apply(chat_event("location"));
        let last = store.log.back().expect("строка журнала");
        assert!(
            last.text.contains("[Location] Ким: привет"),
            "{}",
            last.text
        );
        assert!(matches!(last.kind, LogKind::Chat), "{:?}", last.kind);

        // Партнёрский канал — своя метка и свой цвет.
        store.apply(chat_event("party"));
        let last = store.log.back().expect("строка журнала");
        assert!(last.text.contains("[Party]"), "{}", last.text);
        assert!(matches!(last.kind, LogKind::ChatParty), "{:?}", last.kind);

        // ru: [Гильдия].
        store.lang = Lang::Ru;
        store.apply(chat_event("guild"));
        let last = store.log.back().expect("строка журнала");
        assert!(last.text.contains("[Гильдия]"), "{}", last.text);
        assert!(matches!(last.kind, LogKind::ChatGuild), "{:?}", last.kind);
    }

    #[test]
    fn time_sync_calibrates_offset() {
        let mut store = in_game();
        let server_ts = local_epoch() + 1000;
        store.apply(evt(
            "evt/time.sync",
            serde_json::json!({ "server_ts": server_ts, "rtt_hint_ms": 1 }),
        ));
        assert!(
            (990..=1010).contains(&store.time_offset),
            "offset ≈ +1000: {}",
            store.time_offset
        );
    }

    #[test]
    fn target_cycle_in_forest_and_autoclear() {
        let mut store = in_game(); // деревня: только NPC — монстров нет
        store.cycle_target();
        assert!(store.target.is_none(), "в деревне монстров нет");

        let payload = serde_json::to_value(LocationChanged {
            location: fixtures::forest(),
        })
        .unwrap();
        store.apply(evt("evt/location/changed", payload));

        store.select_target("p_77");
        assert_eq!(store.target.as_deref(), Some("p_77"));

        store.cycle_target(); // монстр → цель
        assert_eq!(store.target.as_deref(), Some("m_3"));
        store.cycle_target(); // один монстр → wrap на себя
        assert_eq!(store.target.as_deref(), Some("m_3"));

        // Гибель цели → авто-сброс (ФР-05).
        store.apply(evt(
            "evt/entity/despawn",
            serde_json::json!({ "entity_id": "m_3", "location_id": "l_forest" }),
        ));
        assert!(store.target.is_none(), "цель исчезла → сброс");
    }

    #[test]
    fn combat_cooldown_recorded() {
        let mut store = in_game();
        store.apply(evt(
            "evt/combat/cooldown",
            serde_json::json!({ "skill_id": "power_strike", "until_epoch": 123 }),
        ));
        assert_eq!(store.combat_cooldowns.get("power_strike"), Some(&123));
    }

    /// M17/F30g: бар кулдауна предмета — остаток и полная
    /// длительность; истёкшая запись и чужой предмет — None.
    #[test]
    fn item_cd_bar_reports_left_and_total() {
        let mut store = in_game();
        store.apply(evt(
            "evt/combat/cooldown",
            serde_json::json!({
                "skill_id": "item:potion_heal",
                "until_epoch": 123,
                "duration_ms": 1000
            }),
        ));
        assert_eq!(
            store.item_cd_bar("potion_heal", 122_000),
            Some((1_000, 1_000)),
            "остаток и полная длительность"
        );
        assert_eq!(store.item_cd_bar("potion_heal", 124_000), None, "истёк");
        assert_eq!(store.item_cd_bar("rune_fire", 122_000), None, "нет записи");
        // Без duration (старый сервер) — fallback в combat_cooldowns.
        store.apply(evt(
            "evt/combat/cooldown",
            serde_json::json!({ "skill_id": "item:scroll_frost", "until_epoch": 200 }),
        ));
        assert_eq!(
            store.item_cd_bar("scroll_frost", 100_000),
            None,
            "без duration — бара нет"
        );
    }

    #[test]
    fn char_buffs_full_replacement() {
        let mut store = in_game();
        // M18/F58d: полная замена списка баффов (паттерн evt/crime/status).
        let expires = local_epoch() + 30;
        store.apply(evt(
            "evt/char/buffs",
            serde_json::json!({
                "buffs": [
                    { "kind": "rune_marksman", "title": "Меткость",
                      "stacks": 2, "expires_at": expires }
                ]
            }),
        ));
        let buffs = &store.snapshot.as_ref().unwrap().char.buffs;
        assert_eq!(buffs.len(), 1);
        assert_eq!(buffs[0].kind, "rune_marksman");
        assert_eq!(buffs[0].stacks, 2);
        assert_eq!(buffs[0].expires_at, expires, "срок сервера не изменён");

        // Пустой список — снятие всех баффов.
        store.apply(evt("evt/char/buffs", serde_json::json!({ "buffs": [] })));
        assert!(
            store.snapshot.as_ref().unwrap().char.buffs.is_empty(),
            "пустая замена снимает баффы"
        );
    }

    #[test]
    fn crime_statuses_replace_add_remove_without_recalculation() {
        let mut store = in_game();
        // Полная замена (§8): клиент хранит как есть, без пересчёта blend (§4.10).
        let server_expiry = local_epoch() + 5000;
        store.apply(evt(
            "evt/crime/status",
            serde_json::json!({
                "statuses": [
                    { "kind": "murderer", "severity": "high", "expires_at": server_expiry },
                    { "kind": "marauder", "severity": "medium", "expires_at": local_epoch() + 2000 }
                ],
                "log_hint": null
            }),
        ));
        let statuses = &store.snapshot.as_ref().unwrap().char.crime.statuses;
        assert_eq!(statuses.len(), 2);
        assert_eq!(
            statuses[0].expires_at, server_expiry,
            "срок сервера не изменён клиентом"
        );

        // Добавление: журнал дополняется, бейдж обновляется по kind.
        store.apply(evt(
            "evt/crime/added",
            serde_json::json!({
                "status": { "kind": "thief", "severity": "medium", "expires_at": local_epoch() + 1000 },
                "log_entry": { "kind": "thief", "at": local_epoch(), "delta_s": 1000, "total_s": 8000 }
            }),
        ));
        assert_eq!(
            store.snapshot.as_ref().unwrap().char.crime.statuses.len(),
            3
        );
        assert_eq!(store.crime_log.len(), 1, "журнал дополнен");

        // Снятие статуса.
        store.apply(evt(
            "evt/crime/removed",
            serde_json::json!({ "kind": "marauder" }),
        ));
        let kinds: Vec<String> = store
            .snapshot
            .as_ref()
            .unwrap()
            .char
            .crime
            .statuses
            .iter()
            .map(|s| s.kind.clone())
            .collect();
        assert_eq!(kinds, vec!["murderer".to_string(), "thief".to_string()]);
    }

    #[test]
    fn pvp_entry_with_active_status_notices() {
        let mut store = in_game();
        // Активный статус (серверный срок в будущем).
        store.apply(evt(
            "evt/crime/status",
            serde_json::json!({
                "statuses": [{ "kind": "murderer", "severity": "high",
                               "expires_at": local_epoch() + 5000 }]
            }),
        ));

        let payload = serde_json::to_value(LocationChanged {
            location: fixtures::forest(), // flags: ["pvp"]
        })
        .unwrap();
        store.apply(evt("evt/location/changed", payload));
        let notice = store.notice.clone().expect("предупреждение о PvP");
        assert!(notice.contains("PvP"), "{notice}");
        assert!(notice.contains("murderer"), "{notice}");

        // Без статусов (после снятия) — тоста нет.
        store.apply(evt(
            "evt/crime/removed",
            serde_json::json!({ "kind": "murderer" }),
        ));
        store.apply(evt(
            "evt/location/changed",
            serde_json::to_value(LocationChanged {
                location: fixtures::village(),
            })
            .unwrap(),
        ));
        store.apply(evt(
            "evt/location/changed",
            serde_json::to_value(LocationChanged {
                location: fixtures::forest(),
            })
            .unwrap(),
        ));
        assert!(store.notice.is_none(), "нет статусов → нет тоста");
    }

    #[test]
    fn crime_log_loaded_from_ack() {
        let mut store = in_game();
        assert!(!store.crime_log_loaded);
        store.on_sheet_sent(Act::CrimeLog);
        let ack = ld2_protocol::Ack::ok_with_payload(
            30,
            None,
            &serde_json::json!({
                "entries": [{ "kind": "marauder", "at": 100, "delta_s": 3471, "total_s": 25071 }]
            }),
        )
        .unwrap();
        store.apply(IngressEvent::Ack(ack));
        assert!(store.crime_log_loaded);
        assert_eq!(store.crime_log.len(), 1);
        assert_eq!(store.crime_log[0].delta_s, 3471);
    }

    /// M25: журнал исчезающий — строки старше TTL удаляются, свежие
    /// остаются; ttl <= 0 — ничего не удаляется.
    #[test]
    fn expire_log_removes_old_entries() {
        let mut store = in_game();
        store.log.clear(); // истечение — от старого конца
        let base = store.log.len();
        store.push_log(LogKind::System, "старая");
        store.log.back_mut().expect("строка").at = local_epoch_ms() - 31_000;
        store.push_log(LogKind::System, "свежая");
        assert!(store.expire_log(local_epoch_ms(), 30), "старая удалена");
        assert_eq!(store.log.len(), base + 1, "свежая осталась");
        assert_eq!(store.log.back().expect("строка").text, "свежая");
        // ttl <= 0 — не удаляем (сервер запретил истечение).
        store.push_log(LogKind::System, "давняя");
        store.log.back_mut().expect("строка").at = local_epoch_ms() - 60_000;
        assert!(
            !store.expire_log(local_epoch_ms(), 0),
            "ttl0 — без истечения"
        );
        assert_eq!(store.log.len(), base + 2, "ничего не удалено");
    }

    #[test]
    fn filter_active_buffs_hides_expired() {
        use ld2_protocol::types::BuffState;
        let now = 1_000;
        let buffs = vec![
            BuffState {
                kind: "rune_marksman".into(),
                title: "Меткость".into(),
                stacks: 2,
                expires_at: now + 30,
                extra: Default::default(),
            },
            BuffState {
                kind: "rune_rage".into(),
                title: "Ярость".into(),
                stacks: 1,
                expires_at: now,
                extra: Default::default(),
            },
        ];
        let active = filter_active_buffs(&buffs, now);
        assert_eq!(active.len(), 1, "просроченный не показывается");
        assert_eq!(active[0].kind, "rune_marksman");
        assert!(filter_active_buffs(&[], now).is_empty());
    }

    #[test]
    fn filter_active_statuses_hides_expired() {
        let now = 1000;
        let statuses = vec![
            CrimeStatus {
                kind: "old".into(),
                severity: ld2_protocol::types::Severity::High,
                expires_at: now - 1,
            },
            CrimeStatus {
                kind: "fresh".into(),
                severity: ld2_protocol::types::Severity::Medium,
                expires_at: now + 60,
            },
        ];
        let active = filter_active_statuses(&statuses, now);
        assert_eq!(active.len(), 1, "истёвший скрыт (ФР-11 fallback)");
        assert_eq!(active[0].kind, "fresh");
        assert_eq!(statuses.len(), 2, "источник не мутирован");
    }

    #[test]
    fn debug_quest_updated_applies_pool() {
        let mut store = in_game();
        store.apply(evt(
            "evt/quest/updated",
            serde_json::json!({
                "quest": {
                    "id": "q_wolves", "title": "Волки", "state": "done",
                    "goals": [ { "text": "Волков убито", "cur": 5, "need": 5 } ],
                    "reward_preview": { "items": [], "xp": 450 }
                },
                "pool_left": 270,
                "award": 150
            }),
        ));
        assert_eq!(
            store.snapshot.as_ref().unwrap().char.xp_pool,
            270,
            "пул из quest/updated"
        );
        assert_eq!(
            store.snapshot.as_ref().unwrap().char.quests[0].state,
            ld2_protocol::types::QuestPhase::Done
        );
    }

    #[test]
    fn log_ring_buffer_capped() {
        let mut store = in_game();
        for i in 0..1005 {
            store.apply(evt(
                "evt/chat/message",
                serde_json::json!({
                    "channel": "location", "from": "p", "name": "Марк",
                    "text": format!("м {i}"), "sent_at": "2026-11-03T00:00:00Z"
                }),
            ));
        }
        assert_eq!(store.log.len(), 1000, "ring-буфер журнала (НФР §8)");
        assert!(store.log.front().expect("старейшая").text.contains("м 5"));
    }
}
