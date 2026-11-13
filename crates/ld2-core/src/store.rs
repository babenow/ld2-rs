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
    EntitySpawn, GuildLogEntry, ItemAdded, ItemRemoved, LocationChanged, MagicCast, StateSnapshot,
    TimeSync as TimeSyncEvent,
};
use ld2_protocol::types::{
    CharacterSummary, CrimeLogEntry, CrimeStatus, GuildState, Hp, LocationSnapshot, PartyState,
    QuestState,
};
use ld2_protocol::{ErrorCode, SessionConfig};

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
}

/// Открытая модалка улучшения (ФР-10): предмет + превью от сервера.
#[derive(Debug, Clone, PartialEq)]
pub struct UpgradeView {
    pub item_uid: String,
    pub item_name: String,
    /// payload `upgrade.preview`: `{preview_id, delta, cost, chance}`.
    pub preview: Option<serde_json::Value>,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub kind: LogKind,
    pub text: String,
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
    /// Открытая инкрустация (ФР-09): uid посоха.
    pub socketing: Option<String>,
    /// M17/F30a: открытый выбор оружия под инкрустацию руны (uid руны).
    pub inscribe: Option<String>,
    /// Смещение часов сервера (evt/time.sync): server = local + offset.
    pub time_offset: i64,
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
    /// Логин последней попытки входа — предзаполнение формы.
    pub last_login: Option<String>,
    /// Журнал мира (ФР-17): ring-буфер [`LOG_LIMIT`].
    pub log: VecDeque<LogEntry>,
    /// История посещённых локаций (ФР-03), свежие в конце.
    pub visited: Vec<VisitedLocation>,
    /// Результат `entity.info` — модалка осмотра (включая лут трупа).
    pub entity_detail: Option<serde_json::Value>,
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
    Item,
    Upgrade,
    Socket,
    /// M17/F30a: выбор оружия под инкрустацию руны/самоцвета.
    Inscribe,
    Create,
    Delete,
}

