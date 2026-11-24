//! Типы состояния клиента (`AppStore` и вспомогательные) — вынесены
//! из `store.rs` (RF4a). Поведение не менялось: только перенос.

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
