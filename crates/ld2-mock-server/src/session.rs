//! Машина состояния соединения мока: `cmd/*` → `ack` (+ `evt/*`).
//! Контракт — `docs/protocol.md`; поведение — примеры §5/§6/§12, ТЗ §4.8.2,
//! skills.md §9. Все события отправляются **после** своего ack (§2.4).

use std::collections::HashMap;

use ld2_protocol::cmd::{
    AuthLogin, AuthResume, CharCreate, CharDelete, CharSelect, CharXpInvest, ChatSend,
    CombatAttack, CraftLearn, CraftRecipes, CraftStart, CrimeLog, EntityInfo, GuildBrowse,
    GuildRankSet, ItemDrop, ItemEquip, ItemUnequip, ItemUse, LocationMove, MagicRuneCast,
    PartyInvite, PartyKick, QuestAccept, SkillUse, SocketExtract, SocketInsert, StateResync,
    TradeAddItem, TradeOpen, TradeSetGold, UpgradePreview, UpgradeStart,
};
use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::evt::{
    CastSource, CharDeath, CharXp, ChatMessage, CombatHit, CombatKind, CraftProfession,
    CraftResult, EntityHp, GuildLogEntry, ItemAdded, ItemRemoved, LocationChanged, LostItem,
    MagicCast, PartyUpdated, Pong, Respawn, TimeSync as TimeSyncEvent, TradeCloseReason,
    TradeClosed, TradeState, UpgradeResult,
};
use ld2_protocol::types::{
    CharacterSummary, CrimeLogEntry, CrimeStatus, Entity, EntityKind, ItemSnapshot, PartyMember,
    PartyState, ProfessionSnapshot, QuestPhase, QuestState, RuneInfo, Severity, SkillSnapshot,
};
use ld2_protocol::{ErrorCode, PROTOCOL_VERSION};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tracing::warn;

use crate::fixtures;

/// Ответ сессии наружу.
#[derive(Debug)]
pub enum Out {
    Text(String),
    Close,
}

impl Out {
    /// Текст конверта (для рассылки через [`crate::world::World`] другим
    /// соединениям); [`Out::Close`] не несёт текста.
    pub fn into_text(self) -> String {
        match self {
            Out::Text(t) => t,
            Out::Close => String::new(),
        }
    }
}

/// Данные превью улучшения (§7 `upgrade.preview` → `upgrade.start`).
struct Preview {
    item_uid: String,
}

pub struct Session {
    /// Общее состояние сервера (версии, рассылка, межигровое) — см. [`crate::world`].
    world: std::sync::Arc<std::sync::Mutex<crate::world::World>>,
    /// Идентификатор этого WS-соединения (ключ в реестре World).
    conn_id: String,
    /// Логин аккаунта сессии (для сохранения состояния при отключении).
    login: Option<String>,
    authenticated: bool,
    /// Лимит слотов персонажей из конфига/CLI (D6; DoD M2 — без правки кода).
    char_slot_limit: u32,
    chars: Vec<CharacterSummary>,
    char: Option<ld2_protocol::types::CharacterState>,
    location: ld2_protocol::types::LocationSnapshot,
    crime_log: Vec<ld2_protocol::types::CrimeLogEntry>,
    previews: HashMap<String, Preview>,
    preview_counter: u64,
    /// M5: следующее улучшение проваливается (сценарий break_on_fail).
    fail_next_upgrade: bool,
    /// M6: трупы, лут с которых уже собран (защита от повтора).
    looted_corpses: HashSet<String>,
}

impl Session {
    /// Своя сессия на каждое WS-соединение; общее состояние — в [`crate::world::World`].
    pub fn new(
        world: std::sync::Arc<std::sync::Mutex<crate::world::World>>,
        conn_id: String,
        char_slot_limit: u32,
    ) -> Self {
        Self {
            world,
            conn_id,
            login: None,
            authenticated: false,
            char_slot_limit,
            chars: Vec::new(),
            char: None,
            location: fixtures::village(),
            crime_log: fixtures::crime_log(),
            previews: HashMap::new(),
            preview_counter: 0,
            fail_next_upgrade: false,
            looted_corpses: HashSet::new(),
        }
    }

