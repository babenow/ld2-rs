//! `AppStore` — единое состояние клиента, собираемое **только** из событий
//! сервера (ТЗ §6.2). UI — проекция стора; редьюсер чистый и тестируется без
//! egui (ТЗ §15.5: «UI-фича без reducer-теста не мёржится»).
//!
//! Побочные эффекты (keyring, токен в транспорт) возвращаются из [`AppStore::apply`]
//! и исполняются слоем приложения — редьюсер остаётся детерминированным.

use std::collections::VecDeque;

use ld2_protocol::evt::{
    CharDeath, CharStats, CharXp, ChatMessage, CombatHit, CorpseCreated, CorpseLooted, CrimeAdded,
    CrimeStatusUpdate, EntityDespawn, EntityHp, EntityMove, EntitySpawn, ItemAdded, ItemRemoved,
    LocationChanged, MagicCast, StateSnapshot,
};
use ld2_protocol::types::{CharacterSummary, Hp, LocationSnapshot, QuestState};
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
    pub create_error: Option<String>,
    pub select_error: Option<String>,
    /// Ошибки действий мира (move/attack/examine), ФР-03.
    pub world_error: Option<String>,
    /// Ошибки действий листа/инвентаря (invest/equip/…), ФР-04/ФР-07.
    pub sheet_error: Option<String>,
    /// Модалка смерти (ФР-06): закрывается только подтверждением.
    pub death: Option<ld2_protocol::evt::CharDeath>,
    /// Описание предмета (ФР-07 «показать описание»).
    pub item_detail: Option<serde_json::Value>,
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
    /// Логин текущей сессии — для ключа keyring (`<host>:<login>`).
    session_login: Option<String>,
}

impl Default for AppStore {
    fn default() -> Self {
        Self::new()
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
            create_error: None,
            select_error: None,
            world_error: None,
            sheet_error: None,
            death: None,
            item_detail: None,
            notice: None,
            last_login: None,
            log: VecDeque::new(),
            visited: Vec::new(),
            entity_detail: None,
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
                Act::Create => self.create_error = Some(msg),
                Act::Select => self.select_error = Some(msg),
                Act::Delete => self.notice = Some(msg),
                Act::Move | Act::Attack | Act::Examine => self.world_error = Some(msg),
                Act::Invest | Act::Equip | Act::Unequip | Act::Drop => self.sheet_error = Some(msg),
            }
            return vec![];
        }

        match act {
            // Успех логина подтверждён ack'ом; дальше ждём evt/session.start.
            Act::Login => {}
            Act::Select => {}
            Act::Delete => {}
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
            Act::Attack | Act::Examine | Act::Drop => {} // событиями / payload ниже
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
            "evt/session.start" => {
                let Ok(cfg) = serde_json::from_value::<SessionConfig>(payload) else {
                    tracing::warn!("некорректный evt/session.start — игнор");
                    return vec![];
                };
                let refresh = cfg.session.refresh.token.clone();
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
                self.push_log(
                    LogKind::Location,
                    format!("Вход в мир: {}", snap.location.name),
                );
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
                let Ok(death) = serde_json::from_value::<CharDeath>(payload) else {
                    return vec![];
                };
                self.push_log(
                    LogKind::Crime,
                    format!(
                        "ВЫ ПОГИБЛИ — утрачено предметов: {}",
                        death.lost_items.len()
                    ),
                );
                self.death = Some(death);
                vec![]
            }
            "evt/magic/cast" => {
                let Ok(cast) = serde_json::from_value::<MagicCast>(payload) else {
                    return vec![];
                };
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
            "evt/quest/progress" | "evt/quest/updated" => {
                #[derive(serde::Deserialize)]
                struct WithQuest {
                    quest: QuestState,
                }
                let Ok(w) = serde_json::from_value::<WithQuest>(payload) else {
                    return vec![];
                };
                let title = w.quest.title.unwrap_or(w.quest.id);
                self.push_log(
                    LogKind::Quest,
                    format!("Квест «{title}»: {:?}", w.quest.state),
                );
                vec![]
            }
            "evt/crime/status" => {
                let Ok(update) = serde_json::from_value::<CrimeStatusUpdate>(payload) else {
                    return vec![];
                };
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
                self.push_log(
                    LogKind::Crime,
                    format!(
                        "{}: +{}с (итого {}с)",
                        added.log_entry.kind, added.log_entry.delta_s, added.log_entry.total_s
                    ),
                );
                vec![]
            }
            "evt/chat/message" => {
                let Ok(msg) = serde_json::from_value::<ChatMessage>(payload) else {
                    return vec![];
                };
                self.push_log(
                    LogKind::Chat,
                    format!("[{}] {}: {}", msg.channel, msg.name, msg.text),
                );
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
        self.sheet_error = None;
        self.pending = Some(act);
    }

    /// Подтверждение смерти (ФР-06): окно «Вы погибли» закрывается
    /// **только** этим действием — молча закрыть нельзя.
    pub fn confirm_death(&mut self) {
        self.death = None;
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
        self.sheet_error = None;
        self.death = None;
        self.item_detail = None;
        self.entity_detail = None;
        self.session_login = None;
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

    pub fn on_create_sent(&mut self) {
        self.create_error = None;
        self.pending = Some(Act::Create);
    }

    pub fn on_select_sent(&mut self) {
        self.select_error = None;
        self.pending = Some(Act::Select);
    }

    pub fn on_delete_sent(&mut self) {
        self.notice = None;
        self.pending = Some(Act::Delete);
    }

    /// Hot-switch: вернуться к выбору персонажей БЕЗ ре-логина (ФР-02).
    /// Снимок персонажа выгружается — следующий вход соберёт чистые сторы (DoD M2).
    /// Журнал и история — общий журнал аккаунта, переживают переключение.
    pub fn back_to_characters(&mut self) {
        self.screen = Screen::Characters;
        self.snapshot = None;
        self.select_error = None;
        self.world_error = None;
        self.sheet_error = None;
        self.entity_detail = None;
        self.item_detail = None;
        self.death = None;
        self.notice = None;
    }

    /// Логаут (ФР-01): чистый сессионный сброс + отзыв токена.
    pub fn logout(&mut self) -> Vec<Effect> {
        self.reset_to_login(None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ld2_mock_server::fixtures;

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
        store.on_delete_sent();
        let ack = ld2_protocol::Ack::error(9, None, &ErrorCode::CharLimit, "нельзя удалить");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.notice.as_deref(), Some("нельзя удалить"));
        assert_eq!(store.slot_usage(), (1, 8), "список не изменился");
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
        let last = store.log.back().expect("лог");
        assert_eq!(last.kind, LogKind::Crime);
        assert!(last.text.contains("ВЫ ПОГИБЛИ"));

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
            store.snapshot.as_ref().unwrap().char.equip.get("staff"),
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
            snap.char.equip.get("staff").is_none_or(|v| v.is_none()),
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
        assert!(store.sheet_error.is_none() && store.pending.is_none());
    }

    #[test]
    fn equip_ack_error_goes_to_sheet_error() {
        let mut store = in_game();
        store.on_sheet_sent(Act::Equip);
        let ack = ld2_protocol::Ack::error(12, None, &ErrorCode::ItemGone, "предмета нет");
        store.apply(IngressEvent::Ack(ack));
        assert_eq!(store.sheet_error.as_deref(), Some("предмета нет"));
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