impl AppStore {
    /// Активный диалог (M11.2): экран персонажей — создание/удаление;
    /// игра — смерть > осмотр > лут > группа > гильдия > сделка > предмет >
    /// улучшение > инкрустация > выбор цели инкрустации (M17/F30a);
    /// вне игры/персонажей — None. `create_open` и `delete_open` —
    /// флаги UI (Root), остальное — состояние store.
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
                } else if self.item_detail.is_some() {
                    Some(Dialog::Item)
                } else if self.upgrade.is_some() {
                    Some(Dialog::Upgrade)
                } else if self.socketing.is_some() {
                    Some(Dialog::Socket)
                } else if self.inscribe.is_some() {
                    // M17/F30a: окно «Куда инкрустировать…».
                    Some(Dialog::Inscribe)
                } else {
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
            socketing: None,
            inscribe: None,
            item_cooldowns: BTreeMap::new(),
            time_offset: 0,
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
            last_login: None,
            log: VecDeque::new(),
            visited: Vec::new(),
            entity_detail: None,
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
                Act::Move | Act::Attack | Act::Examine => self.world_error = Some(msg),
                // M16/F29h: ошибки листа — системные уведомления
                // (тосты), как и весь прочий фидбек.
                Act::Invest
                | Act::Equip
                | Act::Unequip
                | Act::Drop
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
            | Act::Drop
            | Act::Upgrade
            | Act::Cast
            | Act::Quest
            | Act::Chat
            | Act::ChatDelete
            | Act::Trade => {
                // результат событиями (§8)
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
            Act::Learn => {
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
            Act::Preview => {
                if let (Some(upgrade), Some(payload)) =
                    (self.upgrade.as_mut(), ack.payload.as_ref())
                {
                    upgrade.preview = Some(payload.clone());
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

        // Осмотр: payload ack'а — детали сущности (включая лут трупа, ФР-03/ФР-11).
        if act == Act::Examine {
            self.entity_detail = ack.payload;
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
                self.time_offset = sync.server_ts - local;
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
                if let Some(required) = required {
                    if crate::update_check::is_newer(&required, env!("CARGO_PKG_VERSION")) {
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
                let Ok(snap) = serde_json::from_value::<StateSnapshot>(payload) else {
                    tracing::warn!("некорректный evt/state/snapshot — игнор");
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
            "evt/corpse/created" => {
                let Ok(corpse) = serde_json::from_value::<CorpseCreated>(payload) else {
                    return vec![];
                };
                let mut added = false;
                if let Some(snap) = self.snapshot.as_mut() {
                    if !snap
                        .location
                        .entities
                        .iter()
                        .any(|e| e.id == corpse.entity.id)
                    {
                        added = true;
                        snap.location.entities.push(corpse.entity);
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
            "evt/char/xp" => {
                let Ok(xp) = serde_json::from_value::<CharXp>(payload) else {
                    return vec![];
                };
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
                let text = match xp.rank {
                    Some(rank) => format!("+{} XP → {} — ранг {rank}!", xp.amount, xp.skill_id),
                    None => format!("+{} XP → {}", xp.amount, xp.skill_id),
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
                // ФР-06/ФР-07: убыль из инвентаря и из слотов экипировки.
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.inventory.retain(|i| i.uid != removed.uid);
                    snap.char
                        .equip
                        .retain(|_, slot| slot.as_deref() != Some(removed.uid.as_str()));
                }
                self.push_log(LogKind::Loot, format!("Потеряно: {}", removed.uid));
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
                                item.sockets = Some(sockets);
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
                    serde_json::from_value::<ld2_protocol::evt::UpgradeResult>(payload)
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
                self.upgrade = None;
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
                }
                let Ok(w) = serde_json::from_value::<WithProfession>(payload) else {
                    return vec![];
                };
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
                self.push_log(
                    LogKind::Loot,
                    format!(
                        "Профессия «{}»: ранг {}",
                        w.profession.id, w.profession.rank
                    ),
                );
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
                self.push_log(
                    LogKind::Chat,
                    format!("[{}] {}: {}", msg.channel, msg.name, msg.text),
                );
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
            _ => vec![], // неизвестные/ненужные события транспорт уже залогировал
        }
    }

    // ── журнал и история (ФР-17, ФР-03) ────────────────────────────────────

    fn push_log(&mut self, kind: LogKind, text: impl Into<String>) {
        if self.log.len() >= LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            kind,
            text: text.into(),
        });
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
        });
    }

    pub fn close_upgrade(&mut self) {
        self.upgrade = None;
    }

    /// Открыть инкрустацию посоха (ФР-09).
    pub fn open_socketing(&mut self, staff_uid: String) {
        self.socketing = Some(staff_uid);
    }

    pub fn close_socketing(&mut self) {
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
        self.upgrade = None;
        self.socketing = None;
        self.target = None;
        self.loot_confirm = None;
        self.quest_tracker.clear();
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
        self.item_detail = None;
        self.death = None;
        self.upgrade = None;
        self.socketing = None;
        self.quest_tracker.clear();
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
            if !ch.inventory.iter().any(|i| i.uid == rune.uid) {
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // Лист: предмет > улучшение > инкрустация (ФР-07/09/10).
        s.item_detail = Some(serde_json::json!({"name": "x"}));
        s.socketing = Some("staff".into());
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Item));
        s.item_detail = None;
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Socket));
        s.socketing = None;
        // M17/F30a: выбор цели инкрустации — отдельный диалог.
        s.inscribe = Some("rune_1".into());
        assert_eq!(s.active_dialog(false, false), Some(Dialog::Inscribe));
        s.inscribe = None;
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

        // Результат закрывает модалку и пишет лог.
        store.apply(evt(
            "evt/upgrade/result",
            serde_json::json!({
                "uid": "i_17", "ok": true, "new_upgrade": 3,
                "item_destroyed": false, "cost": { "gold": 300 }
            }),
        ));
        assert!(store.upgrade.is_none(), "модалка закрыта по результату");
        let last = store.log.back().expect("лог");
        assert_eq!(last.kind, LogKind::Loot);
        assert!(last.text.contains("+3"), "{}", last.text);
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
        assert!(store.upgrade.is_none());
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
            damage: None,
            def_id: None,
            name: "Руна льда".into(),
            spell: "ice_shard".into(),
            charges: 5,
            max: 5,
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