    /// Короткий лок общего состояния (дисциплина: только session → world).
    fn w(&self) -> std::sync::MutexGuard<'_, crate::world::World> {
        self.world.lock().expect("mutex мира мока")
    }

    /// Отправляет событие (собранный конверт) другому соединению/игроку.
    fn w_event(ty: &str, state_version: Option<u64>, payload: Value) -> String {
        out_event(ty, state_version, payload).into_text()
    }

    /// Обработка входящего текста → список исходящих сообщений (ack → events).
    pub fn on_text(&mut self, text: &str) -> Vec<Out> {
        let env: Envelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(e) => {
                warn!("мок: неразборчивое сообщение: {e}");
                return vec![];
            }
        };
        if !env.msg_type.starts_with("cmd/") {
            warn!("мок: не-команда от клиента: {}", env.msg_type);
            return vec![];
        }
        self.dispatch(&env)
    }

    // ── низкоуровневые помощники ────────────────────────────────────────────

    fn bump(&self) -> u64 {
        self.w().bump()
    }

    fn ack_ok(&self, seq: u32, state_version: Option<u64>) -> Out {
        out_ack(Ack::ok(seq, state_version))
    }

    fn ack_ok_payload<T: Serialize>(&self, seq: u32, state_version: Option<u64>, p: &T) -> Out {
        match Ack::ok_with_payload(seq, state_version, p) {
            Ok(ack) => out_ack(ack),
            Err(e) => self.ack_err(seq, None, ErrorCode::Internal, format!("encode: {e}")),
        }
    }

    fn ack_err(
        &self,
        seq: u32,
        state_version: Option<u64>,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Out {
        out_ack(Ack::error(seq, state_version, &code, message))
    }

    fn event(&self, ty: &str, state_version: Option<u64>, payload: Value) -> Out {
        out_event(ty, state_version, payload)
    }

    fn require_auth(&self, seq: u32) -> Option<Out> {
        if self.authenticated {
            None
        } else {
            Some(self.ack_err(seq, None, ErrorCode::AuthExpired, "нет активной сессии"))
        }
    }

    fn require_char(&self, seq: u32) -> Option<Out> {
        if self.char.is_some() {
            None
        } else {
            Some(self.ack_err(seq, None, ErrorCode::NotInLocation, "персонаж не выбран"))
        }
    }

    fn payload_of<T: DeserializeOwned>(env: &Envelope, seq: u32) -> Result<T, Out> {
        let value = env.payload.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|e| {
            out_ack(Ack::error(
                seq,
                None,
                &ErrorCode::Internal,
                format!("некорректный payload: {e}"),
            ))
        })
    }

    // ── диспетчер ───────────────────────────────────────────────────────────

    // Явные `return` в руках диспетчера — единый стиль «команда → vec ответов».
    #[allow(clippy::needless_return)]
    fn dispatch(&mut self, env: &Envelope) -> Vec<Out> {
        let seq = env.seq.unwrap_or(0);
        match env.msg_type.as_str() {
            // auth / session
            "cmd/auth.login" => return self.auth_login(env, seq),
            "cmd/auth.resume" => return self.auth_resume(env, seq),
            "cmd/auth.logout" => {
                self.authenticated = false;
                return vec![self.ack_ok(seq, None)];
            }
            "cmd/session.refresh" => {
                return if let Some(err) = self.require_auth(seq) {
                    vec![err]
                } else {
                    vec![self.ack_ok_payload(
                        seq,
                        None,
                        &json!({ "access_token": "at_mock_rotated", "expires_in_s": 900 }),
                    )]
                };
            }

            // персонажи
            "cmd/char.list" => {
                return if let Some(err) = self.require_auth(seq) {
                    vec![err]
                } else {
                    vec![self.ack_ok_payload(seq, None, &json!({ "characters": self.chars }))]
                };
            }
            "cmd/char.create" => return self.char_create(env, seq),
            "cmd/char.select" => return self.char_select(env, seq),
            "cmd/char.delete" => return self.char_delete(env, seq),
            "cmd/char.xp_invest" => return self.char_xp_invest(env, seq),

            // мир
            "cmd/location.look" => {
                return if let Some(err) = self.require_auth(seq) {
                    vec![err]
                } else {
                    vec![self.ack_ok_payload(seq, None, &json!({ "location": self.location }))]
                };
            }
            "cmd/location.move" => return self.location_move(env, seq),
            "cmd/entity.info" => return self.entity_info(env, seq),

            // бой
            "cmd/combat.attack" => return self.combat_attack(env, seq, 7, false),
            "cmd/combat.stop" | "cmd/combat.flee" => {
                return if let Some(err) = self.require_char(seq) {
                    vec![err]
                } else {
                    vec![self.ack_ok(seq, None)]
                };
            }
            "cmd/skill.use" => return self.skill_use(env, seq),

            // предметы
            "cmd/item.use" => return self.item_use(env, seq),
            "cmd/item.equip" => return self.item_equip(env, seq),
            "cmd/item.unequip" => return self.item_unequip(env, seq),
            "cmd/item.drop" => return self.item_drop(env, seq),

            // магия / сокеты
            "cmd/magic.scroll_cast" => return self.scroll_cast(env, seq),
            "cmd/magic.rune_cast" => return self.rune_cast(env, seq),
            "cmd/socket.insert" => return self.socket_insert(env, seq),
            "cmd/socket.extract" => return self.socket_extract(env, seq),

            // улучшение
            "cmd/upgrade.preview" => return self.upgrade_preview(env, seq),
            "cmd/upgrade.start" => return self.upgrade_start(env, seq),

            // профессии
            "cmd/craft.list" => {
                return if let Some(err) = self.require_char(seq) {
                    vec![err]
                } else {
                    let professions = self
                        .char
                        .as_ref()
                        .map(|c| c.professions.clone())
                        .unwrap_or_default();
                    vec![self.ack_ok_payload(seq, None, &json!({ "professions": professions }))]
                };
            }
            "cmd/craft.recipes" => return self.craft_recipes(env, seq),
            "cmd/craft.learn" => return self.craft_learn(env, seq),
            "cmd/craft.start" => return self.craft_start(env, seq),

            // квесты
            "cmd/quest.list" => {
                return if let Some(err) = self.require_char(seq) {
                    vec![err]
                } else {
                    let quests = self
                        .char
                        .as_ref()
                        .map(|c| c.quests.clone())
                        .unwrap_or_default();
                    vec![self.ack_ok_payload(seq, None, &json!({ "quests": quests }))]
                };
            }
            "cmd/quest.accept" | "cmd/quest.turn_in" | "cmd/quest.abandon" => {
                return self.quest_action(env, seq, &env.msg_type);
            }

            // преступность
            "cmd/crime.log" => {
                let limit = Self::payload_of::<CrimeLog>(env, seq)
                    .ok()
                    .and_then(|p| p.limit);
                let entries: Vec<_> = limit
                    .map(|n| {
                        self.crime_log
                            .iter()
                            .rev()
                            .take(n as usize)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_else(|| self.crime_log.clone());
                return if let Some(err) = self.require_auth(seq) {
                    vec![err]
                } else {
                    vec![self.ack_ok_payload(seq, None, &json!({ "entries": entries }))]
                };
            }

            // чат
            "cmd/chat.send" => return self.chat_send(env, seq),

            // группа (состояние общее для всех соединений — §8, ФР-14)
            "cmd/party.create" => return self.party_create(seq),
            "cmd/party.invite" => return self.party_invite(env, seq),
            "cmd/party.leave" => return self.party_leave(seq),
            "cmd/party.kick" => return self.party_kick(env, seq),
            "cmd/party.accept" => return self.party_answer(env, seq, true),
            "cmd/party.decline" => return self.party_answer(env, seq, false),

            // торговля
            "cmd/trade.open" => return self.trade_open(env, seq),
            "cmd/trade.add_item" => return self.trade_add_item(env, seq),
            "cmd/trade.set_gold" => return self.trade_set_gold(env, seq),
            "cmd/trade.propose" | "cmd/trade.accept" => return self.trade_lock(env, seq),
            "cmd/trade.cancel" => return self.trade_cancel(env, seq),

            // гильдия
            "cmd/guild.browse" => {
                let _ = Self::payload_of::<GuildBrowse>(env, seq);
                return vec![self.ack_ok_payload(
                    seq,
                    None,
                    &json!({ "guilds": [fixtures::guild_fixture()] }),
                )];
            }
            "cmd/guild.create" => return self.guild_create(env, seq),
            "cmd/guild.join" => return self.guild_join(seq),
            "cmd/guild.leave" => return self.guild_leave(seq),
            "cmd/guild.invite" => return self.guild_invite(env, seq),
            "cmd/guild.kick" => return self.guild_kick(env, seq),
            "cmd/guild.rank_set" => return self.guild_rank_set(env, seq),

            // состояние / сервис
            "cmd/state.resync" => return self.state_resync(env, seq),
            "cmd/ping" => {
                let pong = Pong {
                    ts: fixtures::now_rfc3339(),
                    rtt_ms: 0,
                };
                return vec![
                    self.ack_ok(seq, None),
                    self.event(
                        "evt/pong",
                        None,
                        serde_json::to_value(&pong).unwrap_or_default(),
                    ),
                ];
            }
            "cmd/time.sync" => {
                let sync = TimeSyncEvent {
                    server_ts: fixtures::now_epoch(),
                    rtt_hint_ms: Some(0),
                };
                return vec![
                    self.ack_ok(seq, None),
                    self.event(
                        "evt/time.sync",
                        None,
                        serde_json::to_value(&sync).unwrap_or_default(),
                    ),
                ];
            }

            other => {
                warn!("мок: неизвестная команда {other}");
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::Internal,
                    format!("неизвестная команда: {other}"),
                )];
            }
        }
    }

    // ── auth / session ──────────────────────────────────────────────────────

    fn auth_login(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        let Ok(cmd) = Self::payload_of::<AuthLogin>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if !fixtures::account(&cmd.login, &cmd.password) {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::BadCredentials,
                "неверный логин или пароль",
            )];
        }
        // Аккаунт определяет список персонажей и refresh-токен (§5).
        self.login = Some(cmd.login.clone());
        self.authenticated = true;
        self.chars = fixtures::chars_for(&cmd.login);
        self.restore_home(&cmd.login);
        let refresh = crate::world::refresh_token_for(&cmd.login);
        self.w().bind_token(&refresh, &cmd.login);
        let sv = self.bump();
        let config = fixtures::session_config_for(&cmd.login, self.char_slot_limit);
        vec![
            self.ack_ok_payload(
                seq,
                Some(sv),
                &json!({ "access_token": "at_mock", "expires_in_s": 900 }),
            ),
            self.event(
                "evt/session.start",
                Some(sv),
                serde_json::to_value(&config).unwrap_or_default(),
            ),
        ]
    }

    fn auth_resume(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        let Ok(cmd) = Self::payload_of::<AuthResume>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.refresh_token.is_empty() {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::AuthExpired,
                "refresh-токен недействителен",
            )];
        }
        self.authenticated = true;
        // §4.5: resume по токену восстанавливает аккаунт (персонаж переживает
        // обрыв); неизвестный токен — resume без состояния (так и раньше).
        let login = self.w().login_by_token(&cmd.refresh_token);
        let sv = self.bump();
        let config = match &login {
            Some(login) => {
                self.login = Some(login.clone());
                let home = self.w().load_home(login);
                self.chars = home
                    .as_ref()
                    .map(|h| h.chars.clone())
                    .unwrap_or_else(|| fixtures::chars_for(login));
                self.restore_home(login);
                fixtures::session_config_for(login, self.char_slot_limit)
            }
            None => fixtures::session_config_for(fixtures::LOGIN, self.char_slot_limit),
        };
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event(
                "evt/session.start",
                Some(sv),
                serde_json::to_value(&config).unwrap_or_default(),
            ),
        ]
    }

    /// Восстанавливает персонажа/локацию/журнал сохранённого аккаунта.
    fn restore_home(&mut self, login: &str) {
        let home = self.w().load_home(login);
        if let Some(home) = home {
            self.char = home.char;
            self.location = home.location;
            self.crime_log = home.crime_log;
        }
    }

    // ── персонажи ───────────────────────────────────────────────────────────

    fn char_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharCreate>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if self.chars.len() as u32 >= self.char_slot_limit {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::CharLimit,
                "все слоты персонажей заняты",
            )];
        }
        if cmd.name.trim().len() < 2 || self.chars.iter().any(|c| c.name == cmd.name) {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::NameTaken,
                "имя занято или некорректно",
            )];
        }
        if !fixtures::PROFILES.contains(&cmd.profile.as_str()) {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::BadProfile,
                format!("неизвестный профиль: {}", cmd.profile),
            )];
        }
        let character = CharacterSummary {
            // Уникальный id из общего счётчика (два аккаунта: c1/c2 заняты).
            id: self.w().next_char_id(),
            name: cmd.name.clone(),
            location: self.location.name.clone(),
            summary: Some(format!("Профиль: {}", cmd.profile)),
            crime: vec![],
            online: false,
            extra: Default::default(),
        };
        self.chars.push(character.clone());
        let sv = self.bump();
        vec![self.ack_ok_payload(seq, Some(sv), &json!({ "character": character }))]
    }

    fn char_select(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharSelect>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(summary) = self.chars.iter().find(|c| c.id == cmd.char_id).cloned() else {
            return vec![self.ack_err(seq, None, ErrorCode::NameTaken, "персонаж не найден")];
        };
        self.char = Some(fixtures::character(&summary.id, &summary.name));
        self.location = fixtures::village();
        // Реестр игроков: роутинг whisper/приглашений (ФР-13/14, DoD M8).
        self.w().bind_player(
            &self.conn_id.clone(),
            &summary.id,
            &summary.name,
            &self.location.id.clone(),
        );
        let sv = self.bump();
        let snap = fixtures::snapshot(sv, self.char.as_ref().expect("выше"), &self.location);
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event(
                "evt/state/snapshot",
                Some(sv),
                serde_json::to_value(&snap).unwrap_or_default(),
            ),
        ]
    }

    fn char_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharDelete>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if self.chars.len() <= 1 {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::CharLimit,
                "нельзя удалить последнего персонажа",
            )];
        }
        self.chars.retain(|c| c.id != cmd.char_id);
        if self.char.as_ref().is_some_and(|c| c.id == cmd.char_id) {
            self.char = None;
        }
        let sv = self.bump();
        vec![self.ack_ok(seq, Some(sv))]
    }

    fn char_xp_invest(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharXpInvest>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.points <= 0 {
            return vec![self.ack_err(seq, None, ErrorCode::XpPool, "points должен быть > 0")];
        }

        let mut outcome: Option<Result<(i64, CharXp), (ErrorCode, String)>> = None;
        if let Some(ch) = self.char.as_mut() {
            if ch.xp_pool < cmd.points {
                outcome = Some(Err((
                    ErrorCode::XpPool,
                    format!("в пуле {} очков", ch.xp_pool),
                )));
            } else if let Some(skill) = ch.skills.iter_mut().find(|s| s.id == cmd.skill_id) {
                ch.xp_pool -= cmd.points;
                skill.xp += cmd.points;
                let mut rank_up = false;
                while skill.xp >= skill.xp_next_threshold() && skill.rank < 30 {
                    skill.xp -= skill.xp_next_threshold();
                    skill.rank += 1;
                    rank_up = true;
                }
                outcome = Some(Ok((
                    ch.xp_pool,
                    CharXp {
                        skill_id: skill.id.clone(),
                        amount: cmd.points,
                        skill_xp: skill.xp,
                        rank: rank_up.then_some(skill.rank),
                        xp_to_next: Some(skill.xp_next_threshold()),
                        pool_left: Some(ch.xp_pool),
                    },
                )));
            } else {
                outcome = Some(Err((
                    ErrorCode::Internal,
                    format!("нет навыка {}", cmd.skill_id),
                )));
            }
        }

        match outcome {
            Some(Ok((pool_left, xp_event))) => {
                let sv = self.bump();
                vec![
                    self.ack_ok_payload(seq, Some(sv), &json!({ "pool_left": pool_left })),
                    self.event(
                        "evt/char/xp",
                        Some(sv),
                        serde_json::to_value(&xp_event).unwrap_or_default(),
                    ),
                ]
            }
            Some(Err((code, msg))) => vec![self.ack_err(seq, None, code, msg)],
            None => vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")],
        }
    }

    // ── мир ─────────────────────────────────────────────────────────────────

    fn location_move(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<LocationMove>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(target) = fixtures::exit_target(&self.location.id, &cmd.exit_id) else {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::ExitBlocked,
                format!("выход {} отсюда недоступен", cmd.exit_id),
            )];
        };
        self.location = target;
        let sv = self.bump();
        let loc = serde_json::to_value(&self.location).unwrap_or_default();
        vec![
            self.ack_ok_payload(seq, Some(sv), &json!({ "location": loc })),
            self.event(
                "evt/location/changed",
                Some(sv),
                serde_json::to_value(LocationChanged {
                    location: self.location.clone(),
                })
                .unwrap_or_default(),
            ),
        ]
    }

    fn entity_info(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<EntityInfo>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(entity) = self
            .location
            .entities
            .iter()
            .find(|e| e.id == cmd.entity_id)
            .cloned()
        else {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::TargetGone,
                "сущность не найдена в локации",
            )];
        };
        let loot = if entity.kind == EntityKind::Corpse {
            let uid = format!("loot:{}:0", entity.id);
            let taken = self.looted_corpses.contains(&entity.id);
            let item = if entity.was_player.unwrap_or(false) {
                ItemSnapshot {
                    uid,
                    item_type: "weapon".into(),
                    name: "Кинжал".into(),
                    rarity: "common".into(),
                    upgrade: 0,
                    stats: Default::default(),
                    requires: None,
                    sockets: None,
                    qty: Some(1),
                    gold_value: Some(40),
                    extra: Default::default(),
                }
            } else {
                ItemSnapshot {
                    uid,
                    item_type: "material".into(),
                    name: format!("Трофея: {}", entity.name.trim_start_matches("Труп: ")),
                    rarity: "common".into(),
                    upgrade: 0,
                    stats: Default::default(),
                    requires: None,
                    sockets: None,
                    qty: Some(1),
                    gold_value: Some(15),
                    extra: Default::default(),
                }
            };
            if taken {
                Value::Null
            } else {
                json!([item])
            }
        } else {
            Value::Null
        };
        let detail = json!({
            "entity": entity,
            "loot": loot,
            "looted": self.looted_corpses.contains(&entity.id),
            "was_player": entity.was_player.unwrap_or(false),
        });
        vec![self.ack_ok_payload(seq, None, &detail)]
    }

    // ── бой ─────────────────────────────────────────────────────────────────

    fn combat_attack(&mut self, env: &Envelope, seq: u32, damage: i64, _skill: bool) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CombatAttack>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        self.hit_entity(&cmd.target_id, damage, CombatKind::Melee, seq)
    }

    fn hit_entity(&mut self, target_id: &str, damage: i64, kind: CombatKind, seq: u32) -> Vec<Out> {
        let Some(entity) = self
            .location
            .entities
            .iter_mut()
            .find(|e| e.id == target_id)
        else {
            return vec![self.ack_err(seq, None, ErrorCode::TargetGone, "цели нет в локации")];
        };
        if entity.kind == EntityKind::Player {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::PvpForbidden,
                "нападение на игроков в моке запрещено",
            )];
        }
        if entity.kind != EntityKind::Monster {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::TargetGone,
                "по этой цели нельзя ударить",
            )];
        }

        let target_name = entity.name.clone();
        let hp = entity
            .hp
            .get_or_insert(ld2_protocol::types::Hp { cur: 0, max: 0 });
        hp.cur = (hp.cur - damage).max(0);
        let died = hp.cur == 0;
        let new_hp = *hp;

        let sv = self.bump();
        let char_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let char_id = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();

        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        outs.push(
            self.event(
                "evt/combat/hit",
                Some(sv),
                serde_json::to_value(CombatHit {
                    source: char_id,
                    source_name: char_name,
                    target: target_id.into(),
                    target_name: target_name.clone(),
                    kind,
                    amount: damage,
                    crit: false,
                    miss: false,
                    text: format!("Вы бьёте: {target_name} −{damage}."),
                    extra: Default::default(),
                })
                .unwrap_or_default(),
            ),
        );
        outs.push(
            self.event(
                "evt/entity/hp",
                Some(sv),
                serde_json::to_value(EntityHp {
                    entity_id: target_id.into(),
                    hp: new_hp,
                })
                .unwrap_or_default(),
            ),
        );

        if died {
            self.location.entities.retain(|e| e.id != target_id);
            outs.push(self.event(
                "evt/entity/despawn",
                Some(sv),
                json!({ "entity_id": target_id, "location_id": self.location.id }),
            ));
            let corpse = Entity {
                id: format!("cor_{target_id}"),
                kind: EntityKind::Corpse,
                name: format!("Труп: {target_name}"),
                hp: None,
                tags: None,
                was_player: Some(false),
                aggro: None,
            };
            self.location.entities.push(corpse.clone());
            outs.push(self.event(
                "evt/corpse/created",
                Some(sv),
                json!({ "entity": corpse, "loot_count": 1 }),
            ));

            // Лут с монстра — автодобавление в инвентарь (ФР-07).
            if let Some(ch) = self.char.as_mut() {
                let loot = ItemSnapshot {
                    uid: format!("loot_{target_id}"),
                    item_type: "material".into(),
                    name: format!("Трофея: {target_name}"),
                    rarity: "common".into(),
                    upgrade: 0,
                    stats: Default::default(),
                    requires: None,
                    sockets: None,
                    qty: Some(1),
                    gold_value: Some(15),
                    extra: Default::default(),
                };
                ch.inventory.push(loot.clone());
                outs.push(self.event(
                    "evt/item/added",
                    Some(sv),
                    serde_json::to_value(ItemAdded { item: loot }).unwrap_or_default(),
                ));
            }

            // Прогресс квеста «Волки» (ФР-12, M7): убийство монстра двигает
            // цель; при достижении need — состояние «к сдаче».
            let mut progressed: Option<QuestState> = None;
            if let Some(ch) = self.char.as_mut() {
                if let Some(q) = ch
                    .quests
                    .iter_mut()
                    .find(|q| q.id == "q_wolves" && q.state == QuestPhase::Active)
                {
                    if let Some(goal) = q.goals.first_mut() {
                        let cur = goal.cur.unwrap_or(0);
                        let need = goal.need.unwrap_or(5);
                        if cur < need {
                            goal.cur = Some(cur + 1);
                            if cur + 1 >= need {
                                q.state = QuestPhase::TurnIn;
                            }
                            progressed = Some(q.clone());
                        }
                    }
                }
            }
            if let Some(quest) = progressed {
                outs.push(self.event("evt/quest/progress", Some(sv), json!({ "quest": quest })));
            }
        }
        outs
    }

    fn skill_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SkillUse>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        // Мок знает одну активку: power_strike (skills.md §4) — требует sword ≥ 5.
        if cmd.skill_id != "power_strike" {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::Internal,
                format!("нет активного навыка {}", cmd.skill_id),
            )];
        }
        let sword_rank = self
            .char
            .as_ref()
            .and_then(|c| c.skills.iter().find(|s| s.id == "sword").map(|s| s.rank))
            .unwrap_or(0);
        if sword_rank < 5 {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::RequirementAttr,
                "требуется Владение мечом 5",
            )];
        }
        let Some(target) = cmd.target_id else {
            return vec![self.ack_ok(seq, None)];
        };
        let is_monster = self
            .location
            .entities
            .iter()
            .any(|e| e.id == target && e.kind == EntityKind::Monster);
        let mut outs = self.hit_entity(&target, 13, CombatKind::Skill, seq);
        if is_monster {
            // ФР-05: кулдаун активки (skills.md §4: power_strike — 8 с).
            outs.push(self.event(
                "evt/combat/cooldown",
                None,
                json!({ "skill_id": cmd.skill_id, "until_epoch": fixtures::now_epoch() + 8 }),
            ));
        }
        outs
    }

    // ── предметы ────────────────────────────────────────────────────────────

    fn item_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUse>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        // M6 (ФР-11): сбор лута с трупа — отдельный адресный uid `loot:*`.
        if cmd.item_uid.starts_with("loot:") {
            return self.take_corpse_loot(seq, &cmd.item_uid);
        }
        let kind = self
            .char
            .as_ref()
            .and_then(|c| c.inventory.iter().find(|i| i.uid == cmd.item_uid))
            .map(|i| i.item_type.clone());
        match kind.as_deref() {
            Some("scroll") => self.scroll_cast(env, seq),
            Some("potion") => {
                let sv = self.bump();
                let mut outs = vec![self.ack_ok(seq, Some(sv))];
                if let Some(ch) = self.char.as_mut() {
                    ch.inventory.retain(|i| i.uid != cmd.item_uid);
                    ch.hp.cur = (ch.hp.cur + 20).min(ch.hp.max);
                    let hp = ch.hp;
                    outs.push(
                        self.event(
                            "evt/item/removed",
                            Some(sv),
                            serde_json::to_value(ItemRemoved {
                                uid: cmd.item_uid.clone(),
                                reason: ld2_protocol::evt::ItemRemovalReason::Consumed,
                            })
                            .unwrap_or_default(),
                        ),
                    );
                    outs.push(self.event(
                        "evt/char/stats",
                        Some(sv),
                        json!({ "patch": { "hp": hp } }),
                    ));
                }
                outs
            }
            _ => vec![self.ack_err(seq, None, ErrorCode::ItemGone, "предмет нельзя применить")],
        }
    }

    fn item_equip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemEquip>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let slots = fixtures::session_config(self.char_slot_limit)
            .inventory
            .equip_slots;
        if !slots.contains(&cmd.slot) {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::Internal,
                format!("нет слота {}", cmd.slot),
            )];
        }
        let mut outs = Vec::new();
        if let Some(ch) = self.char.as_ref() {
            if !ch.inventory.iter().any(|i| i.uid == cmd.item_uid) {
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::ItemGone,
                    "предмета нет в инвентаре",
                )];
            }
        }
        let sv = self.bump();
        if let Some(ch) = self.char.as_mut() {
            ch.equip
                .insert(cmd.slot.clone(), Some(cmd.item_uid.clone()));
            let item = ch.inventory.iter().find(|i| i.uid == cmd.item_uid).cloned();
            let char_ref = ch.clone();
            outs.push(self.ack_ok_payload(
                seq,
                Some(sv),
                &json!({ "char": char_ref, "item": item }),
            ));
        }
        outs
    }

    fn item_unequip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUnequip>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let mut outs = Vec::new();
        let has_item = self
            .char
            .as_ref()
            .is_some_and(|ch| ch.equip.get(&cmd.slot).is_some_and(|v| v.is_some()));
        if !has_item {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "слот пуст")];
        }
        let sv = self.bump();
        if let Some(ch) = self.char.as_mut() {
            let uid = ch.equip.remove(&cmd.slot).flatten();
            let item = uid.and_then(|u| ch.inventory.iter().find(|i| i.uid == u).cloned());
            let char_ref = ch.clone();
            outs.push(self.ack_ok_payload(
                seq,
                Some(sv),
                &json!({ "char": char_ref, "item": item }),
            ));
        }
        outs
    }

    fn item_drop(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemDrop>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if let Some(ch) = self.char.as_mut() {
            if !ch.inventory.iter().any(|i| i.uid == cmd.item_uid) {
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::ItemGone,
                    "предмета нет в инвентаре",
                )];
            }
            ch.inventory.retain(|i| i.uid != cmd.item_uid);
            ch.equip.retain(|_, v| v.as_ref() != Some(&cmd.item_uid));
            let sv = self.bump();
            return vec![
                self.ack_ok(seq, Some(sv)),
                self.event(
                    "evt/item/removed",
                    Some(sv),
                    serde_json::to_value(ItemRemoved {
                        uid: cmd.item_uid,
                        reason: ld2_protocol::evt::ItemRemovalReason::Dropped,
                    })
                    .unwrap_or_default(),
                ),
            ];
        }
        vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")]
    }

    // ── магия (D4: руна = мана + заряд; свиток без маны) ────────────────────

    fn scroll_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUse>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };

        let uid = cmd.item_uid;
        let spell = self
            .char
            .as_ref()
            .and_then(|c| c.inventory.iter().find(|i| i.uid == uid))
            .map(|i| {
                i.stats
                    .get("spell")
                    .and_then(Value::as_str)
                    .unwrap_or("scroll_spell")
                    .to_string()
            });
        let Some(spell) = spell else {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "свитка нет")];
        };

        let sv = self.bump();
        if let Some(ch) = self.char.as_mut() {
            ch.inventory.retain(|i| i.uid != uid);
        }
        let char_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let char_id = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();

        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        // D4: свиток ману НЕ тратит → mp_left не передаётся.
        outs.push(
            self.event(
                "evt/magic/cast",
                Some(sv),
                serde_json::to_value(MagicCast {
                    by: char_id,
                    by_name: char_name,
                    spell,
                    what: CastSource::Scroll,
                    target: cmd.target_id.clone(),
                    charges_left: None,
                    mp_left: None,
                    ok: true,
                    extra: Default::default(),
                })
                .unwrap_or_default(),
            ),
        );
        outs.push(
            self.event(
                "evt/item/removed",
                Some(sv),
                serde_json::to_value(ItemRemoved {
                    uid,
                    reason: ld2_protocol::evt::ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ),
        );
        if let Some(target) = cmd.target_id {
            outs.extend(self.hit_entity(&target, 12, CombatKind::Scroll, seq));
        }
        outs
    }

    fn rune_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicRuneCast>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };

        // Проверки D4: посох в инвентаре, в руках, руна в сокете, заряды, мана.
        let mut error: Option<(ErrorCode, String)> = None;
        let mut new_charges = 0;
        let mut new_mp = 0;
        let mut spell = String::new();

        if let Some(ch) = self.char.as_ref() {
            let staff = ch.inventory.iter().find(|i| i.uid == cmd.staff_uid);
            match staff {
                None => error = Some((ErrorCode::ItemGone, "посоха нет в инвентаре".into())),
                Some(staff) => {
                    if ch.equip.get("staff").and_then(|v| v.as_ref()) != Some(&cmd.staff_uid) {
                        error = Some((ErrorCode::Internal, "посох не экипирован".into()));
                    } else {
                        let socket = staff
                            .sockets
                            .as_ref()
                            .and_then(|s| s.iter().find(|s| s.index == cmd.socket_index));
                        match socket.and_some_rune() {
                            None => error = Some((ErrorCode::NoRuneSlot, "сокет пуст".into())),
                            Some(rune) if rune.charges <= 0 => {
                                error = Some((ErrorCode::NoCharges, "у руны нет зарядов".into()))
                            }
                            Some(rune) => {
                                if ch.mp.cur < fixtures::RUNE_MP_COST {
                                    error = Some((
                                        ErrorCode::NotEnoughMp,
                                        format!(
                                            "нужно {} маны, есть {}",
                                            fixtures::RUNE_MP_COST,
                                            ch.mp.cur
                                        ),
                                    ));
                                } else {
                                    new_charges = rune.charges - 1;
                                    new_mp = ch.mp.cur - fixtures::RUNE_MP_COST;
                                    spell = rune.spell.clone();
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }

        let sv = self.bump();
        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        let mut staff_value = None;
        if let Some(ch) = self.char.as_mut() {
            ch.mp.cur = new_mp;
            if let Some(staff) = ch.inventory.iter_mut().find(|i| i.uid == cmd.staff_uid) {
                if let Some(socket) = staff
                    .sockets
                    .as_mut()
                    .and_then(|s| s.iter_mut().find(|s| s.index == cmd.socket_index))
                {
                    if let Some(rune) = socket.rune.as_mut() {
                        rune.charges = new_charges;
                    }
                }
            }
            staff_value = ch
                .inventory
                .iter()
                .find(|i| i.uid == cmd.staff_uid)
                .cloned()
                .map(serde_json::to_value)
                .and_then(Result::ok);
        }
        let char_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let char_id = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        outs.push(
            self.event(
                "evt/magic/cast",
                Some(sv),
                serde_json::to_value(MagicCast {
                    by: char_id,
                    by_name: char_name,
                    spell,
                    what: CastSource::Rune,
                    target: cmd.target_id.clone(),
                    charges_left: Some(new_charges),
                    mp_left: Some(new_mp as i32),
                    ok: true,
                    extra: Default::default(),
                })
                .unwrap_or_default(),
            ),
        );
        if let Some(target) = cmd.target_id {
            outs.extend(self.hit_entity(&target, 12, CombatKind::Rune, seq));
        }
        // Патч инвентаря — после каста (каст → его последствия).
        if let Some(staff_value) = staff_value {
            outs.push(self.event(
                "evt/item/state",
                Some(sv),
                json!({ "uid": cmd.staff_uid, "patch": { "item": staff_value } }),
            ));
        }
        outs
    }

    fn socket_insert(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketInsert>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let mut error: Option<(ErrorCode, String)> = None;
        let mut rune_info: Option<RuneInfo> = None;

        if let Some(ch) = self.char.as_ref() {
            let staff = ch.inventory.iter().find(|i| i.uid == cmd.staff_uid);
            let rune_item = ch.inventory.iter().find(|i| i.uid == cmd.rune_uid);
            match (staff, rune_item) {
                (None, _) => error = Some((ErrorCode::ItemGone, "посоха нет".into())),
                (_, None) => error = Some((ErrorCode::ItemGone, "руны нет".into())),
                (Some(staff), Some(rune_item)) => {
                    if staff.item_type != "staff" {
                        error = Some((ErrorCode::NoRuneSlot, "это не посох".into()));
                    } else if rune_item.item_type != "rune" {
                        error = Some((ErrorCode::NoRuneSlot, "это не руна".into()));
                    } else {
                        let socket_free = staff.sockets.as_ref().is_some_and(|s| {
                            s.iter()
                                .any(|s| s.index == cmd.socket_index && s.rune.is_none())
                        });
                        if !socket_free {
                            error =
                                Some((ErrorCode::NoRuneSlot, "сокет занят или отсутствует".into()));
                        } else {
                            rune_info = Some(RuneInfo {
                                name: rune_item.name.clone(),
                                spell: rune_item
                                    .stats
                                    .get("spell")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_string(),
                                charges: rune_item
                                    .stats
                                    .get("charges")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(5) as i32,
                                max: rune_item
                                    .stats
                                    .get("max")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(5) as i32,
                            });
                        }
                    }
                }
            }
        }

        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }

        let sv = self.bump();
        let mut staff_value = None;
        if let Some(ch) = self.char.as_mut() {
            ch.inventory.retain(|i| i.uid != cmd.rune_uid);
            if let Some(staff) = ch.inventory.iter_mut().find(|i| i.uid == cmd.staff_uid) {
                if let Some(socket) = staff
                    .sockets
                    .as_mut()
                    .and_then(|s| s.iter_mut().find(|s| s.index == cmd.socket_index))
                {
                    socket.rune = rune_info;
                }
            }
            staff_value = ch
                .inventory
                .iter()
                .find(|i| i.uid == cmd.staff_uid)
                .cloned()
                .map(serde_json::to_value)
                .and_then(Result::ok);
        }

        let mut outs = Vec::new();
        match staff_value {
            // §7: ack несёт обновлённый посох (как в socket.extract).
            Some(staff_value) => {
                outs.push(self.ack_ok_payload(seq, Some(sv), &json!({ "staff": staff_value })));
                outs.push(self.event(
                    "evt/item/state",
                    Some(sv),
                    json!({ "uid": cmd.staff_uid, "patch": { "item": staff_value } }),
                ));
            }
            None => outs.push(self.ack_ok(seq, Some(sv))),
        }
        outs
    }

    fn socket_extract(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketExtract>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let mut error: Option<(ErrorCode, String)> = None;
        let mut extracted: Option<ItemSnapshot> = None;

        if let Some(ch) = self.char.as_ref() {
            let staff = ch.inventory.iter().find(|i| i.uid == cmd.staff_uid);
            match staff {
                None => error = Some((ErrorCode::ItemGone, "посоха нет".into())),
                Some(staff) => {
                    let socket = staff
                        .sockets
                        .as_ref()
                        .and_then(|s| s.iter().find(|s| s.index == cmd.socket_index));
                    match socket.and_some_rune() {
                        None => error = Some((ErrorCode::NoRuneSlot, "сокет пуст".into())),
                        Some(rune) => {
                            let mut stats = serde_json::Map::new();
                            stats.insert("spell".into(), json!(rune.spell));
                            stats.insert("charges".into(), json!(rune.charges));
                            stats.insert("max".into(), json!(rune.max));
                            extracted = Some(ItemSnapshot {
                                uid: format!("rune_{}_{}", cmd.staff_uid, cmd.socket_index),
                                item_type: "rune".into(),
                                name: rune.name.clone(),
                                rarity: "common".into(),
                                upgrade: 0,
                                stats,
                                requires: None,
                                sockets: None,
                                qty: Some(1),
                                gold_value: None,
                                extra: Default::default(),
                            });
                        }
                    }
                }
            }
        }

        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }

        let sv = self.bump();
        let mut outs = Vec::new();
        if let Some(ch) = self.char.as_mut() {
            if let Some(staff) = ch.inventory.iter_mut().find(|i| i.uid == cmd.staff_uid) {
                if let Some(socket) = staff
                    .sockets
                    .as_mut()
                    .and_then(|s| s.iter_mut().find(|s| s.index == cmd.socket_index))
                {
                    socket.rune = None;
                }
            }
            let item = extracted.clone();
            if let Some(item) = extracted {
                ch.inventory.push(item);
            }
            let staff_value = ch
                .inventory
                .iter()
                .find(|i| i.uid == cmd.staff_uid)
                .cloned()
                .map(serde_json::to_value)
                .and_then(Result::ok);
            outs.push(self.ack_ok_payload(
                seq,
                Some(sv),
                &json!({ "staff": staff_value, "rune": item }),
            ));
        }
        outs
    }

    // ── улучшение (ФР-10) ───────────────────────────────────────────────────

    fn upgrade_preview(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradePreview>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let upgrade = self
            .char
            .as_ref()
            .and_then(|c| c.inventory.iter().find(|i| i.uid == cmd.item_uid))
            .map(|i| i.upgrade);
        let Some(upgrade) = upgrade else {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "предмета нет")];
        };
        if upgrade as u32 >= fixtures::UPGRADE_MAX {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::UpgradeMax,
                "достигнут максимум улучшения",
            )];
        }
        self.preview_counter += 1;
        let preview_id = format!("pv_{}", self.preview_counter);
        self.previews.insert(
            preview_id.clone(),
            Preview {
                item_uid: cmd.item_uid.clone(),
            },
        );
        vec![self.ack_ok_payload(
            seq,
            None,
            &json!({
                "preview_id": preview_id,
                "delta": { "upgrade": format!("+{}", upgrade + 1) },
                "cost": { "gold": 300, "materials": { "rune_dust": 2 } },
                "chance": 0.75
            }),
        )]
    }

    fn upgrade_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradeStart>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(preview) = self.previews.remove(&cmd.preview_id) else {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::UpgradePreviewStale,
                "превью устарело — пересоздайте",
            )];
        };
        if preview.item_uid != cmd.item_uid {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::UpgradePreviewStale,
                "превью от другого предмета",
            )];
        }

        // Сценарий M5 (break_on_fail): предмет разрушается — по ТЗ ФР-10
        // «предмет разрушается»; результат — событием, не ack.
        if self.fail_next_upgrade {
            self.fail_next_upgrade = false;
            let removed = match self.char.as_mut() {
                Some(ch) => {
                    let found = ch.inventory.iter().any(|i| i.uid == cmd.item_uid);
                    if found {
                        ch.inventory.retain(|i| i.uid != cmd.item_uid);
                        ch.equip
                            .retain(|_, v| v.as_deref() != Some(cmd.item_uid.as_str()));
                    }
                    found
                }
                None => false,
            };
            if !removed {
                return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "предмета нет")];
            }
            let sv = self.bump();
            let mut outs = vec![self.ack_ok(seq, Some(sv))];
            outs.push(
                self.event(
                    "evt/upgrade/result",
                    Some(sv),
                    serde_json::to_value(UpgradeResult {
                        uid: cmd.item_uid.clone(),
                        ok: false,
                        new_upgrade: None,
                        item_destroyed: Some(true),
                        cost: serde_json::Map::from_iter([
                            ("gold".to_string(), json!(300)),
                            ("materials".to_string(), json!({ "rune_dust": 2 })),
                        ]),
                    })
                    .unwrap_or_default(),
                ),
            );
            outs.push(
                self.event(
                    "evt/item/removed",
                    Some(sv),
                    serde_json::to_value(ItemRemoved {
                        uid: cmd.item_uid,
                        reason: ld2_protocol::evt::ItemRemovalReason::Consumed,
                    })
                    .unwrap_or_default(),
                ),
            );
            return outs;
        }

        let mut new_upgrade = None;
        if let Some(ch) = self.char.as_mut() {
            if let Some(item) = ch.inventory.iter_mut().find(|i| i.uid == cmd.item_uid) {
                item.upgrade += 1;
                new_upgrade = Some(item.upgrade);
            }
        }
        let Some(new_upgrade) = new_upgrade else {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "предмета нет")];
        };

        let sv = self.bump();
        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        outs.push(
            self.event(
                "evt/upgrade/result",
                Some(sv),
                serde_json::to_value(UpgradeResult {
                    uid: cmd.item_uid.clone(),
                    ok: true,
                    new_upgrade: Some(new_upgrade as u32),
                    item_destroyed: Some(false),
                    cost: serde_json::Map::from_iter([
                        ("gold".to_string(), json!(300)),
                        ("materials".to_string(), json!({ "rune_dust": 2 })),
                    ]),
                })
                .unwrap_or_default(),
            ),
        );
        outs.push(self.event(
            "evt/item/state",
            Some(sv),
            json!({ "uid": cmd.item_uid, "patch": { "upgrade": new_upgrade } }),
        ));
        outs
    }

    // ── профессии (skills.md) ───────────────────────────────────────────────

    fn craft_recipes(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let filter = Self::payload_of::<CraftRecipes>(env, seq)
            .ok()
            .and_then(|p| p.profession);
        let recipes: Vec<Value> = fixtures::recipes()
            .into_iter()
            .filter(|r| filter.is_none() || r["profession"] == filter.as_deref().unwrap_or(""))
            .collect();
        vec![self.ack_ok_payload(seq, None, &json!({ "recipes": recipes }))]
    }

    fn craft_learn(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftLearn>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        const PRIMARY: &[&str] = &[
            "blacksmith",
            "alchemist",
            "scribe",
            "woodcarver",
            "tailor",
            "cook",
        ];
        const SECONDARY: &[&str] = &["herbalist", "hunter"];
        if !PRIMARY.contains(&cmd.profession_id.as_str())
            && !SECONDARY.contains(&cmd.profession_id.as_str())
        {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::Internal,
                format!("нет профессии {}", cmd.profession_id),
            )];
        }

        let mut error: Option<(ErrorCode, String)> = None;
        let mut learned: Option<ProfessionSnapshot> = None;
        if let Some(ch) = self.char.as_mut() {
            if ch.professions.iter().any(|p| p.id == cmd.profession_id) {
                learned = ch
                    .professions
                    .iter()
                    .find(|p| p.id == cmd.profession_id)
                    .cloned();
            } else {
                let is_primary = PRIMARY.contains(&cmd.profession_id.as_str());
                let used = ch
                    .professions
                    .iter()
                    .filter(|p| PRIMARY.contains(&p.id.as_str()))
                    .count() as u32;
                if is_primary && used >= fixtures::PROFESSION_LIMIT {
                    error = Some((
                        ErrorCode::ProfessionLimit,
                        format!("лимит первичных профессий: {used}"),
                    ));
                } else {
                    let prof = ProfessionSnapshot {
                        id: cmd.profession_id.clone(),
                        rank: 1,
                        xp: 0,
                        xp_to_next: 80,
                        extra: Default::default(),
                    };
                    ch.professions.push(prof.clone());
                    learned = Some(prof);
                }
            }
        }

        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }
        let Some(profession) = learned else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")];
        };
        let sv = self.bump();
        vec![
            self.ack_ok_payload(seq, Some(sv), &json!({ "profession": profession })),
            self.event(
                "evt/craft/profession",
                Some(sv),
                serde_json::to_value(CraftProfession {
                    profession: profession.clone(),
                })
                .unwrap_or_default(),
            ),
        ]
    }

    fn craft_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftStart>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(recipe) = fixtures::recipes()
            .into_iter()
            .find(|r| r["id"] == cmd.recipe_id)
        else {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::Internal,
                format!("нет рецепта {}", cmd.recipe_id),
            )];
        };

        // Проверка ингредиентов по типам (skills.md §5.4).
        let inputs = recipe["inputs"].as_array().cloned().unwrap_or_default();
        let mut error: Option<(ErrorCode, String)> = None;
        if let Some(ch) = self.char.as_ref() {
            for input in &inputs {
                let need_type = input["type"].as_str().unwrap_or_default();
                let need_qty = input["qty"].as_i64().unwrap_or(1) * cmd.qty.max(1);
                let have: i64 = ch
                    .inventory
                    .iter()
                    .filter(|i| i.item_type == need_type)
                    .map(|i| i.qty.unwrap_or(1))
                    .sum();
                if have < need_qty {
                    error = Some((
                        ErrorCode::Ingredients,
                        format!("не хватает {need_type}: {have}/{need_qty}"),
                    ));
                    break;
                }
            }
        }
        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }

        let sv = self.bump();
        let mut consumed = Vec::new();
        let made_uid = format!("crafted_{}", self.preview_counter + 1);
        self.preview_counter += 1;
        if let Some(ch) = self.char.as_mut() {
            for input in &inputs {
                let need_type = input["type"].as_str().unwrap_or_default();
                let mut left = input["qty"].as_i64().unwrap_or(1) * cmd.qty.max(1);
                ch.inventory.retain_mut(|i| {
                    if i.item_type != need_type || left <= 0 {
                        return true;
                    }
                    let have = i.qty.unwrap_or(1);
                    let take = have.min(left);
                    left -= take;
                    let rest = have - take;
                    consumed.push(json!({ "type": need_type, "qty": take }));
                    if rest > 0 {
                        i.qty = Some(rest);
                        true
                    } else {
                        false
                    }
                });
            }
            let output_type = recipe["output"]["type"]
                .as_str()
                .unwrap_or("item")
                .to_string();
            let output_name = recipe["name"].as_str().unwrap_or("Предмет").to_string();
            let made = ItemSnapshot {
                uid: made_uid.clone(),
                item_type: output_type,
                name: output_name,
                rarity: "common".into(),
                upgrade: 0,
                stats: Default::default(),
                requires: None,
                sockets: None,
                qty: Some(1),
                gold_value: Some(50),
                extra: Default::default(),
            };
            ch.inventory.push(made.clone());
            let mut outs = vec![self.ack_ok(seq, Some(sv))];
            outs.push(
                self.event(
                    "evt/craft/result",
                    Some(sv),
                    serde_json::to_value(CraftResult {
                        recipe_id: cmd.recipe_id.clone(),
                        ok: true,
                        made: vec![ld2_protocol::evt::MadeItem {
                            uid: made_uid,
                            qty: cmd.qty.max(1),
                        }],
                        consumed,
                        extra: Default::default(),
                    })
                    .unwrap_or_default(),
                ),
            );
            outs.push(self.event(
                "evt/item/added",
                Some(sv),
                serde_json::to_value(ItemAdded { item: made }).unwrap_or_default(),
            ));
            return outs;
        }
        vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")]
    }

    // ── квесты ──────────────────────────────────────────────────────────────

    fn quest_action(&mut self, env: &Envelope, seq: u32, msg_type: &str) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<QuestAccept>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let mut error: Option<(ErrorCode, String)> = None;
        let mut quest_out: Option<QuestState> = None;
        let mut pool_award: Option<i64> = None;

        if let Some(ch) = self.char.as_mut() {
            match msg_type {
                "cmd/quest.accept" => {
                    quest_out = ch
                        .quests
                        .iter_mut()
                        .find(|q| q.id == cmd.quest_id)
                        .map(|q| q.clone());
                    if quest_out.is_none() {
                        error = Some((ErrorCode::Internal, "квест не найден".into()));
                    }
                }
                "cmd/quest.turn_in" => {
                    if let Some(q) = ch.quests.iter_mut().find(|q| q.id == cmd.quest_id) {
                        q.state = ld2_protocol::types::QuestPhase::Done;
                        quest_out = Some(q.clone());
                        pool_award = Some(150);
                        ch.xp_pool += 150;
                    } else {
                        error = Some((ErrorCode::Internal, "квест не найден".into()));
                    }
                }
                _ => {
                    let removed = ch.quests.iter().find(|q| q.id == cmd.quest_id).cloned();
                    match removed {
                        Some(q) => {
                            ch.quests.retain(|q| q.id != cmd.quest_id);
                            quest_out = Some(q);
                        }
                        None => error = Some((ErrorCode::Internal, "квест не найден".into())),
                    }
                }
            }
        }

        if let Some((code, msg)) = error {
            return vec![self.ack_err(seq, None, code, msg)];
        }
        let Some(quest) = quest_out else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")];
        };
        let sv = self.bump();
        let mut outs = vec![self.ack_ok_payload(seq, Some(sv), &json!({ "quest": quest }))];
        if pool_award.is_some() {
            let pool_left = self.char.as_ref().map(|c| c.xp_pool).unwrap_or_default();
            outs.push(self.event(
                "evt/quest/updated",
                Some(sv),
                json!({ "quest": quest, "pool_left": pool_left, "award": pool_award }),
            ));
        } else {
            outs.push(self.event("evt/quest/updated", Some(sv), json!({ "quest": quest })));
        }
        outs
    }

    // ── чат ─────────────────────────────────────────────────────────────────

    fn chat_send(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatSend>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let channels = &fixtures::session_config(self.char_slot_limit).chat.channels;
        if !channels.contains(&cmd.channel) {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::Internal,
                format!("нет канала {}", cmd.channel),
            )];
        }
        let max_len = fixtures::session_config(self.char_slot_limit).chat.max_len as usize;
        if cmd.text.chars().count() > max_len {
            return vec![self.ack_err(
                seq,
                None,
                ErrorCode::RateLimited,
                format!("сообщение длиннее {max_len} символов"),
            )];
        }
        // Сценарии мока (PLAN-M4, см. --help): тестовые триггеры — это обычный
        // текст чата, контракт (§7) их не специфицирует, интерпретация внутренняя.
        if cmd.text.starts_with("/scenario ") {
            return match cmd.text.as_str() {
                "/scenario death" => self.scenario_death(seq),
                "/scenario upgrade_fail" => self.scenario_upgrade_fail(seq),
                // НФР §8: burst 200 событий/с — проверка без потери порядка.
                "/scenario stress" => self.scenario_stress(seq),
                other => vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::Internal,
                    format!("неизвестный сценарий: {other}"),
                )],
            };
        }
        let (name, tags) = match self.char.as_ref() {
            Some(c) => (
                c.name.clone(),
                Some(
                    c.crime
                        .statuses
                        .iter()
                        .map(|s| s.kind.clone())
                        .collect::<Vec<_>>(),
                ),
            ),
            None => ("Аноним".to_string(), None),
        };
        let msg = ChatMessage {
            channel: cmd.channel.clone(),
            from: self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
            name: name.clone(),
            text: cmd.text.clone(),
            tags,
            sent_at: fixtures::now_rfc3339(),
            extra: Default::default(),
        };
        let echo = serde_json::to_value(&msg).unwrap_or_default();
        let mut outs = vec![self.ack_ok(seq, None)];

        // Доставка адресатам (§8; порядок «ack → событие» для вызвавшего).
        match cmd.channel.as_str() {
            // Whisper (ФР-13): только адресат по `to` (имя), себе — эхо.
            "whisper" => {
                let to = cmd.to.clone().unwrap_or_default();
                let target = {
                    let w = self.w();
                    w.player_by_name(&to).cloned()
                };
                match target {
                    Some(p) if p.char_id != msg.from => {
                        let text = Self::w_event("evt/chat/message", None, echo.clone());
                        self.w().send_to_conn(&p.conn_id, text);
                    }
                    // Нет адресата — эхо себе (как и любой канал), без ошибки:
                    // §7 не специфицирует E_* для «не в сети».
                    _ => {}
                }
            }
            // Гильдейский канал — участникам гильдии; групповой — участникам
            // группы; остальные (system/location/global) — все в локации/мир.
            "guild" => {
                let text = Self::w_event("evt/chat/message", None, echo);
                let w = self.w();
                if let Some(g) = w.guild.as_ref() {
                    for m in &g.members {
                        if m.id != msg.from {
                            w.send_to_player(&m.id, text.clone());
                        }
                    }
                }
            }
            "party" => {
                let text = Self::w_event("evt/chat/message", None, echo);
                let w = self.w();
                for conn in w.party_member_conns() {
                    if conn != self.conn_id {
                        w.send_to_conn(&conn, text.clone());
                    }
                }
            }
            "location" => {
                // В локации — тем, кто в ней же; для мока — все онлайн-игроки
                // в этой локации (DoD M8).
                let text = Self::w_event("evt/chat/message", None, echo);
                let my_loc = self.location.id.clone();
                let w = self.w();
                for p in w.online_players() {
                    if p.location_id == my_loc && p.char_id != msg.from {
                        w.send_to_conn(&p.conn_id, text.clone());
                    }
                }
            }
            // system/global — общий мир.
            _ => {
                let text = Self::w_event("evt/chat/message", None, echo);
                let w = self.w();
                w.broadcast(text, Some(&self.conn_id));
            }
        }
        outs.push(self.event(
            "evt/chat/message",
            None,
            serde_json::to_value(msg).unwrap_or_default(),
        ));
        outs
    }

    /// Частичное сложение сроков (§4.10): T = сумма живых сроков, вклад
    /// нового преступления `k·t·(1 − T/T_MAX)` — растёт сублинеейно.
    /// Возвращает события `evt/crime/added` + `evt/crime/status` (замена).
    fn add_crime_status(&mut self, kind: &str, sv: Option<u64>) -> Vec<Out> {
        let now = fixtures::now_epoch();
        let config = fixtures::session_config(self.char_slot_limit);
        let k = config.crime.blend_k;
        let t_max = config.crime.t_max_s.max(1) as f64;
        let base_s = config
            .crime
            .base
            .get(&format!("{kind}_s"))
            .copied()
            .unwrap_or(3_600);
        let severity = config
            .crime
            .kinds
            .iter()
            .find(|c| c.kind == kind)
            .map(|c| c.severity)
            .unwrap_or(Severity::Medium);

        let (t_total, existing) = match self.char.as_ref() {
            Some(ch) => {
                let live: Vec<i64> = ch
                    .crime
                    .statuses
                    .iter()
                    .filter(|s| s.expires_at > now)
                    .map(|s| s.expires_at - now)
                    .collect();
                let existing = ch
                    .crime
                    .statuses
                    .iter()
                    .find(|s| s.kind == kind)
                    .map(|s| s.expires_at);
                (live.iter().sum::<i64>(), existing)
            }
            None => return vec![],
        };

        // §4.10: вклад = k·t·(1 − T/T_MAX), минимум 1 секунда.
        let delta = ((k * base_s as f64) * (1.0 - (t_total as f64 / t_max)))
            .round()
            .max(1.0) as i64;

        let mut status_out = None;
        if let Some(ch) = self.char.as_mut() {
            if let Some(prev) = existing {
                // Повторное преступление того же kind — срок растёт с убыванием.
                if let Some(st) = ch.crime.statuses.iter_mut().find(|s| s.kind == kind) {
                    st.expires_at = prev + delta;
                    status_out = Some(st.clone());
                }
            } else {
                let st = CrimeStatus {
                    kind: kind.to_string(),
                    severity,
                    expires_at: now + delta,
                };
                ch.crime.statuses.push(st.clone());
                status_out = Some(st);
            }
        }
        let Some(status) = status_out else {
            return vec![];
        };

        let entry = CrimeLogEntry {
            kind: kind.to_string(),
            at: now,
            delta_s: delta,
            total_s: t_total + delta,
        };
        let statuses = self
            .char
            .as_ref()
            .map(|c| c.crime.statuses.clone())
            .unwrap_or_default();
        vec![
            self.event(
                "evt/crime/added",
                sv,
                json!({ "status": status, "log_entry": entry }),
            ),
            self.event(
                "evt/crime/status",
                sv,
                json!({
                    "statuses": statuses,
                    "log_hint": format!("{}: +{}с (итого {}с)", kind, delta, t_total + delta)
                }),
            ),
        ]
    }

    /// Сбор лута с трупа (ФР-11): `loot:<corpse>:<n>`; труп игрока вешает
    /// «Мародёр» через blend §4.10; повторный сбор невозможен.
    fn take_corpse_loot(&mut self, seq: u32, loot_uid: &str) -> Vec<Out> {
        let parts: Vec<&str> = loot_uid.split(':').collect();
        if parts.len() < 3 {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "нет лута")];
        }
        let corpse_id = parts[1];
        if self.looted_corpses.contains(corpse_id) {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "лут уже собран")];
        }
        let Some(corpse) = self
            .location
            .entities
            .iter()
            .find(|e| e.id == corpse_id && e.kind == EntityKind::Corpse)
            .cloned()
        else {
            return vec![self.ack_err(seq, None, ErrorCode::TargetGone, "трупа нет в локации")];
        };
        let was_player = corpse.was_player.unwrap_or(false);
        let item = if was_player {
            ItemSnapshot {
                uid: loot_uid.into(),
                item_type: "weapon".into(),
                name: "Кинжал".into(),
                rarity: "common".into(),
                upgrade: 0,
                stats: Default::default(),
                requires: None,
                sockets: None,
                qty: Some(1),
                gold_value: Some(40),
                extra: Default::default(),
            }
        } else {
            ItemSnapshot {
                uid: loot_uid.into(),
                item_type: "material".into(),
                name: format!("Трофея: {}", corpse.name.trim_start_matches("Труп: ")),
                rarity: "common".into(),
                upgrade: 0,
                stats: Default::default(),
                requires: None,
                sockets: None,
                qty: Some(1),
                gold_value: Some(15),
                extra: Default::default(),
            }
        };
        self.looted_corpses.insert(corpse_id.to_string());
        let by = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        if let Some(ch) = self.char.as_mut() {
            ch.inventory.push(item.clone());
        }
        let sv = self.bump();
        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        outs.push(self.event(
            "evt/item/added",
            Some(sv),
            serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
        ));
        outs.push(self.event(
            "evt/corpse/looted",
            Some(sv),
            json!({ "corpse_id": corpse_id, "by": by, "by_name": by_name,
                    "was_player": was_player }),
        ));
        if was_player {
            // ФР-11: лут с трупа игрока → статус «Мародёр» (blend §4.10).
            outs.extend(self.add_crime_status("marauder", Some(sv)));
        }
        outs
    }

    /// Сценарий M5: следующее улучшение проваливается (break_on_fail).
    fn scenario_upgrade_fail(&mut self, seq: u32) -> Vec<Out> {
        self.fail_next_upgrade = true;
        let msg = ChatMessage {
            channel: "system".into(),
            from: "server".into(),
            name: "Сценарий".into(),
            text: "следующее улучшение предмета провалится (break_on_fail)".into(),
            tags: None,
            sent_at: fixtures::now_rfc3339(),
            extra: Default::default(),
        };
        vec![
            self.ack_ok(seq, None),
            self.event(
                "evt/chat/message",
                None,
                serde_json::to_value(msg).unwrap_or_default(),
            ),
        ]
    }

    /// Сценарий смерти (DoD M4): сводка утраченного → **полная потеря лута**
    /// (инвентарь + экипировка) → респаун в деревне с полным hp/mp.
    /// Триггер: чат `/scenario death` (см. `--help` мока).
    /// НФР §8 (stress 200 ev/s): burst из 200 событий чата одной пачкой —
    /// клиент обязан применить их все, сохранив порядок.
    fn scenario_stress(&mut self, seq: u32) -> Vec<Out> {
        let mut outs = vec![self.ack_ok(seq, None)];
        for i in 1..=200 {
            let msg = ChatMessage {
                channel: "system".into(),
                from: "server".into(),
                name: "Стресс".into(),
                text: format!("stress {i}"),
                tags: None,
                sent_at: fixtures::now_rfc3339(),
                extra: Default::default(),
            };
            outs.push(self.event(
                "evt/chat/message",
                None,
                serde_json::to_value(msg).unwrap_or_default(),
            ));
        }
        outs
    }

    fn scenario_death(&mut self, seq: u32) -> Vec<Out> {
        let (lost, lost_uids, hp, mp) = match self.char.as_mut() {
            Some(ch) => {
                let lost: Vec<LostItem> = ch
                    .inventory
                    .iter()
                    .map(|i| LostItem {
                        uid: i.uid.clone(),
                        name: i.name.clone(),
                        rarity: Some(i.rarity.clone()),
                        extra: Default::default(),
                    })
                    .collect();
                let uids: Vec<String> = ch.inventory.iter().map(|i| i.uid.clone()).collect();
                ch.inventory.clear();
                ch.equip.clear();
                ch.hp.cur = ch.hp.max;
                ch.mp.cur = ch.mp.max;
                (lost, uids, ch.hp, ch.mp)
            }
            None => {
                return vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")];
            }
        };
        self.location = fixtures::village();
        let sv = self.bump();

        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        // ФР-06: окно «Вы погибли» со сводкой утраченного.
        outs.push(
            self.event(
                "evt/char/death",
                Some(sv),
                serde_json::to_value(CharDeath {
                    victim_char: true,
                    lost_items: lost,
                    corpse: "cor_self".into(),
                    respawn: Respawn {
                        location_id: "l_village".into(),
                        delay_s: 0,
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                })
                .unwrap_or_default(),
            ),
        );
        // Каждый утерянный предмет — отдельным событием (§8).
        for uid in lost_uids {
            outs.push(
                self.event(
                    "evt/item/removed",
                    Some(sv),
                    serde_json::to_value(ItemRemoved {
                        uid,
                        reason: ld2_protocol::evt::ItemRemovalReason::LostOnDeath,
                    })
                    .unwrap_or_default(),
                ),
            );
        }
        // Респаун: ресурсы и локация.
        outs.push(self.event(
            "evt/char/stats",
            Some(sv),
            json!({ "patch": { "hp": hp, "mp": mp } }),
        ));
        outs.push(
            self.event(
                "evt/location/changed",
                Some(sv),
                serde_json::to_value(LocationChanged {
                    location: self.location.clone(),
                })
                .unwrap_or_default(),
            ),
        );
        outs
    }

    // ── группа (состояние в [`crate::world::World`], рассылка участникам) ──

    /// Рассылка `evt/party/updated` всем участникам (свой клиент — через
    /// возвращаемый `Out`, чужие — в их сокеты, порядок §2.4 не нарушен).
    fn party_broadcast(&self, sv: u64, payload: Value) {
        let text = Self::w_event("evt/party/updated", Some(sv), payload);
        let w = self.w();
        for conn in w.party_member_conns() {
            if conn != self.conn_id {
                w.send_to_conn(&conn, text.clone());
            }
        }
    }

    fn party_create(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let (id, name, hp, loc) = match self.char.as_ref() {
            Some(c) => (c.id.clone(), c.name.clone(), c.hp, self.location.id.clone()),
            None => return vec![self.ack_err(seq, None, ErrorCode::Internal, "нет персонажа")],
        };
        let party = PartyState {
            id: "pt_1".into(),
            leader: id.clone(),
            members: vec![PartyMember {
                id,
                name,
                hp: Some(hp),
                location_id: Some(loc),
                ready: Some(true),
                extra: Default::default(),
            }],
            extra: Default::default(),
        };
        let sv = self.bump();
        {
            let mut w = self.w();
            w.party = Some(party.clone());
        }
        self.party_broadcast(sv, json!({ "party": party }));
        vec![
            self.ack_ok_payload(seq, Some(sv), &json!({ "party": party })),
            self.event(
                "evt/party/updated",
                Some(sv),
                serde_json::to_value(PartyUpdated { party: Some(party) }).unwrap_or_default(),
            ),
        ]
    }

    /// Приглашение (§7): онлайн-игрок получает `evt/party/updated` c
    /// опциональным полем `invite` (§11 — новое поле совместимо без bump);
    /// не-онлайн (NPC-сущность в локации) — добавляется сразу, как раньше.
    fn party_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyInvite>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let my_char = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let sv = self.bump();

        // Онлайн-путь (DoD M8): приглашение второму клиенту.
        let online = {
            let w = self.w();
            w.player_by_char(&cmd.player_id).cloned()
        };
        if let Some(target) = online {
            if self.w().party.is_none() {
                // Приглашаем в несуществующую группу — сначала создать нельзя:
                // контракт (§7) не специфицирует ошибку, повторяем M1-семантику
                // «сначала party.create».
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::Internal,
                    "группы нет — сначала party.create",
                )];
            }
            let invite_id = self.w().new_invite(&my_char, &target.char_id);
            let payload = json!({
                "party": serde_json::Value::Null,
                "invite": {
                    "invite_id": invite_id,
                    "from": my_char,
                    "from_name": self.char.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                }
            });
            let text = Self::w_event("evt/party/updated", Some(sv), payload.clone());
            self.w().send_to_conn(&target.conn_id, text);
            return vec![self.ack_ok(seq, Some(sv))];
        }

        // Фолбэк: фиктивный NPC-участник (контрактные/одиночные сценарии).
        let name = if cmd.player_id == "p_77" {
            "Марк"
        } else {
            "Гость"
        };
        {
            let mut w = self.w();
            if w.party.is_none() {
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::Internal,
                    "группы нет — сначала party.create",
                )];
            }
            if let Some(party) = w.party.as_mut() {
                if !party.members.iter().any(|m| m.id == cmd.player_id) {
                    party.members.push(PartyMember {
                        id: cmd.player_id,
                        name: name.into(),
                        hp: Some(ld2_protocol::types::Hp { cur: 30, max: 55 }),
                        location_id: Some("l_forest".into()),
                        ready: None,
                        extra: Default::default(),
                    });
                }
            }
        }
        let payload = json!({ "party": self.w().party.clone() });
        self.party_broadcast(sv, payload.clone());
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event("evt/party/updated", Some(sv), payload),
        ]
    }

    /// Ответ на приглашение (§7 `party.accept/decline {invite_id}`).
    fn party_answer(&mut self, env: &Envelope, seq: u32, accept: bool) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Answer {
            invite_id: String,
        }
        let Ok(cmd) = Self::payload_of::<Answer>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let invite = self.w().take_invite(&cmd.invite_id);
        let Some(_invite) = invite.filter(|i| i.to == me) else {
            return vec![self.ack_err(seq, None, ErrorCode::TargetGone, "приглашение не найдено")];
        };
        let sv = self.bump();
        {
            let mut w = self.w();
            if accept {
                if w.party.is_none() {
                    // Инициатор вышел, пока приглашение висело.
                    return vec![self.ack_err(
                        seq,
                        None,
                        ErrorCode::TargetGone,
                        "группа распущена",
                    )];
                }
                if let Some(party) = w.party.as_mut() {
                    if !party.members.iter().any(|m| m.id == me) {
                        party.members.push(PartyMember {
                            id: me,
                            name: self
                                .char
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            hp: self.char.as_ref().map(|c| c.hp),
                            location_id: Some(self.location.id.clone()),
                            ready: Some(true),
                            extra: Default::default(),
                        });
                    }
                }
            }
        }
        let payload = json!({ "party": self.w().party.clone() });
        self.party_broadcast(sv, payload.clone());
        vec![
            self.ack_ok_payload(seq, Some(sv), &payload),
            self.event("evt/party/updated", Some(sv), payload),
        ]
    }

    /// Выход из группы: при оставшихся участниках — рассылка, при пустой —
    /// `party: null` участникам и себе.
    fn party_leave(&mut self, seq: u32) -> Vec<Out> {
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let sv = self.bump();
        {
            let mut w = self.w();
            if let Some(party) = w.party.as_mut() {
                party.members.retain(|m| m.id != me);
                if party.members.is_empty() {
                    w.party = None;
                } else if party.leader == me {
                    party.leader = party.members[0].id.clone();
                }
            }
        }
        let payload = json!({ "party": self.w().party.clone() });
        self.party_broadcast(sv, payload.clone());
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event("evt/party/updated", Some(sv), payload),
        ]
    }

    fn party_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyKick>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let sv = self.bump();
        {
            let mut w = self.w();
            if w.party.is_none() {
                return vec![self.ack_err(seq, None, ErrorCode::Internal, "группы нет")];
            }
            if let Some(party) = w.party.as_mut() {
                party.members.retain(|m| m.id != cmd.player_id);
                if party.members.is_empty() {
                    w.party = None;
                }
            }
        }
        let payload = json!({ "party": self.w().party.clone() });
        self.party_broadcast(sv, payload.clone());
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event("evt/party/updated", Some(sv), payload),
        ]
    }

    // ── торговля (общая на сервер, зеркало you/them, версии — §8/ФР-15) ────

    /// Зеркальная перспектива сделки для своей стороны.
    fn trade_view(&self, my_char: &str) -> Option<TradeState> {
        let w = self.w();
        let t = w.trade.as_ref()?;
        let (me, them) = if t.a.char_id == my_char {
            (&t.a, &t.b)
        } else {
            (&t.b, &t.a)
        };
        Some(TradeState {
            offer_id: t.offer_id.clone(),
            version: t.version,
            you: me.items.clone(),
            them: them.items.clone(),
            you_gold: me.gold,
            them_gold: them.gold,
            you_locked: me.locked,
            them_locked: them.locked,
            // §5: `trade.value_hint` — оценка стоимости (сумма gold_value).
            value_hint: Some(
                me.items
                    .iter()
                    .chain(them.items.iter())
                    .filter_map(|i| i.gold_value)
                    .sum::<i64>()
                    + me.gold
                    + them.gold,
            ),
            extra: Default::default(),
        })
    }

    /// Рассылка `evt/trade/state` обеим сторонам (каждой — её зеркало);
    /// вызывающая сторона получает свой текст через возвращаемый `Out`.
    fn trade_broadcast_state(&self, sv: u64) {
        let (a_char, b_char, a_conn, b_conn) = {
            let w = self.w();
            let Some(t) = w.trade.as_ref() else {
                return;
            };
            (
                t.a.char_id.clone(),
                t.b.char_id.clone(),
                t.a.conn_id.clone(),
                t.b.conn_id.clone(),
            )
        };
        let w = self.w();
        for (view_char, conn) in [(a_char, a_conn), (b_char, b_conn)] {
            if conn == self.conn_id || conn.is_empty() {
                continue; // свой клиент — через Out (порядок §2.4)
            }
            if let Some(v) = self.trade_view_for(&w, &view_char) {
                if let Ok(v) = serde_json::to_value(v) {
                    let text = Self::w_event("evt/trade/state", Some(sv), v);
                    w.send_to_conn(&conn, text);
                }
            }
        }
    }

    /// Перспектива для произвольного char_id (через готовый мир).
    fn trade_view_for(&self, w: &crate::world::World, my_char: &str) -> Option<TradeState> {
        let t = w.trade.as_ref()?;
        let (me, them) = if t.a.char_id == my_char {
            (&t.a, &t.b)
        } else {
            (&t.b, &t.a)
        };
        Some(TradeState {
            offer_id: t.offer_id.clone(),
            version: t.version,
            you: me.items.clone(),
            them: them.items.clone(),
            you_gold: me.gold,
            them_gold: them.gold,
            you_locked: me.locked,
            them_locked: them.locked,
            value_hint: Some(
                me.items
                    .iter()
                    .chain(them.items.iter())
                    .filter_map(|i| i.gold_value)
                    .sum::<i64>()
                    + me.gold
                    + them.gold,
            ),
            extra: Default::default(),
        })
    }

    fn trade_open(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeOpen>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let my_conn = self.conn_id.clone();
        // Партнёр должен быть онлайн (DoD M8: два клиента) — иначе фолбэк
        // на «мок-партнёра» (старое одиночное поведение без `them`).
        let partner = {
            let w = self.w();
            w.player_by_char(&cmd.player_id).cloned()
        };
        // Проверка «в одной локации» (ФР-15): сервер — истина.
        if let Some(p) = partner.as_ref() {
            if p.char_id != me && p.location_id != self.location.id {
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::NotInLocation,
                    "партнёр в другой локации",
                )];
            }
        }
        let sv = self.bump();
        let offer_id = format!("of_{me}");
        let (partner_char, partner_conn) = match partner {
            Some(p) if p.char_id != me => (p.char_id, p.conn_id),
            // Фолбэк: одиночная сделка с фиктивным партнёром (M1-семантика).
            _ => ("npc".into(), String::new()),
        };
        let (own_view, partner_view) = {
            let mut w = self.w();
            w.trade = Some(crate::world::TradeShared {
                offer_id: offer_id.clone(),
                a: crate::world::TradeSide {
                    char_id: me.clone(),
                    conn_id: my_conn.clone(),
                    items: vec![],
                    gold: 0,
                    locked: false,
                },
                b: crate::world::TradeSide {
                    char_id: partner_char.clone(),
                    conn_id: partner_conn.clone(),
                    items: vec![],
                    gold: 0,
                    locked: false,
                },
                opener: me.clone(),
                version: 1,
            });
            let own = self.trade_view_for(&w, &me);
            let theirs = if partner_conn.is_empty() {
                None
            } else {
                self.trade_view_for(&w, &partner_char)
            };
            (own, theirs)
        };
        // Партнёру — его зеркало сразу (свой клиент получит через Out).
        if let (Some(v), true) = (partner_view, !partner_conn.is_empty()) {
            if let Ok(v) = serde_json::to_value(v) {
                let text = Self::w_event("evt/trade/state", Some(sv), v);
                self.w().send_to_conn(&partner_conn, text);
            }
        }
        vec![
            self.ack_ok_payload(seq, Some(sv), &json!({ "offer_id": offer_id })),
            self.event(
                "evt/trade/state",
                Some(sv),
                serde_json::to_value(own_view).unwrap_or_default(),
            ),
        ]
    }

    fn trade_add_item(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeAddItem>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let item = self
            .char
            .as_ref()
            .and_then(|c| c.inventory.iter().find(|i| i.uid == cmd.item_uid))
            .cloned();
        let Some(item) = item else {
            return vec![self.ack_err(seq, None, ErrorCode::ItemGone, "предмета нет")];
        };
        let sv = self.bump();
        {
            let mut w = self.w();
            let Some(t) = w.trade.as_mut() else {
                return vec![self.ack_err(seq, None, ErrorCode::TradePartnerGone, "нет сделки")];
            };
            let side = if t.a.char_id == me {
                &mut t.a
            } else {
                &mut t.b
            };
            side.items.push(item);
            t.version += 1;
        }
        let view = self.trade_view(&me);
        self.trade_broadcast_state(sv);
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event(
                "evt/trade/state",
                Some(sv),
                serde_json::to_value(view).unwrap_or_default(),
            ),
        ]
    }

    fn trade_set_gold(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeSetGold>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.gold < 0 {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "золото < 0")];
        }
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let sv = self.bump();
        {
            let mut w = self.w();
            let Some(t) = w.trade.as_mut() else {
                return vec![self.ack_err(seq, None, ErrorCode::TradePartnerGone, "нет сделки")];
            };
            let side = if t.a.char_id == me {
                &mut t.a
            } else {
                &mut t.b
            };
            side.gold = cmd.gold;
            t.version += 1;
        }
        let view = self.trade_view(&me);
        self.trade_broadcast_state(sv);
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event(
                "evt/trade/state",
                Some(sv),
                serde_json::to_value(view).unwrap_or_default(),
            ),
        ]
    }

    /// `propose`/`accept`: сверка общей версии (§8), фиксация своей стороны;
    /// обе стороны зафиксированы → обмен + `evt/trade/closed {done}`.
    fn trade_lock(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Lock {
            offer_id: String,
            version: u64,
        }
        let Ok(cmd) = Self::payload_of::<Lock>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let me = self.char.as_ref().map(|c| c.id.clone()).unwrap_or_default();
        let sv = self.bump();

        // Расчёт обмена при завершении: что я отдаю/получаю.
        let mut apply: Option<crate::world::SessionCmd> = None;
        let mut done: Option<String> = None;
        {
            let mut w = self.w();
            let Some(t) = w.trade.as_mut() else {
                return vec![self.ack_err(seq, None, ErrorCode::TradePartnerGone, "нет сделки")];
            };
            if t.offer_id != cmd.offer_id || t.version != cmd.version {
                let actual = t.version;
                return vec![self.ack_err(
                    seq,
                    None,
                    ErrorCode::TradeStale,
                    format!("версия сделки устарела (сейчас {actual})"),
                )];
            }
            let side = if t.a.char_id == me {
                &mut t.a
            } else {
                &mut t.b
            };
            side.locked = true;
            t.version += 1;
            if t.a.locked && t.b.locked {
                // Обмен: мои items → партнёру, его items → мне (§8 result).
                let (a_id, _b_id, a_conn, b_conn, offer_id) = (
                    t.a.char_id.clone(),
                    t.b.char_id.clone(),
                    t.a.conn_id.clone(),
                    t.b.conn_id.clone(),
                    t.offer_id.clone(),
                );
                let (a_items, b_items) = (t.a.items.clone(), t.b.items.clone());
                let a_loss: Vec<String> = a_items.iter().map(|i| i.uid.clone()).collect();
                let b_loss: Vec<String> = b_items.iter().map(|i| i.uid.clone()).collect();
                // Своё применение (эта сессия).
                let mine_gain = if a_id == me {
                    b_items.clone()
                } else {
                    a_items.clone()
                };
                let mine_loss = if a_id == me {
                    a_loss.clone()
                } else {
                    b_loss.clone()
                };
                apply = Some(crate::world::SessionCmd::TradeApply {
                    gain: mine_gain,
                    loss: mine_loss,
                });
                // Партнёру — его команда в его сокет.
                let partner_gain = if a_id == me { a_items } else { b_items };
                let partner_loss = if a_id == me { b_loss } else { a_loss };
                let partner_conn = if a_id == me { b_conn } else { a_conn };
                if !partner_conn.is_empty() {
                    w.send_cmd(
                        &partner_conn,
                        crate::world::SessionCmd::TradeApply {
                            gain: partner_gain,
                            loss: partner_loss,
                        },
                    );
                }
                done = Some(offer_id);
                w.trade = None;
            }
        }

        let closed_text = done.as_ref().map(|offer_id| {
            Self::w_event(
                "evt/trade/closed",
                Some(sv),
                serde_json::to_value(TradeClosed {
                    offer_id: offer_id.clone(),
                    reason: TradeCloseReason::Done,
                    result: None,
                })
                .unwrap_or_default(),
            )
        });

        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        if let Some(text) = closed_text {
            // Рассылка `done` партнёру, свой клиент — через Out.
            let w = self.w();
            w.broadcast(text.clone(), Some(&self.conn_id));
            outs.push(Out::Text(text));
        } else {
            let view = self.trade_view(&me);
            self.trade_broadcast_state(sv);
            outs.push(self.event(
                "evt/trade/state",
                Some(sv),
                serde_json::to_value(view).unwrap_or_default(),
            ));
        }
        // Применение обмена к своему персонажу (после событий, §2.4).
        if let Some(cmd) = apply {
            self.apply_session_cmd(cmd);
        }
        outs
    }

    fn trade_cancel(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Only {
            offer_id: String,
        }
        let Ok(cmd) = Self::payload_of::<Only>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let sv = self.bump();
        {
            let mut w = self.w();
            w.trade = None;
        }
        let closed = Self::w_event(
            "evt/trade/closed",
            Some(sv),
            serde_json::to_value(TradeClosed {
                offer_id: cmd.offer_id,
                reason: TradeCloseReason::Cancelled,
                result: None,
            })
            .unwrap_or_default(),
        );
        let w = self.w();
        w.broadcast(closed.clone(), Some(&self.conn_id));
        vec![self.ack_ok(seq, Some(sv)), Out::Text(closed)]
    }

    /// Публичная точка для сервера: применение команды из [`crate::world::World`]
    /// (вызывается в `select!`-цикле соединения).
    pub fn apply_cmd(&mut self, cmd: crate::world::SessionCmd) -> Vec<Out> {
        self.apply_session_cmd(cmd);
        Vec::new() // события ушли в свой сокет через World (порядок §2.4)
    }

    /// Применение [`crate::world::SessionCmd`] к своему персонажу: обмен по
    /// сделке → события `evt/item/removed|added` в свой сокет.
    fn apply_session_cmd(&mut self, cmd: crate::world::SessionCmd) {
        match cmd {
            crate::world::SessionCmd::TradeApply { gain, loss } => {
                let mut evs: Vec<String> = Vec::new();
                if let Some(ch) = self.char.as_mut() {
                    ch.inventory.retain(|i| !loss.contains(&i.uid));
                }
                for uid in &loss {
                    evs.push(Self::w_event(
                        "evt/item/removed",
                        None,
                        json!({ "uid": uid, "reason": "traded" }),
                    ));
                }
                for item in &gain {
                    let ev = Self::w_event(
                        "evt/item/added",
                        None,
                        serde_json::to_value(ld2_protocol::evt::ItemAdded { item: item.clone() })
                            .unwrap_or_default(),
                    );
                    evs.push(ev);
                    if let Some(ch) = self.char.as_mut() {
                        ch.inventory.push(item.clone());
                    }
                }
                // Свой сокет: через World (порядок removed → added сохранён).
                let conn = self.conn_id.clone();
                let w = self.w();
                for ev in evs {
                    w.send_to_conn(&conn, ev);
                }
            }
        }
    }

    // ── гильдия (состояние в World, рассылка участникам, ФР-16) ────────────

    fn guild_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Only {
            name: String,
        }
        let Ok(cmd) = Self::payload_of::<Only>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let mut guild = fixtures::guild_fixture();
        guild.name = cmd.name;
        let sv = self.bump();
        {
            let mut w = self.w();
            w.guild = Some(guild.clone());
        }
        let payload = json!({ "guild": guild });
        let text = Self::w_event("evt/guild/updated", Some(sv), payload.clone());
        {
            let w = self.w();
            w.broadcast(text, Some(&self.conn_id));
        }
        vec![
            self.ack_ok_payload(seq, Some(sv), &json!({ "guild": guild })),
            self.event("evt/guild/updated", Some(sv), payload),
        ]
    }

    fn guild_join(&mut self, seq: u32) -> Vec<Out> {
        let sv = self.bump();
        {
            let mut w = self.w();
            w.guild = Some(fixtures::guild_fixture());
        }
        let payload = json!({ "guild": self.w().guild.clone() });
        let text = Self::w_event("evt/guild/updated", Some(sv), payload.clone());
        {
            let w = self.w();
            w.broadcast(text, Some(&self.conn_id));
        }
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event("evt/guild/updated", Some(sv), payload),
        ]
    }

    fn guild_leave(&mut self, seq: u32) -> Vec<Out> {
        let sv = self.bump();
        {
            let mut w = self.w();
            w.guild = None;
        }
        let text = Self::w_event("evt/guild/updated", Some(sv), json!({ "guild": null }));
        {
            let w = self.w();
            w.broadcast(text, Some(&self.conn_id));
        }
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event("evt/guild/updated", Some(sv), json!({ "guild": null })),
        ]
    }

    /// Приглашение в гильдию (§7): онлайн-игрок получает `evt/guild/updated`
    /// c опциональным полем `invite` (§11 — совместимо без bump).
    fn guild_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Only {
            player_id: String,
        }
        let Ok(cmd) = Self::payload_of::<Only>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let sv = self.bump();
        let target = {
            let w = self.w();
            w.player_by_char(&cmd.player_id).cloned()
        };
        match target {
            Some(p) => {
                let payload = json!({
                    "guild": serde_json::Value::Null,
                    "invite": {
                        "guild_id": self.w().guild.as_ref().map(|g| g.id.clone()).unwrap_or_else(|| "g_1".into()),
                        "guild_name": self.w().guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| "Стальные волки".into()),
                        "from_name": self.char.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                    }
                });
                let text = Self::w_event("evt/guild/updated", Some(sv), payload);
                self.w().send_to_conn(&p.conn_id, text);
                vec![self.ack_ok(seq, Some(sv))]
            }
            None => vec![self.ack_err(seq, None, ErrorCode::TargetGone, "игрок не в сети")],
        }
    }

    /// Исключение из гильдии: участнику — `guild: null`, остальным — обновление.
    fn guild_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_char(seq) {
            return vec![err];
        }
        #[derive(serde::Deserialize)]
        struct Only {
            player_id: String,
        }
        let Ok(cmd) = Self::payload_of::<Only>(env, seq) else {
            return vec![self.ack_err(seq, None, ErrorCode::Internal, "некорректный payload")];
        };
        let sv = self.bump();
        // Мок-гильдия одностранична: исключение = выгрузка жертвы + журнал.
        let text_null = Self::w_event("evt/guild/updated", Some(sv), json!({ "guild": null }));
        let text_log = Self::w_event(
            "evt/guild/log",
            None,
            json!({ "entry": GuildLogEntry {
                kind: "leave".into(),
                text: format!("Исключён игрок {}", cmd.player_id),
                at: fixtures::now_epoch(),
                extra: Default::default(),
            }}),
        );
        {
            let w = self.w();
            if let Some(p) = w.player_by_char(&cmd.player_id) {
                w.send_to_conn(&p.conn_id, text_null);
            }
            w.broadcast(text_log, Some(&self.conn_id));
        }
        vec![self.ack_ok(seq, Some(sv))]
    }

    fn guild_rank_set(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = Self::payload_of::<GuildRankSet>(env, seq).err() {
            return vec![err];
        }
        let cmd = Self::payload_of::<GuildRankSet>(env, seq).expect("проверено выше");
        let sv = self.bump();
        // Реальная смена ранга в состоянии (§7 атомарно, ФР-16).
        {
            let mut w = self.w();
            if let Some(g) = w.guild.as_mut() {
                if let Some(m) = g.members.iter_mut().find(|m| m.id == cmd.player_id) {
                    m.rank = cmd.rank.clone();
                }
            }
        }
        let entry = GuildLogEntry {
            kind: "rank_change".into(),
            text: format!("Ранг {} → {}", cmd.player_id, cmd.rank),
            at: fixtures::now_epoch(),
            extra: Default::default(),
        };
        let text_log = Self::w_event("evt/guild/log", Some(sv), json!({ "entry": entry }));
        {
            let w = self.w();
            w.broadcast(text_log, Some(&self.conn_id));
        }
        vec![
            self.ack_ok(seq, Some(sv)),
            self.event(
                "evt/guild/log",
                Some(sv),
                json!({ "entry": GuildLogEntry {
                    kind: "rank_change".into(),
                    text: format!("Ранг {} → {}", cmd.player_id, cmd.rank),
                    at: fixtures::now_epoch(),
                    extra: Default::default(),
                }}),
            ),
        ]
    }

    // ── состояние ───────────────────────────────────────────────────────────

    fn state_resync(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = Self::payload_of::<StateResync>(env, seq).err() {
            return vec![err];
        }
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let sv = self.w().version();
        let mut outs = vec![self.ack_ok(seq, Some(sv))];
        if let Some(ch) = self.char.as_ref() {
            // §3: mock без дельт — всегда полный снапшот.
            let mut snap = fixtures::snapshot(sv, ch, &self.location);
            // §7: снапшот несёт межигровое состояние (общее для клиентов).
            {
                let w = self.w();
                snap.party = w.party.clone();
                snap.guild = w.guild.clone();
            }
            outs.push(self.event(
                "evt/state/snapshot",
                Some(sv),
                serde_json::to_value(snap).unwrap_or_default(),
            ));
        }
        outs
    }
}

/// Отключение соединения: сохраняем состояние аккаунта (персонаж переживает
/// обрыв — §4.5) и отвязываем канал от [`crate::world::World`].
impl Drop for Session {
    fn drop(&mut self) {
        let mut w = self.world.lock().expect("mutex мира");
        if let Some(login) = self.login.clone() {
            w.save_home(
                &login,
                crate::world::Home {
                    chars: self.chars.clone(),
                    char: self.char.clone(),
                    location: self.location.clone(),
                    crime_log: self.crime_log.clone(),
                },
            );
        }
        w.unregister_conn(&self.conn_id);
    }
}

// ── вспомогательные типы ────────────────────────────────────────────────────

/// Хелпер для доступа к руне сокета.
trait SocketRuneExt {
    fn and_some_rune(&self) -> Option<&RuneInfo>;
}

impl SocketRuneExt for Option<&ld2_protocol::types::Socket> {
    fn and_some_rune(&self) -> Option<&RuneInfo> {
        self.and_then(|s| s.rune.as_ref())
    }
}

/// Порог XP навыка — формула skills.md §2.2: `round(120 · 1.32^rank)`.
trait XpNext {
    fn xp_next_threshold(&self) -> i64;
}

impl XpNext for SkillSnapshot {
    fn xp_next_threshold(&self) -> i64 {
        (120.0 * 1.32f64.powi(self.rank as i32)).round() as i64
    }
}

fn out_ack(ack: Ack) -> Out {
    Out::Text(serde_json::to_string(&ack).unwrap_or_default())
}

fn out_event(ty: &str, state_version: Option<u64>, payload: Value) -> Out {
    let env = Envelope {
        v: PROTOCOL_VERSION,
        msg_type: ty.to_string(),
        seq: None,
        ts: Some(fixtures::now_rfc3339()),
        state_version,
        payload: Some(payload),
    };
    Out::Text(serde_json::to_string(&env).unwrap_or_default())
}
