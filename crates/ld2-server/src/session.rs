//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{
    AuthLogin, AuthRegister, AuthResume, CharCreate, CharDelete, CharSelect, CharXpInvest,
    ChatDelete, ChatSend, CombatAttack, CraftLearn, CraftRecipes, CraftStart, CrimeLog, EntityInfo,
    GuildBrowse, GuildCreate, GuildInvite, GuildJoin, GuildKick, GuildRankSet, ItemDrop, ItemEquip,
    ItemMove, ItemUnequip, ItemUse, LocationMove, MagicRuneCast, MagicScrollCast, PartyAccept,
    PartyInvite, PartyKick, SkillUse, SocketExtract, SocketInsert, StateResync, TradeAddItem,
    TradeCancel, TradeOpen, TradePropose, TradeSetGold, UpgradePreview, UpgradeStart,
};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{
    CastSource, CharStats, CharXp, CombatCooldown, CombatHit, CombatKind, CraftProfession,
    CrimeAdded, EntityHp, ItemAdded, ItemRemovalReason, ItemRemoved, ItemState, LocationChanged,
    MagicCast, Pong, StateSnapshot, TimeSync, UpgradeResult,
};
use ld2_protocol::types::{
    CharacterState, CharacterSummary, CrimeLogEntry, CrimeState, CrimeStatus, Entity, EquipMap, Hp,
    ItemSnapshot, LocationSnapshot, Mp, ProfessionSnapshot, Requires, Severity, SkillSnapshot,
    Socket,
};
use ld2_protocol::ErrorCode;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::warn;

use crate::auth;
use crate::content::ContentInput;
use crate::db;
use crate::state::SharedState;
use crate::trainers;

/// Исходящее сообщение соединения (как в моке: ack → события → опц. закрытие).
#[derive(Debug)]
pub enum Out {
    Text(String),
    Close,
}

pub struct Session {
    state: SharedState,
    peer: SocketAddr,
    /// §2.4.2: последние 128 обработанных `seq` → точный кэшированный ack.
    ack_cache: HashMap<u32, String>,
    seq_order: VecDeque<u32>,
    /// Аккаунт соединения (ADR-006): `Some` после login/register/resume.
    account_id: Option<i64>,
    /// M14/F19: роль аккаунта — права на system-чат/удаление (§9 E_PERMISSION).
    role: String,
    #[allow(dead_code)] // журнал/логика персонажей — M12.2 (фаза «персонажи»)
    login: Option<String>,
    authenticated: bool,
    /// Выбранный персонаж (`char.select`, §3): снапшот и ack-версии — его.
    selected_char_id: Option<String>,
    char: Option<CharacterState>,
    /// Текущая локация персонажа (M12.3): разрешается при входе/переходе.
    location_id: Option<String>,
    /// Канал исходящих конвертов соединения (рассылка тика боя, M12.5).
    out_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Активное превью улучшения (ФР-10, M12.5c): stale → `E_UPGRADE_PREVIEW_STALE`.
    preview_state: Option<UpgradePreviewState>,
    /// Кулдаун чата (`chat.cooldown_s`, ФР-13): момент последнего сообщения.
    last_chat_at: Option<std::time::Instant>,
}

/// Превью улучшения (одно на соединение; уровень изменился → stale).
#[derive(Debug, Clone)]
struct UpgradePreviewState {
    preview_id: String,
    item_uid: String,
    /// Уровень на момент превью.
    level: i32,
}

impl Session {
    pub fn new(
        state: SharedState,
        peer: SocketAddr,
        out_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            state,
            peer,
            out_tx,
            ack_cache: HashMap::new(),
            seq_order: VecDeque::new(),
            account_id: None,
            role: "player".to_string(),
            login: None,
            authenticated: false,
            selected_char_id: None,
            char: None,
            location_id: None,
            preview_state: None,
            last_chat_at: None,
        }
    }

    /// Входящий текст → исходящие (ровно один ack на команду, §2.2).
    pub async fn on_text(&mut self, text: &str) -> Vec<Out> {
        let env: Envelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(e) => {
                warn!("неразборчивое сообщение: {e}");
                return vec![];
            }
        };
        // §11: сервер говорит только v=1; более новое не разбираем — игнор.
        if env.v != PROTOCOL_VERSION {
            warn!("конверт v={} (ожидается {PROTOCOL_VERSION}) — игнор", env.v);
            return vec![];
        }
        if !env.msg_type.starts_with("cmd/") {
            warn!("не-команда от клиента: {}", env.msg_type);
            return vec![];
        }
        // §2.4.2: повтор seq → точный кэшированный ack, без повторных событий.
        if let Some(seq) = env.seq {
            if let Some(cached) = self.ack_cache.get(&seq) {
                return vec![Out::Text(cached.clone())];
            }
        }
        let outs = self.dispatch(&env).await;
        // Первый ответ диспетчера — всегда ack (§2.2); кэшируем его текст.
        if let (Some(seq), Some(Out::Text(ack_text))) = (env.seq, outs.first()) {
            self.ack_cache.insert(seq, ack_text.clone());
            self.seq_order.push_back(seq);
            while self.seq_order.len() > 128 {
                if let Some(old) = self.seq_order.pop_front() {
                    self.ack_cache.remove(&old);
                }
            }
        }
        outs
    }

    async fn dispatch(&mut self, env: &Envelope) -> Vec<Out> {
        let seq = env.seq.unwrap_or(0);
        match env.msg_type.as_str() {
            // auth / session (ADR-006, §4.5)
            "cmd/auth.register" => self.auth_register(env, seq).await,
            "cmd/auth.login" => self.auth_login(env, seq).await,
            "cmd/auth.resume" => self.auth_resume(env, seq).await,
            "cmd/auth.logout" => self.auth_logout(seq).await,
            "cmd/session.refresh" => self.auth_refresh(seq).await,
            // персонажи (ФР-02, §5/§3)
            "cmd/char.list" => self.char_list(seq).await,
            "cmd/char.create" => self.char_create(env, seq).await,
            "cmd/char.select" => self.char_select(env, seq).await,
            "cmd/char.delete" => self.char_delete(env, seq).await,
            // мир (ФР-03, M12.3)
            "cmd/location.look" => self.location_look(seq).await,
            "cmd/location.move" => self.location_move(env, seq).await,
            "cmd/entity.info" => self.entity_info(env, seq).await,
            "cmd/state.resync" => self.state_resync(env, seq).await,
            // бой (ФР-05, M12.5)
            "cmd/combat.attack" => self.combat_attack(env, seq).await,
            "cmd/combat.stop" => self.combat_stop(seq).await,
            "cmd/combat.flee" => self.combat_flee(seq).await,
            // гильдия (ФР-16, M12.7d)
            "cmd/guild.browse" => self.guild_browse(env, seq).await,
            "cmd/guild.create" => self.guild_create(env, seq).await,
            "cmd/guild.join" => self.guild_join(env, seq).await,
            "cmd/guild.leave" => self.guild_leave(seq).await,
            "cmd/guild.invite" => self.guild_invite(env, seq).await,
            "cmd/guild.kick" => self.guild_kick(env, seq).await,
            "cmd/guild.rank_set" => self.guild_rank_set(env, seq).await,
            // сделка (ФР-15, M12.7c)
            "cmd/trade.open" => self.trade_open(env, seq).await,
            "cmd/trade.add_item" => self.trade_add_item(env, seq).await,
            "cmd/trade.set_gold" => self.trade_set_gold(env, seq).await,
            "cmd/trade.propose" => self.trade_lock_cmd(env, seq).await,
            "cmd/trade.accept" => self.trade_lock_cmd(env, seq).await,
            "cmd/trade.cancel" => self.trade_cancel(env, seq).await,
            // группа (ФР-14, M12.7b)
            "cmd/party.create" => self.party_create(seq).await,
            "cmd/party.invite" => self.party_invite(env, seq).await,
            "cmd/party.accept" => self.party_accept(env, seq).await,
            "cmd/party.decline" => self.party_decline(env, seq).await,
            "cmd/party.leave" => self.party_leave(seq).await,
            "cmd/party.kick" => self.party_kick(env, seq).await,
            // профессии/крафт (ФР-08, M12.6)
            "cmd/craft.list" => self.craft_list(seq).await,
            "cmd/craft.recipes" => self.craft_recipes(env, seq).await,
            "cmd/craft.learn" => self.craft_learn(env, seq).await,
            "cmd/craft.start" => self.craft_start(env, seq).await,
            // квесты (ФР-12, M12.6)
            "cmd/quest.list" => self.quest_list(seq).await,
            "cmd/quest.accept" => self.quest_accept(env, seq).await,
            "cmd/quest.turn_in" => self.quest_turn_in(env, seq).await,
            "cmd/quest.abandon" => self.quest_abandon(env, seq).await,
            // преступность (ФР-11, §10, M12.5d)
            "cmd/crime.log" => self.crime_log(env, seq).await,
            // улучшение предметов (ФР-10, M12.5c)
            "cmd/upgrade.preview" => self.upgrade_preview(env, seq).await,
            "cmd/upgrade.start" => self.upgrade_start(env, seq).await,
            // активные способности (§4, M14)
            "cmd/skill.use" => self.skill_use(env, seq).await,
            // магия и сокеты (ФР-09, M12.5b)
            "cmd/magic.rune_cast" => self.magic_rune_cast(env, seq).await,
            "cmd/magic.scroll_cast" => self.magic_scroll_cast(env, seq).await,
            "cmd/socket.insert" => self.socket_insert(env, seq).await,
            "cmd/socket.extract" => self.socket_extract(env, seq).await,
            // лист/инвентарь/опыт (ФР-04/07, M12.4)
            "cmd/char.xp_invest" => self.char_xp_invest(env, seq).await,
            "cmd/item.use" => self.item_use(env, seq).await,
            "cmd/item.equip" => self.item_equip(env, seq).await,
            "cmd/item.unequip" => self.item_unequip(env, seq).await,
            "cmd/item.drop" => self.item_drop(env, seq).await,
            "cmd/item.move" => self.item_move(env, seq).await,
            // dev-сценарии (`/scenario …`); чат — ФР-13 в фазе M12.7
            "cmd/chat.send" => self.chat_send(env, seq).await,
            "cmd/chat.delete" => self.chat_delete(env, seq).await,
            // Сервисный ping (§7): ack + echo; RTT клиент считает по своему
            // `ts` — сервер отвечает 0, как и мок.
            "cmd/ping" => {
                let pong = Pong {
                    ts: crate::time::now_rfc3339(),
                    rtt_ms: 0,
                };
                vec![
                    out_ack(Ack::ok(seq, None)),
                    out_event(
                        "evt/pong",
                        None,
                        serde_json::to_value(&pong).unwrap_or_default(),
                    ),
                ]
            }
            "cmd/time.sync" => {
                let sync = TimeSync {
                    server_ts: crate::time::now_epoch(),
                    rtt_hint_ms: Some(0),
                };
                vec![
                    out_ack(Ack::ok(seq, None)),
                    out_event(
                        "evt/time.sync",
                        None,
                        serde_json::to_value(&sync).unwrap_or_default(),
                    ),
                ]
            }
            other => {
                warn!("неизвестная команда {other}");
                vec![out_ack(Ack::error(
                    seq,
                    None,
                    &ErrorCode::Internal,
                    format!("неизвестная команда: {other}"),
                ))]
            }
        }
    }

    // ── auth (ADR-006, D29/D30) ─────────────────────────────────────────────

    /// Регистрация = немедленный вход (ADR-006); лимиты — из конфига (D25).
    async fn auth_register(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(retry) = self.rate_limit() {
            return vec![Self::ack_rate_limited(seq, retry)];
        }
        let Ok(cmd) = Self::payload_of::<AuthRegister>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let auth_cfg = &self.state.config.auth;
        if !auth_cfg.register_enabled {
            return vec![Self::err(
                seq,
                ErrorCode::RegisterDisabled,
                "регистрация временно недоступна",
            )];
        }
        let (login_len, password_len) = (cmd.login.chars().count(), cmd.password.chars().count());
        if login_len < auth_cfg.min_login_len || password_len < auth_cfg.min_password_len {
            return vec![Self::err(
                seq,
                ErrorCode::WeakPassword,
                format!(
                    "логин — от {} символов, пароль — от {}",
                    auth_cfg.min_login_len, auth_cfg.min_password_len
                ),
            )];
        }
        // Email: нормализация + синтаксис (D33, ADR-007) — до любых
        // обращений к БД (serde уже гарантировал наличие поля).
        let email = auth::normalize_email(&cmd.email);
        if !auth::validate_email(&email) {
            return vec![Self::err(
                seq,
                ErrorCode::BadEmail,
                "некорректный email (ожидается name@domain.tld)",
            )];
        }
        // Дешёвая проверка занятости: argon2 дорогой (защита от DoS).
        match db::accounts::login_exists(&self.state.pool, &cmd.login).await {
            Ok(true) => {
                return vec![Self::err(seq, ErrorCode::LoginTaken, "логин занят")];
            }
            Ok(false) => {}
            Err(e) => {
                warn!("ошибка БД при регистрации: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        match db::accounts::email_exists(&self.state.pool, &email).await {
            Ok(true) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::EmailTaken,
                    "email уже зарегистрирован",
                )];
            }
            Ok(false) => {}
            Err(e) => {
                warn!("ошибка БД при регистрации: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let hash = auth::hash_password(&cmd.password);
        match db::accounts::create_account(&self.state.pool, &cmd.login, &hash, &email).await {
            Ok(Some(id)) => self.finish_login(seq, id, cmd.login).await,
            // Гонка двух регистраций: UNIQUE решает — логин или email занят.
            Ok(None) => {
                if db::accounts::email_exists(&self.state.pool, &email)
                    .await
                    .unwrap_or(false)
                {
                    vec![Self::err(
                        seq,
                        ErrorCode::EmailTaken,
                        "email уже зарегистрирован",
                    )]
                } else {
                    vec![Self::err(seq, ErrorCode::LoginTaken, "логин занят")]
                }
            }
            Err(e) => {
                // Частичный UNIQUE по email (не по login) тоже возможен в гонке.
                if let sqlx::Error::Database(db_err) = &e {
                    if db_err.constraint() == Some("accounts_email_idx") {
                        return vec![Self::err(
                            seq,
                            ErrorCode::EmailTaken,
                            "email уже зарегистрирован",
                        )];
                    }
                }
                warn!("ошибка БД при регистрации: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    async fn auth_login(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(retry) = self.rate_limit() {
            return vec![Self::ack_rate_limited(seq, retry)];
        }
        let Ok(cmd) = Self::payload_of::<AuthLogin>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account = match db::accounts::find_for_login(&self.state.pool, &cmd.login).await {
            Ok(Some(account)) => account,
            // Бан маскируется под неверные учётные данные (D30): find_for_login
            // не возвращает забаненных — факт бана не раскрываем.
            Ok(None) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::BadCredentials,
                    "неверный логин или пароль",
                )];
            }
            Err(e) => {
                warn!("ошибка БД при входе: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if !auth::verify_password(&cmd.password, &account.password_hash) {
            return vec![Self::err(
                seq,
                ErrorCode::BadCredentials,
                "неверный логин или пароль",
            )];
        }
        self.finish_login(seq, account.id, cmd.login).await
    }

    async fn auth_resume(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(retry) = self.rate_limit() {
            return vec![Self::ack_rate_limited(seq, retry)];
        }
        let Ok(cmd) = Self::payload_of::<AuthResume>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.refresh_token.is_empty() {
            return vec![Self::err(
                seq,
                ErrorCode::AuthExpired,
                "refresh-токен недействителен",
            )];
        }
        let hash = auth::token_hash(&cmd.refresh_token);
        match db::accounts::account_by_refresh(&self.state.pool, &hash).await {
            Ok(Some((account_id, login))) => self.finish_login(seq, account_id, login).await,
            Ok(None) => {
                vec![Self::err(
                    seq,
                    ErrorCode::AuthExpired,
                    "refresh-токен недействителен",
                )]
            }
            Err(e) => {
                warn!("ошибка БД при resume: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// `auth.logout` (§4.5): отзыв **всех** refresh аккаунта + снятие
    /// аутентификации соединения.
    async fn auth_logout(&mut self, seq: u32) -> Vec<Out> {
        if let Some(account_id) = self.account_id {
            if let Err(e) = db::accounts::revoke_refreshes(&self.state.pool, account_id).await {
                warn!("отзыв refresh не удался: {e}");
            }
        }
        // Сессия выходит из мира: бой и рассылка (M12.5).
        if let Some(char_id) = self.selected_char_id.take() {
            let mut world = self.state.world.lock().expect("мир");
            world.unbind_char(&char_id);
        }
        self.char = None;
        self.location_id = None;
        self.authenticated = false;
        self.account_id = None;
        self.role = "player".to_string();
        self.login = None;
        vec![out_ack(Ack::ok(seq, None))]
    }

    /// `session.refresh` (§7): ротация access-токена на живом соединении.
    async fn auth_refresh(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let (access, _) = auth::new_token();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "access_token": access,
                "expires_in_s": self.state.config.auth.access_ttl_s,
            }),
        )]
    }

    /// Общий хвост входа (login/register/resume): refresh в БД, аккаунт на
    /// соединении, ack + `evt/session.start` — целиком из конфига (D25).
    async fn finish_login(&mut self, seq: u32, account_id: i64, login: String) -> Vec<Out> {
        let cfg = self.state.config.clone();
        let (access, _) = auth::new_token();
        let (refresh, refresh_hash) = auth::new_token();
        let expires_at = crate::time::now_epoch() + cfg.auth.refresh_ttl_s as i64;
        if let Err(e) =
            db::accounts::issue_refresh(&self.state.pool, account_id, &refresh_hash, expires_at)
                .await
        {
            warn!("ошибка выдачи refresh: {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        if let Err(e) = db::accounts::touch_login(&self.state.pool, account_id).await {
            warn!("last_login_at не обновлён: {e}"); // best-effort, не ломает вход
        }
        self.account_id = Some(account_id);
        self.login = Some(login);
        self.authenticated = true;
        self.selected_char_id = None;
        self.char = None;
        self.location_id = None;
        // Восстановление последней сессии (§4.5): после resume клиент шлёт
        // `state.resync` и ждёт снапшот — персонаж должен быть выбран.
        self.restore_last_char(account_id).await;
        // §5 characters[]: живые персонажи аккаунта (ошибка — не ломает вход).
        let mut chars = Vec::new();
        match db::characters::list(&self.state.pool, account_id).await {
            Ok(rows) => {
                for row in &rows {
                    chars.push(self.summary_for(row).await);
                }
            }
            Err(e) => warn!("список персонажей при входе: {e}"),
        }
        // M14/F19: роль аккаунта — в session.start (права модерации);
        // ошибка БД маскируется под `player` (не блокирует вход).
        let role = db::accounts::role(&self.state.pool, account_id)
            .await
            .unwrap_or_else(|e| {
                warn!("роль аккаунта: {e}");
                "player".to_string()
            });
        self.role = role.clone();
        let config = auth::session_config(&cfg, access.clone(), refresh, chars, role);
        vec![
            Self::ack_payload(
                seq,
                &serde_json::json!({
                    "access_token": access,
                    "expires_in_s": cfg.auth.access_ttl_s,
                }),
            ),
            out_event(
                "evt/session.start",
                None,
                serde_json::to_value(&config).unwrap_or_default(),
            ),
        ]
    }

    // ── персонажи (ФР-02, §5/§3) ─────────────────────────────────────────────

    async fn char_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let account_id = self.account_id.expect("require_auth выше");
        match db::characters::list(&self.state.pool, account_id).await {
            Ok(rows) => {
                let mut characters = Vec::with_capacity(rows.len());
                for row in &rows {
                    characters.push(self.summary_for(row).await);
                }
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "characters": characters }),
                )]
            }
            Err(e) => {
                warn!("char.list: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    async fn char_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharCreate>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let pool = &self.state.pool;
        // Лимит слотов — из конфига (D25, DoD M2: конфиг, не код).
        let limit = i64::from(self.state.config.gameplay.char_slot_limit);
        match db::characters::count_alive(pool, account_id).await {
            Ok(count) if count >= limit => {
                return vec![Self::err(
                    seq,
                    ErrorCode::CharLimit,
                    "все слоты персонажей заняты",
                )];
            }
            Ok(_) => {}
            Err(e) => {
                warn!("char.create (лимит): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        if cmd.name.trim().chars().count() < 2 {
            return vec![Self::err(
                seq,
                ErrorCode::NameTaken,
                "имя занято или некорректно",
            )];
        }
        if !PROFILES.contains(&cmd.profile.as_str()) {
            return vec![Self::err(
                seq,
                ErrorCode::BadProfile,
                format!("неизвестный профиль: {}", cmd.profile),
            )];
        }
        match db::characters::create(pool, account_id, &cmd.name, &cmd.profile).await {
            Ok(Some(row)) => {
                // M17/F30e: стартовый ресурс по профилю (WoW): воин —
                // ярость с нуля, лучник — полная энергия; мана (маг) —
                // дефолт колонок.
                let res = &self.state.config.gameplay.resource;
                let resource = match cmd.profile.as_str() {
                    "warrior" => Some((0, res.rage_max as i32)),
                    "archer" => Some((res.energy_max as i32, res.energy_max as i32)),
                    _ => None,
                };
                if let Some((cur, max)) = resource {
                    if let Err(e) = db::characters::set_resource(pool, &row.id, cur, max).await {
                        warn!("char.create (ресурс): {e}");
                    }
                }
                let character = self.summary_for(&row).await;
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "character": character }),
                )]
            }
            Ok(None) => {
                vec![Self::err(
                    seq,
                    ErrorCode::NameTaken,
                    "имя занято или некорректно",
                )]
            }
            Err(e) => {
                warn!("char.create: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    async fn char_select(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharSelect>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let Some(row) =
            (match db::characters::find(&self.state.pool, account_id, &cmd.char_id).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("char.select: {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            })
        else {
            // Паритет с моком: не найден/чужой → «персонаж не найден».
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        // §3: state_version монотонен на сессию персонажа и переживает
        // reconnect — бамп атомарный в БД.
        let state_version =
            match db::characters::bump_state_version(&self.state.pool, &row.id).await {
                Ok(v) => v as u64,
                Err(e) => {
                    warn!("char.select (state_version): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        // M12.3: позиция (пустая → спавн с персистом, D27) и выбор —
        // для восстановления сессии при resume (§4.5).
        let (location_id, location) = self.resolve_location(&row.location_id, &row.id).await;
        let char_state = match self.character_state(&row).await {
            Ok(state) => state,
            Err(e) => {
                warn!("char.select (лист персонажа): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        self.char = Some(char_state);
        self.location_id = Some(location_id.clone());
        self.selected_char_id = Some(row.id.clone());
        // Реестр мира (M12.5): рассылка тика боя + мишени агрессии.
        {
            let mut world = self.state.world.lock().expect("мир");
            world.bind_char(&row.id, &row.name, self.out_tx.clone());
            world.set_char_location(&row.id, &location_id);
        }
        if let Some(account_id) = self.account_id {
            if let Err(e) = db::accounts::set_last_char(&self.state.pool, account_id, &row.id).await
            {
                warn!("last_char_id не обновлён: {e}");
            }
        }
        let party = self.state.social.lock().expect("соц").party_of(&row.id);
        let guild = self.guild_state_of(&row.id).await;
        let snapshot = StateSnapshot {
            state_version,
            char: self.char.clone().expect("выше"),
            location,
            party,
            guild,
            server_ts: crate::time::now_epoch(),
            extra: Default::default(),
        };
        let mut outs = vec![
            out_ack(Ack::ok(seq, Some(state_version))),
            out_event(
                "evt/state/snapshot",
                Some(state_version),
                serde_json::to_value(&snapshot).unwrap_or_default(),
            ),
        ];
        // ФР-13/§5: реплей истории чата (server_history) после снапшота.
        outs.extend(self.chat_history_events().await);
        outs
    }

    async fn char_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharDelete>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let pool = &self.state.pool;
        let found = match db::characters::find(pool, account_id, &cmd.char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("char.delete: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if found.is_none() {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        }
        match db::characters::count_alive(pool, account_id).await {
            Ok(count) if count <= 1 => {
                return vec![Self::err(
                    seq,
                    ErrorCode::CharLimit,
                    "нельзя удалить последнего персонажа",
                )];
            }
            Ok(_) => {}
            Err(e) => {
                warn!("char.delete (лимит): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        match db::characters::soft_delete(pool, account_id, &cmd.char_id).await {
            Ok(0) => vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")],
            Ok(_) => {
                if self.selected_char_id.as_deref() == Some(cmd.char_id.as_str()) {
                    self.selected_char_id = None;
                    self.char = None;
                }
                vec![out_ack(Ack::ok(seq, None))]
            }
            Err(e) => {
                warn!("char.delete: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    // ── мир (ФР-03, M12.3) ─────────────────────────────────────────────────

    /// Локация персонажа: по `location_id`; пустой → спавн из сида (персист,
    /// D27); нет в БД (контент не засеян/удалён) → заглушка (D26).
    async fn resolve_location(
        &self,
        location_id: &str,
        char_id: &str,
    ) -> (String, LocationSnapshot) {
        let pool = &self.state.pool;
        if !location_id.is_empty() {
            return match db::locations::get(pool, location_id).await {
                Ok(Some(row)) => (row.id.clone(), row.snapshot()),
                Ok(None) => {
                    warn!("локация {location_id} отсутствует в сиде — заглушка");
                    (location_id.to_string(), location_snapshot(location_id))
                }
                Err(e) => {
                    warn!("чтение локации {location_id}: {e}");
                    (location_id.to_string(), location_snapshot(location_id))
                }
            };
        }
        // Пустая позиция (старый/новый персонаж) → спавн из meta.
        let spawn = match db::locations::spawn_id(pool).await {
            Ok(Some(spawn)) => spawn,
            Ok(None) => String::new(),
            Err(e) => {
                warn!("спавн из meta: {e}");
                String::new()
            }
        };
        if spawn.is_empty() {
            return (String::new(), location_snapshot(""));
        }
        if let Err(e) = db::locations::assign_spawn(pool, char_id, &spawn).await {
            warn!("спавн не персистнут: {e}");
        }
        match db::locations::get(pool, &spawn).await {
            Ok(Some(row)) => (spawn, row.snapshot()),
            Ok(None) => {
                warn!("спавн-локация {spawn} отсутствует в БД");
                let stub = location_snapshot(&spawn);
                (spawn, stub)
            }
            Err(e) => {
                warn!("чтение спавна {spawn}: {e}");
                let stub = location_snapshot(&spawn);
                (spawn, stub)
            }
        }
    }

    /// Поднять последнего выбранного персонажа (если жив) + его локацию —
    /// resume → `state.resync` → снапшот (§3/§4.5).
    async fn restore_last_char(&mut self, account_id: i64) {
        let last = match db::accounts::last_char(&self.state.pool, account_id).await {
            Ok(last) => last,
            Err(e) => {
                warn!("last_char_id: {e}");
                return;
            }
        };
        let Some(last) = last else {
            return;
        };
        match db::characters::find(&self.state.pool, account_id, &last).await {
            Ok(Some(row)) => {
                let (location_id, _) = self.resolve_location(&row.location_id, &row.id).await;
                match self.character_state(&row).await {
                    Ok(state) => {
                        self.char = Some(state);
                        self.location_id = Some(location_id.clone());
                        self.selected_char_id = Some(row.id.clone());
                        let mut world = self.state.world.lock().expect("мир");
                        world.bind_char(&row.id, &row.name, self.out_tx.clone());
                        world.set_char_location(&row.id, &location_id);
                    }
                    Err(e) => warn!("восстановление листа персонажа: {e}"),
                }
            }
            Ok(None) => {} // удалён — клиент начнёт с экрана персонажей
            Err(e) => warn!("восстановление персонажа {last}: {e}"),
        }
    }

    /// Отображаемое имя локации для сводок §5: пустой id → имя спавна.
    async fn location_display(&self, location_id: &str) -> String {
        if !location_id.is_empty() {
            return match db::locations::location_name(&self.state.pool, location_id).await {
                Ok(Some(name)) => name,
                Ok(None) => location_id.to_string(),
                Err(e) => {
                    warn!("имя локации {location_id}: {e}");
                    location_id.to_string()
                }
            };
        }
        let spawn = db::locations::spawn_id(&self.state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if spawn.is_empty() {
            return String::new();
        }
        db::locations::location_name(&self.state.pool, &spawn)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Сводка §5 для строки БД — с именем локации из сида.
    async fn summary_for(&self, row: &db::characters::CharacterRow) -> CharacterSummary {
        let location = self.location_display(&row.location_id).await;
        summary_of(row, location)
    }

    /// `cmd/location.look` (§7): полный снапшот текущей локации.
    async fn location_look(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.location_id.clone().unwrap_or_default();
        let (location_id, location) = self.resolve_location(&current, &char_id).await;
        self.location_id = Some(location_id);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "location": location }),
        )]
    }

    /// `cmd/location.move` (§7): переход по выходу — персист позиции + бамп
    /// `state_version` (§3/D27), ack с новой локацией, затем
    /// `evt/location/changed` (журнал мира, ФР-17; доставка остальным — M12.7).
    async fn location_move(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<LocationMove>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.location_id.clone().unwrap_or_default();
        let Some(cur_row) = (match db::locations::get(&self.state.pool, &current).await {
            Ok(row) => row,
            Err(e) => {
                warn!("location.move (чтение {current}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ExitBlocked, "персонаж не в мире")];
        };
        let Some((_, target)) = cur_row.exit_target(&cmd.exit_id) else {
            return vec![Self::err(seq, ErrorCode::ExitBlocked, "выход недоступен")];
        };
        let state_version =
            match db::locations::move_char(&self.state.pool, &char_id, &target).await {
                Ok(v) => v as u64,
                Err(e) => {
                    warn!("location.move (персист): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        self.location_id = Some(target.clone());
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, &target);
            world.end_combat(&char_id); // смена локации прерывает бой (M12.5)
        }
        let location = match db::locations::get(&self.state.pool, &target).await {
            Ok(Some(row)) => row.snapshot(),
            Ok(None) => {
                warn!("целевая локация {target} отсутствует в сиде — заглушка");
                location_snapshot(&target)
            }
            Err(e) => {
                warn!("чтение локации {target}: {e}");
                location_snapshot(&target)
            }
        };
        let ack = match Ack::ok_with_payload(
            seq,
            Some(state_version),
            &serde_json::json!({ "location": location.clone() }),
        ) {
            Ok(ack) => out_ack(ack),
            Err(e) => Self::err(seq, ErrorCode::Internal, format!("encode: {e}")),
        };
        let changed = out_event(
            "evt/location/changed",
            Some(state_version),
            serde_json::to_value(LocationChanged { location }).unwrap_or_default(),
        );
        vec![ack, changed]
    }

    /// `cmd/entity.info` (§7): осмотр статической сущности текущей локации
    /// вместе с её лутом (однократное взятие, M12.4); динамические трупы —
    /// M12.5.
    async fn entity_info(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<EntityInfo>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };

        // Динамический труп (M12.5): живёт в мире, не в строке локации.
        let dyn_loot = {
            let world = self.state.world.lock().expect("мир");
            world
                .dynamic_corpse(&cmd.entity_id)
                .map(|c| (c.id.clone(), c.name.clone(), c.loot.clone()))
        };
        if let Some((corpse_id, corpse_name, slots)) = dyn_loot {
            let mut loot = Vec::new();
            let mut remaining = 0usize;
            for (i, def_id) in slots.iter().enumerate() {
                let Some(def_id) = def_id else {
                    continue; // слот уже взят
                };
                remaining += 1;
                let loot_uid = format!("loot:{corpse_id}:{i}");
                let def = match db::items::def(&self.state.pool, def_id).await {
                    Ok(Some(def)) => def,
                    Ok(None) => {
                        warn!("лут трупа {corpse_id}: нет определения {def_id}");
                        continue;
                    }
                    Err(e) => {
                        warn!("entity.info (определение {def_id}): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let row = db::items::ItemRow {
                    uid: loot_uid,
                    def_id: def_id.clone(),
                    qty: def.qty,
                    upgrade: 0,
                    equipped_slot: None,
                    sockets: serde_json::json!([]),
                };
                loot.push(item_snapshot(&row, &def));
            }
            let entity = Entity {
                id: corpse_id,
                kind: ld2_protocol::types::EntityKind::Corpse,
                name: corpse_name,
                hp: None,
                tags: None,
                was_player: None,
                aggro: None,
                extra: Default::default(),
            };
            return vec![Self::ack_payload(
                seq,
                &serde_json::json!({
                    "entity": entity,
                    "loot": loot,
                    "was_player": false,
                    "looted": !slots.is_empty() && remaining == 0,
                }),
            )];
        }

        // Статическая сущность из сида (D26) вместе с её лутом (однократно).
        let current = self.location_id.clone().unwrap_or_default();
        let entities = match db::locations::get(&self.state.pool, &current).await {
            Ok(Some(row)) => {
                serde_json::from_value::<Vec<Entity>>(row.entities.clone()).unwrap_or_default()
            }
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!("entity.info (чтение {current}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(entity) = entities.into_iter().find(|e| e.id == cmd.entity_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "сущность не найдена")];
        };
        let defs = entity_loot_defs(&entity);
        let was_player = entity.was_player.unwrap_or(false);
        let mut loot = Vec::new();
        let mut remaining = 0usize;
        for (i, def_id) in defs.iter().enumerate() {
            let loot_uid = format!("loot:{}:{}", entity.id, i);
            match db::items::loot_taken_exists(&self.state.pool, &loot_uid, &char_id).await {
                Ok(true) => continue,
                Ok(false) => remaining += 1,
                Err(e) => {
                    warn!("entity.info (лут {loot_uid}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            let def = match db::items::def(&self.state.pool, def_id).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    warn!("лут ссылается на неизвестное определение {def_id}");
                    continue;
                }
                Err(e) => {
                    warn!("entity.info (определение {def_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let row = db::items::ItemRow {
                uid: loot_uid,
                def_id: def_id.clone(),
                qty: def.qty,
                upgrade: 0,
                equipped_slot: None,
                sockets: serde_json::json!([]),
            };
            loot.push(item_snapshot(&row, &def));
        }
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "entity": entity,
                "loot": loot,
                "was_player": was_player,
                "looted": !defs.is_empty() && remaining == 0,
            }),
        )]
    }

    /// `cmd/state.resync` (§3): всегда полный снапшот (spec-легально: сервер
    /// «может достроить» delta, но полная замена стора — валидный ответ;
    /// генерация delta-патчей — отклонение M12.3). Версия — без бампа.
    async fn state_resync(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(_cmd) = Self::payload_of::<StateResync>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let account_id = self.account_id.expect("require_auth выше");
        let Some(row) = (match db::characters::find(&self.state.pool, account_id, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("state.resync: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        let state_version = row.state_version as u64;
        let (location_id, location) = self.resolve_location(&row.location_id, &row.id).await;
        self.location_id = Some(location_id);
        let char_state = match self.character_state(&row).await {
            Ok(state) => state,
            Err(e) => {
                warn!("state.resync (лист персонажа): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let party = self.state.social.lock().expect("соц").party_of(&row.id);
        let guild = self.guild_state_of(&row.id).await;
        let snapshot = StateSnapshot {
            state_version,
            char: char_state,
            location,
            party,
            guild,
            server_ts: crate::time::now_epoch(),
            extra: Default::default(),
        };
        vec![
            out_ack(Ack::ok(seq, Some(state_version))),
            out_event(
                "evt/state/snapshot",
                Some(state_version),
                serde_json::to_value(&snapshot).unwrap_or_default(),
            ),
        ]
    }

    /// Выбранный персонаж (для отвязки от мира при дисконнекте, M12.5).
    pub fn selected_char(&self) -> Option<String> {
        self.selected_char_id.clone()
    }

    /// Текущий персонаж для команд мира (§3: без него «не в мире»).
    fn require_char_id(&self, seq: u32) -> Result<String, Out> {
        self.selected_char_id
            .clone()
            .ok_or_else(|| Self::err(seq, ErrorCode::NotInLocation, "персонаж не выбран"))
    }

    // ── лист, инвентарь, опыт (ФР-04/07, M12.4) ────────────────────────────

    /// Полный лист персонажа §3: строка БД + атрибуты (skills.md §1) +
    /// **все** навыки контента (вложенные — со своим XP, нет — нули) +
    /// инвентарь/экипировка (слоты — из конфига, D25).
    async fn character_state(
        &self,
        row: &db::characters::CharacterRow,
    ) -> Result<CharacterState, sqlx::Error> {
        let pool = &self.state.pool;
        let mut attrs: BTreeMap<String, i64> =
            serde_json::from_value(row.attrs.clone()).unwrap_or_default();

        let mut learned = HashMap::new();
        for skill in db::skills::learned(pool, &row.id).await? {
            learned.insert(skill.skill_id, (skill.xp, skill.rank));
        }
        let mut skills = Vec::new();
        for skill in db::skills::content_skills(pool).await? {
            let (xp, rank) = learned.get(&skill.id).copied().unwrap_or((0, 0));
            let rank_u32 = rank.max(0) as u32;
            skills.push(SkillSnapshot {
                id: skill.id,
                name: Some(skill.name),
                // M14/F17: пустое описание старого сида → None (без поля).
                description: (!skill.description.is_empty()).then_some(skill.description),
                // M15/F28: группа (skills.md §3) — группировка вкладки.
                group: (!skill.skill_group.is_empty()).then_some(skill.skill_group),
                rank: rank_u32,
                xp,
                xp_to_next: crate::systems::xp_to_next(rank_u32, &self.state.config.skills),
            });
        }

        let mut equip: EquipMap = self
            .state
            .config
            .gameplay
            .inventory
            .equip_slots
            .iter()
            .map(|slot| (slot.clone(), None))
            .collect();
        let mut inventory = Vec::new();
        // M17/F30c: raw-сокеты надетой брони — из них считаются
        // бонусы самоцветов (правило контента, вручную).
        let mut armor_sockets: Vec<Value> = Vec::new();
        for item in db::items::inventory(pool, &row.id).await? {
            let Some(def) = db::items::def(pool, &item.def_id).await? else {
                warn!("нет определения {} для предмета {}", item.def_id, item.uid);
                continue;
            };
            if let Some(slot) = &item.equipped_slot {
                equip.insert(slot.clone(), Some(item.uid.clone()));
                if def.item_type == "armor" {
                    armor_sockets.push(item.sockets.clone());
                }
            }
            inventory.push(item_snapshot(&item, &def));
        }
        // M17/F30c: самоцветы в сокетах надетой брони улучшают
        // основные характеристики — бонус в снапшот-attrs (виден в
        // «Атрибутах»/«Параметрах», участвует в требованиях и гейтах).
        for sockets_json in &armor_sockets {
            let sockets: Vec<Value> =
                serde_json::from_value(sockets_json.clone()).unwrap_or_default();
            for socket in sockets {
                let Some(gem_id) = socket.get("def_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Ok(Some(gem)) = db::items::def(pool, gem_id).await else {
                    continue;
                };
                if gem.item_type != "gem" {
                    continue;
                }
                let (Some(attr), Some(value)) = (
                    gem.stats.get("attr").and_then(|v| v.as_str()),
                    gem.stats.get("value").and_then(|v| v.as_i64()),
                ) else {
                    continue;
                };
                *attrs.entry(attr.to_string()).or_insert(0) += value;
            }
        }

        // M15/F24: уровень и шкала опыта (окно персонажа).
        let (level, xp_in_level, xp_to_next) =
            crate::systems::level_for_total(row.total_xp, &self.state.config.gameplay.level);

        // M15/F26: производные характеристики (вкладка «Параметры») —
        // из атрибутов, навыков и именно снаряжённых предметов.
        let equip_uids: std::collections::HashSet<&str> =
            equip.values().flatten().map(String::as_str).collect();
        let equipped: Vec<ld2_protocol::types::ItemSnapshot> = inventory
            .iter()
            .filter(|i| equip_uids.contains(i.uid.as_str()))
            .cloned()
            .collect();
        let derived = crate::stats::derive(
            &attrs,
            &skills,
            &equipped,
            self.state.config.gameplay.combat.base_damage,
        );

        Ok(CharacterState {
            id: row.id.clone(),
            name: row.name.clone(),
            // M17/F30b: класс для правил экипировки на клиенте (F30e —
            // ресурс боя).
            profile: Some(row.profile.clone()),
            attrs,
            skills,
            // Изученные профессии (§6, M12.6).
            professions: match db::professions::learned(pool, &row.id).await {
                Ok(rows) => rows
                    .iter()
                    .map(|(id, xp, rank)| self.profession_snapshot(id, *xp, *rank))
                    .collect(),
                Err(e) => {
                    warn!("профессии {}: {e}", row.id);
                    Vec::new()
                }
            },
            hp: Hp {
                cur: i64::from(row.hp_cur),
                max: i64::from(row.hp_max),
            },
            // M17/F30e: ресурс по профилю — ярость/энергия/мана.
            mp: {
                let max = crate::systems::resource_max(
                    row.profile.as_str(),
                    &self.state.config.gameplay.resource,
                    i64::from(row.mp_max),
                );
                Mp {
                    cur: i64::from(row.mp_cur).clamp(0, max),
                    max,
                }
            },
            xp_pool: row.xp_pool,
            level: Some(level),
            total_xp: Some(row.total_xp),
            xp_in_level: Some(xp_in_level),
            xp_to_next: Some(xp_to_next),
            derived: Some(derived),
            equip,
            inventory,
            crime: CrimeState {
                // Активные статусы §10 (переживают рестарт, D27).
                statuses: match db::crime::active_statuses(pool, &row.id, crate::time::now_epoch())
                    .await
                {
                    Ok(rows) => rows
                        .into_iter()
                        .map(|(kind, expires_at)| self.crime_status(&kind, expires_at))
                        .collect(),
                    Err(e) => {
                        warn!("crime (статусы {}): {e}", row.id);
                        Vec::new()
                    }
                },
            },
            // Журнал квестов: контент + прогресс (§6, ФР-12).
            quests: {
                let defs = db::quests::defs(pool).await.unwrap_or_default();
                let progress = db::quests::progress(pool, &row.id)
                    .await
                    .unwrap_or_default();
                defs.iter()
                    .map(|def| {
                        let p = progress.iter().find(|(id, _, _)| id == &def.0);
                        db::quests::build_state(def, p)
                    })
                    .collect()
            },
            extra: Default::default(),
        })
    }

    /// Перечитать лист персонажа из БД в сессию (после мутаций, для ack
    /// с `char` §7).
    async fn reload_char(&mut self, seq: u32, char_id: &str) -> Result<CharacterState, Out> {
        let row = db::characters::get(&self.state.pool, char_id)
            .await
            .map_err(|e| {
                warn!("reload листа (БД): {e}");
                Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")
            })?
            .ok_or_else(|| Self::err(seq, ErrorCode::NameTaken, "персонаж не найден"))?;
        self.character_state(&row)
            .await
            .map_err(|e| {
                warn!("reload листа: {e}");
                Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")
            })
            .inspect(|state| {
                self.char = Some(state.clone());
            })
    }

    /// Текущая позиция из БД (источник истины): тик-смерть/респаун меняют
    /// её вне сессии — кэш синхронизируется (M12.5).
    async fn current_location(&mut self, char_id: &str) -> String {
        match crate::db::characters::get(&self.state.pool, char_id).await {
            Ok(Some(row)) => {
                if self.location_id.as_deref() != Some(row.location_id.as_str()) {
                    self.location_id = Some(row.location_id.clone());
                }
                row.location_id
            }
            Ok(None) => self.location_id.clone().unwrap_or_default(),
            Err(e) => {
                warn!("чтение позиции {char_id}: {e}");
                self.location_id.clone().unwrap_or_default()
            }
        }
    }

    /// `cmd/combat.attack` (§7): начать/сменить бой с мобом текущей локации
    /// (тик-луп мира наносит удары, M12.5).
    async fn combat_attack(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CombatAttack>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let location = self.current_location(&char_id).await;
        let now = crate::time::now_ms();
        let started = {
            let mut world = self.state.world.lock().expect("мир");
            world.start_combat(&char_id, &location, &cmd.target_id, now)
        };
        if !started {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/combat.stop` (§7): выйти из боя.
    async fn combat_stop(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        {
            let mut world = self.state.world.lock().expect("мир");
            world.end_combat(&char_id);
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/combat.flee` (§7): шанс побега из конфига `[gameplay.combat]`;
    /// провал → `E_RATE`-стиль `E_COOLDOWN` с `retry_after_s`.
    async fn combat_flee(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let in_combat = {
            let world = self.state.world.lock().expect("мир");
            world.in_combat(&char_id)
        };
        if !in_combat {
            return vec![Self::err(seq, ErrorCode::NotInLocation, "вы не в бою")];
        }
        let cfg = &self.state.config.gameplay.combat;
        // Детерминированный «случай» по времени (без rand-зависимости).
        let roll = (crate::time::now_ms() % 100) as f64 / 100.0;
        let fled = roll < cfg.flee_chance;
        {
            let mut world = self.state.world.lock().expect("мир");
            if fled {
                world.end_combat(&char_id);
            }
        }
        if fled {
            vec![Self::ack_payload(seq, &serde_json::json!({}))]
        } else {
            let retry = (cfg.attack_interval_s.ceil() as u64).max(1);
            let mut ack = Ack::error(seq, None, &ErrorCode::Cooldown, "не удалось сбежать");
            if let Some(err) = ack.error.as_mut() {
                err.retry_after_s = Some(retry);
            }
            vec![out_ack(ack)]
        }
    }

    /// `cmd/skill.use` (§7, skills.md §4, M14): активная способность.
    /// v1 — `power_strike`: ×`damage_mult` от `base_damage` по цели-мобу,
    /// серверный кулдаун (`skills.active.<id>.cooldown_s`) → `E_COOLDOWN`,
    /// гейт навыка → `E_REQUIREMENT_ATTR`, нет цели → `E_TARGET_GONE`.
    async fn skill_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SkillUse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(active) = self.state.config.skills.active.get(&cmd.skill_id).cloned() else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("неизвестная способность: {}", cmd.skill_id),
            )];
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        let now_s = crate::time::now_epoch();

        // Гейт по навыку (§4: открывается рангом).
        if let Some(req) = active.req_skill.as_deref() {
            let rank = db::skills::learned_one(pool, &char_id, req)
                .await
                .ok()
                .flatten()
                .map(|(_, r)| r)
                .unwrap_or(0);
            if rank < active.req_rank as i32 {
                return vec![Self::err(
                    seq,
                    ErrorCode::RequirementAttr,
                    format!(
                        "нужен навык «{req}» ранг {} (сейчас {rank})",
                        active.req_rank
                    ),
                )];
            }
        }

        // Цель: моб в локации персонажа (одна цель — §4).
        let Some(target) = cmd.target_id.as_deref() else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не указана")];
        };
        {
            let world = self.state.world.lock().expect("мир");
            let alive = world.mob_location(target).as_deref() == Some(location.as_str());
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        // Серверный кулдаун (§4/E_COOLDOWN + retry_after_s) — после гейта
        // и цели: запрос валидируется целиком, потом — rate-limit.
        {
            let world = self.state.world.lock().expect("мир");
            if let Some(left) = world.ability_cooldown_left(&char_id, &cmd.skill_id, now_s) {
                let mut ack = Ack::error(
                    seq,
                    None,
                    &ErrorCode::Cooldown,
                    "способность перезаряжается",
                );
                if let Some(err) = ack.error.as_mut() {
                    err.retry_after_s = Some(left.max(1) as u64);
                }
                return vec![out_ack(ack)];
            }
        }

        // M17/F30e: ресурс класса — стоимость активной способности
        // (ярость воина / энергия лучника / мана мага; 0 — бесплатно)
        // и профиль для прироста ярости за урон.
        let Some(crow) = (match db::characters::get(pool, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("skill.use (ресурс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        let profile = crow.profile.as_str();
        let res_cfg = &self.state.config.gameplay.resource;
        let res_max = crate::systems::resource_max(profile, res_cfg, i64::from(crow.mp_max));
        let mut res_cur = i64::from(crow.mp_cur).min(res_max);
        let mut resource_spent = false;
        if active.cost > 0 {
            let cost = i64::from(active.cost);
            if res_cur < cost {
                return vec![Self::err(
                    seq,
                    ErrorCode::NotEnoughMp,
                    format!(
                        "не хватает {}: {res_cur} < {cost}",
                        crate::systems::resource_name(profile)
                    ),
                )];
            }
            res_cur -= cost;
            if let Err(e) = db::characters::set_mp(pool, &char_id, res_cur as i32).await {
                warn!("skill.use (списание ресурса): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            resource_spent = true;
        }

        // Урон: base_damage × множитель способности (§4: ×1.8).
        let damage = ((self.state.config.gameplay.combat.base_damage as f64) * active.damage_mult)
            .round()
            .max(1.0) as i64;
        // M17/F30e: ярость воина за нанесённый урон (15 % — конфиг).
        let rage = crate::systems::rage_gain(res_cfg, profile, damage);

        // Кулдаун фиксируем ДО удара (защита от двойного нажатия в одном тике).
        let until = now_s + active.cooldown_s.ceil() as i64;
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_ability_cooldown(&char_id, &cmd.skill_id, until);
        }

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        mine.push(out_event(
            "evt/combat/cooldown",
            None,
            serde_json::to_value(CombatCooldown {
                skill_id: Some(cmd.skill_id.clone()),
                until_epoch: until,
            })
            .unwrap_or_default(),
        ));
        // M17/F30e: списанный ресурс — сразу клиенту (патч mp).
        if resource_spent {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "mp".into(),
                serde_json::json!({ "cur": res_cur, "max": res_max }),
            );
            mine.push(out_event(
                "evt/char/stats",
                None,
                serde_json::to_value(CharStats { patch }).unwrap_or_default(),
            ));
        }

        // Урон цели: hit/hp → при смерти: despawn/труп/бой окончен + XP.
        let mut others: Vec<(String, String)> = Vec::new();
        let mut xp_work: Option<crate::tick::KillXp> = None;
        let mut hit_done = false;
        {
            let mut world = self.state.world.lock().expect("мир");
            if let Some(hp) = world.damage_mob(target, damage) {
                hit_done = true;
                let mob_name = world
                    .mob(target)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                let mob_max = world.mob_max_hp(target);
                let hit = CombatHit {
                    source: char_id.clone(),
                    source_name: by_name.clone(),
                    target: target.to_string(),
                    target_name: mob_name,
                    kind: CombatKind::Skill,
                    amount: damage,
                    crit: false,
                    miss: false,
                    text: format!("«{}»: −{damage}", cmd.skill_id),
                    extra: Default::default(),
                };
                for addr in world.chars_in_location(&location) {
                    push_evt(&mut others, &addr, "evt/combat/hit", &hit);
                    push_evt(
                        &mut others,
                        &addr,
                        "evt/entity/hp",
                        &EntityHp {
                            entity_id: target.to_string(),
                            hp: Hp {
                                cur: hp,
                                max: mob_max,
                            },
                        },
                    );
                }
                if hp == 0 {
                    let skills_cfg = &self.state.config.skills;
                    xp_work = crate::tick::mob_kill_effects(
                        &mut world,
                        &mut others,
                        target,
                        &location,
                        &char_id,
                        skills_cfg,
                    );
                }
            }
        }
        // M17/F30e: прирост ярости за нанесённый урон (воин) — после
        // удара, с патчем клиенту.
        if rage > 0 && hit_done {
            let new_rage = (res_cur + rage).min(res_max);
            if new_rage > res_cur {
                match db::characters::set_mp(pool, &char_id, new_rage as i32).await {
                    Ok(()) => {
                        let mut patch = serde_json::Map::new();
                        patch.insert(
                            "mp".into(),
                            serde_json::json!({ "cur": new_rage, "max": res_max }),
                        );
                        mine.push(out_event(
                            "evt/char/stats",
                            None,
                            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
                        ));
                    }
                    Err(e) => warn!("skill.use (ярость {char_id}): {e}"),
                }
            }
        }
        for (addr, text) in others {
            if text.is_empty() {
                continue;
            }
            if addr == char_id {
                mine.push(Out::Text(text));
            } else {
                let world = self.state.world.lock().expect("мир");
                world.send_to_char(&addr, text);
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        mine
    }

    /// `cmd/char.xp_invest` (§7): вложение XP-пула в навык — транзакционно
    /// (D27), потолок/гейты — skills.md §2.2 (конфиг `[skills]`, D25).
    async fn char_xp_invest(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharXpInvest>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.points <= 0 {
            return vec![Self::err(seq, ErrorCode::XpPool, "points должен быть > 0")];
        }
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        match db::skills::content_skill(pool, &cmd.skill_id).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::XpPool, "нет такого навыка")];
            }
            Err(e) => {
                warn!("xp_invest (навык): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        // M14: вложение/повышение — только у тренера по навыку в локации.
        let Some(price) = (match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Skill,
            &cmd.skill_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("xp_invest (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этому навыку",
            )];
        };

        let pool_now = match db::skills::xp_pool(pool, &char_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!("xp_invest (пул): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if pool_now < cmd.points {
            return vec![Self::err(seq, ErrorCode::XpPool, "не хватает очков опыта")];
        }
        let (mut xp, mut rank) = match db::skills::learned(pool, &char_id).await {
            Ok(rows) => rows
                .into_iter()
                .find(|s| s.skill_id == cmd.skill_id)
                .map(|s| (s.xp, s.rank))
                .unwrap_or((0, 0)),
            Err(e) => {
                warn!("xp_invest (прогресс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Ранг на момент запроса — по нему считается цена (вложение не
        // дорожает от того, что само же повышает ранг).
        let rank_at_start = rank;
        let cfg = &self.state.config.skills;
        xp += cmd.points;
        loop {
            let need = crate::systems::xp_to_next(rank.max(0) as u32, cfg);
            if xp < need {
                break;
            }
            if rank >= cfg.rank_max as i32 {
                return vec![Self::err(
                    seq,
                    ErrorCode::RankCap,
                    "достигнут потолок ранга",
                )];
            }
            let next = rank + 1;
            // Гейт атрибутов (§2.2): конфиг даёт значение порога; сравниваем
            // с максимальным из пяти атрибутов персонажа.
            if let Some(gate) = cfg.gates.iter().find(|g| g.rank as i32 == next) {
                let max_attr = self
                    .char
                    .as_ref()
                    .map(|c| c.attrs.values().max().copied().unwrap_or(0))
                    .unwrap_or(0);
                if max_attr < gate.attr {
                    return vec![Self::err(
                        seq,
                        ErrorCode::RequirementAttr,
                        format!(
                            "ранг {next} требует атрибут {} (максимум {max_attr})",
                            gate.attr
                        ),
                    )];
                }
            }
            xp -= need;
            rank = next;
        }
        // M14: оплата тренеру — ПОСЛЕ всех проверок (тренер → пул → ранг →
        // гейт атрибутов): за неудачное вложение золото не списывается.
        let gold = trainers::gold_price(&price, i64::from(rank_at_start));
        // F29a: оплата монетами — события списания в конце ответа.
        let money_events = match self.pay_trainer(seq, &char_id, &price, gold).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };

        let pool_left = pool_now - cmd.points;
        if let Err(e) =
            db::skills::invest_tx(pool, &char_id, &cmd.skill_id, xp, rank, pool_left).await
        {
            warn!("xp_invest (транзакция): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let mut outs = vec![
            Self::ack_payload(seq, &serde_json::json!({ "pool_left": pool_left })),
            out_event(
                "evt/char/xp",
                None,
                serde_json::to_value(CharXp {
                    skill_id: cmd.skill_id,
                    amount: cmd.points,
                    skill_xp: xp,
                    rank: Some(rank.max(0) as u32),
                    xp_to_next: Some(crate::systems::xp_to_next(rank.max(0) as u32, cfg)),
                    pool_left: Some(pool_left),
                })
                .unwrap_or_default(),
            ),
        ];
        outs.extend(money_events);
        outs
    }

    /// `cmd/item.move` (M16/F29g): поменять местами два предмета
    /// рюкзака (перетаскивание) → ack `{}`; порядок — `items.sort_order`,
    /// клиент меняет сетку локально при отправке.
    async fn item_move(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemMove>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        match db::items::swap_order(&self.state.pool, &char_id, &cmd.item_uid, &cmd.target_uid)
            .await
        {
            Ok(true) => vec![Self::ack_payload(seq, &serde_json::json!({}))],
            Ok(false) => vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")],
            Err(e) => {
                warn!("item.move: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// `cmd/item.use` (§7): взятие статического лута (`loot:*`) или расход
    /// зелья (heal из stats §6 → HP + убыль стака).
    async fn item_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.item_uid.starts_with("loot:") {
            return self.take_loot(seq, &char_id, &cmd.item_uid).await;
        }
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.use (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.use (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // Зелье: heal из stats (открытый объект §6).
        if def.item_type == "potion" {
            if let Some(heal) = def.stats.get("heal").and_then(|v| v.as_i64()) {
                let row = match db::characters::get(pool, &char_id).await {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
                    }
                    Err(e) => {
                        warn!("item.use (персонаж): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let max = i64::from(row.hp_max);
                let new_cur = (i64::from(row.hp_cur).min(max) + heal).min(max);
                if let Err(e) = db::characters::set_hp(pool, &char_id, new_cur as i32).await {
                    warn!("item.use (hp): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                if item.qty > 1 {
                    let new_qty = i64::from(item.qty) - 1;
                    if let Err(e) = db::items::set_qty(pool, &item.uid, new_qty).await {
                        warn!("item.use (qty): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(new_qty));
                    outs.push(out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState {
                            uid: item.uid.clone(),
                            patch,
                        })
                        .unwrap_or_default(),
                    ));
                } else {
                    if let Err(e) = db::items::delete(pool, &item.uid).await {
                        warn!("item.use (удаление): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    outs.push(out_event(
                        "evt/item/removed",
                        None,
                        serde_json::to_value(ItemRemoved {
                            uid: item.uid.clone(),
                            reason: ItemRemovalReason::Consumed,
                        })
                        .unwrap_or_default(),
                    ));
                }
                let mut patch = serde_json::Map::new();
                patch.insert(
                    "hp".into(),
                    serde_json::json!({ "cur": new_cur, "max": max }),
                );
                outs.push(out_event(
                    "evt/char/stats",
                    None,
                    serde_json::to_value(CharStats { patch }).unwrap_or_default(),
                ));
                return outs;
            }
        }
        vec![Self::err(
            seq,
            ErrorCode::ItemGone,
            "предмет нельзя применить",
        )]
    }

    /// Взятие лута сущности: `loot:<сущность>:<idx>` — однократно
    /// (`loot_taken`, D27), лимит слотов — из конфига (D25).
    async fn take_loot(&mut self, seq: u32, char_id: &str, loot_uid: &str) -> Vec<Out> {
        let Some(rest) = loot_uid.strip_prefix("loot:") else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let Some((entity_id, idx)) = rest.rsplit_once(':') else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let Ok(idx) = idx.parse::<usize>() else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let pool = self.state.pool.clone();

        // Динамический труп (M12.5): слот в памяти; лимит — до изъятия.
        let dynamic = {
            let world = self.state.world.lock().expect(" мир");
            world
                .dynamic_corpse(entity_id)
                .and_then(|c| c.loot.get(idx).cloned())
        };
        if let Some(slot) = dynamic {
            let Some(def_id) = slot else {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")];
            };
            match db::items::count(&pool, char_id).await {
                Ok(n) if n >= i64::from(self.state.config.gameplay.inventory.slots) => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::InventoryFull,
                        "инвентарь заполнен",
                    )];
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("take_loot (слоты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            let Some(def) = (match db::items::def(&pool, &def_id).await {
                Ok(def) => def,
                Err(e) => {
                    warn!("take_loot (определение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }) else {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "нет определения предмета",
                )];
            };
            // Изъятие из памяти: None — кто-то успел раньше (гонок нет —
            // обработка последовательна, но проверка честная).
            let took = {
                let mut world = self.state.world.lock().expect("мир");
                world.take_dynamic_loot(entity_id, idx)
            };
            if took.is_none() {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")];
            }
            // F29a: монеты складываются в существующий стак (патч
            // qty), обычные предметы — новым инстансом.
            let (row, stacked) = if def.item_type == db::items::MONEY_TYPE {
                match db::items::grant_stacking(&pool, char_id, &def_id, i64::from(def.qty)).await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("take_loot (стак монет): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
            } else {
                match db::items::insert(&pool, char_id, &def_id, i64::from(def.qty)).await {
                    Ok(row) => (row, false),
                    Err(e) => {
                        warn!("take_loot (insert): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
            };
            let ev = if stacked {
                let mut patch = serde_json::Map::new();
                patch.insert("qty".into(), serde_json::json!(row.qty));
                out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: row.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                )
            } else {
                out_event(
                    "evt/item/added",
                    None,
                    serde_json::to_value(ItemAdded {
                        item: item_snapshot(&row, &def),
                    })
                    .unwrap_or_default(),
                )
            };
            return vec![Self::ack_payload(seq, &serde_json::json!({})), ev];
        }

        // Статический лут из сида (D26): однократно на персонажа.
        let current = self.current_location(char_id).await;
        let Some(loc_row) = (match db::locations::get(&pool, &current).await {
            Ok(row) => row,
            Err(e) => {
                warn!("take_loot (локация): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "персонаж не в мире")];
        };
        let entities: Vec<Entity> =
            serde_json::from_value(loc_row.entities.clone()).unwrap_or_default();
        let Some(entity) = entities.into_iter().find(|e| e.id == entity_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "сущность не найдена")];
        };
        let defs = entity_loot_defs(&entity);
        let Some(def_id) = defs.get(idx).cloned() else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого лута")];
        };
        match db::items::count(&pool, char_id).await {
            Ok(n) if n >= i64::from(self.state.config.gameplay.inventory.slots) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::InventoryFull,
                    "инвентарь заполнен",
                )];
            }
            Ok(_) => {}
            Err(e) => {
                warn!("take_loot (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let Some(def) = (match db::items::def(&pool, &def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("take_loot (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        match db::items::take_loot_tx(&pool, loot_uid, char_id, &def_id, i64::from(def.qty)).await {
            Ok(None) => vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")],
            Ok(Some(row)) => {
                let item = item_snapshot(&row, &def);
                let mut outs = vec![
                    Self::ack_payload(seq, &serde_json::json!({})),
                    out_event(
                        "evt/item/added",
                        None,
                        serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
                    ),
                ];
                // ФР-11: лут с трупа **игрока** → статус «Мародёр» (§10).
                if entity.was_player.unwrap_or(false) {
                    if let Some((_, text)) = self.apply_crime(char_id, "marauder").await {
                        outs.push(Out::Text(text));
                    }
                }
                outs
            }
            Err(e) => {
                warn!("take_loot (транзакция): {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// `cmd/item.equip` (§7): слот — из конфига (D25), требования — из
    /// определения (§6 `requires`); занятый слот снимает прежний предмет.
    async fn item_equip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemEquip>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if !self
            .state
            .config
            .gameplay
            .inventory
            .equip_slots
            .contains(&cmd.slot)
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "слот недоступен")];
        }
        let pool = self.state.pool.clone();
        let Some(item) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.equip (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        if item.equipped_slot.is_some() {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмет уже надет")];
        }
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.equip (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // Требования предмета (§6) → E_REQUIREMENT_ATTR.
        if let Some(requires) = def
            .requires
            .as_ref()
            .and_then(|v| serde_json::from_value::<Requires>(v.clone()).ok())
        {
            let attrs = self
                .char
                .as_ref()
                .map(|c| c.attrs.clone())
                .unwrap_or_default();
            for (attr, need) in &requires.attr {
                let have = attrs.get(attr).copied().unwrap_or(0);
                if have < *need {
                    return vec![Self::err(
                        seq,
                        ErrorCode::RequirementAttr,
                        format!("не хватает {}: {have} < {need}", attr_genitive(attr)),
                    )];
                }
            }
        }
        // M17/F30b: слот↔предмет (парные ring/artifact — по
        // псевдослоту); без слота предмет не экипируется.
        match def.slot.as_deref() {
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::ItemGone,
                    "предмет не экипируется",
                )];
            }
            Some(req) => {
                let ok = req == cmd.slot
                    || (req == "ring" && (cmd.slot == "ring1" || cmd.slot == "ring2"))
                    || (req == "artifact" && (cmd.slot == "artifact1" || cmd.slot == "artifact2"));
                if !ok {
                    return vec![Self::err(
                        seq,
                        ErrorCode::ItemGone,
                        format!("«{}» не надевается в этот слот", def.name),
                    )];
                }
            }
        }
        // M17/F30b: правила классов для оружия/щитов/посоха.
        if matches!(def.item_type.as_str(), "weapon" | "staff" | "shield") {
            let profile = self.char.as_ref().and_then(|c| c.profile.clone());
            let is_bow = def.skill.as_deref() == Some("bow");
            let gate = match profile.as_deref() {
                Some("mage") if def.item_type != "staff" => Some("маг носит только посох"),
                Some("archer") if !is_bow => Some("лучник носит только луки"),
                Some("warrior") if def.item_type == "staff" => Some("воин не носит посох"),
                Some("warrior") if is_bow => Some("воин не носит луки"),
                _ => None,
            };
            if let Some(msg) = gate {
                return vec![Self::err(seq, ErrorCode::ItemGone, msg)];
            }
        }
        // M17/F30b: левая рука закрыта, пока в правой двуручник.
        if cmd.slot == "off_hand" {
            if let Ok(Some(main)) =
                db::items::find_equipped_in_slot(&pool, &char_id, "main_hand").await
            {
                if let Ok(Some(main_def)) = db::items::def(&pool, &main.def_id).await {
                    if main_def.hands >= 2 {
                        return vec![Self::err(
                            seq,
                            ErrorCode::ItemGone,
                            "левая рука занята двуручным оружием",
                        )];
                    }
                }
            }
        }
        // Переэкипка: прежний предмет слота снимается (без события —
        // клиент получает полный `char` в ack, §7).
        match db::items::find_equipped_in_slot(&pool, &char_id, &cmd.slot).await {
            Ok(Some(prev)) => {
                if let Err(e) = db::items::set_equipped(&pool, &prev.uid, None).await {
                    warn!("item.equip (снятие прежнего): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!("item.equip (слот): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        if let Err(e) = db::items::set_equipped(&pool, &item.uid, Some(&cmd.slot)).await {
            warn!("item.equip (надевание): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M17/F30b: двуручник в правой руке блокирует левую — щит
        // автоматически снимается (как в WoW при смене на двуручное).
        if cmd.slot == "main_hand" && def.hands >= 2 {
            match db::items::find_equipped_in_slot(&pool, &char_id, "off_hand").await {
                Ok(Some(prev)) => {
                    if let Err(e) = db::items::set_equipped(&pool, &prev.uid, None).await {
                        warn!("item.equip (снятие левой руки): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("item.equip (левая рука): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }
        let char_state = match self.reload_char(seq, &char_id).await {
            Ok(state) => state,
            Err(out) => return vec![out],
        };
        let updated = match db::items::find(&pool, &item.uid, &char_id).await {
            Ok(Some(row)) => item_snapshot(&row, &def),
            _ => {
                return vec![Self::err(seq, ErrorCode::Internal, "предмет исчез")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "char": char_state, "item": updated }),
        )]
    }

    /// `cmd/item.unequip` (§7): снять предмет слота → ack `{char, item}`.
    async fn item_unequip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUnequip>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();
        let Some(item) = (match db::items::find_equipped_in_slot(&pool, &char_id, &cmd.slot).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.unequip (слот): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "в этом слоте пусто")];
        };
        if let Err(e) = db::items::set_equipped(&pool, &item.uid, None).await {
            warn!("item.unequip: {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let char_state = match self.reload_char(seq, &char_id).await {
            Ok(state) => state,
            Err(out) => return vec![out],
        };
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.unequip (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        let updated = item_snapshot(&item, &def);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "char": char_state, "item": updated }),
        )]
    }

    /// `cmd/item.drop` (§7): полный выброс → `evt/item/removed`
    /// (reason=dropped), частичный → `evt/item/state` с новым qty.
    async fn item_drop(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemDrop>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.drop (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let qty = cmd.qty.unwrap_or(i64::from(item.qty));
        if qty <= 0 {
            return vec![Self::err(
                seq,
                ErrorCode::ItemGone,
                "некорректное количество",
            )];
        }
        if qty >= i64::from(item.qty) {
            if let Err(e) = db::items::delete(pool, &item.uid).await {
                warn!("item.drop: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    "evt/item/removed",
                    None,
                    serde_json::to_value(ItemRemoved {
                        uid: item.uid.clone(),
                        reason: ItemRemovalReason::Dropped,
                    })
                    .unwrap_or_default(),
                ),
            ]
        } else {
            let new_qty = i64::from(item.qty) - qty;
            if let Err(e) = db::items::set_qty(pool, &item.uid, new_qty).await {
                warn!("item.drop (qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                ),
            ]
        }
    }

    /// `cmd/chat.send` (§7): dev-сценарии `/scenario …` (ФР-06/тесты);
    /// полноценный чат (ФР-13) — фаза M12.7.
    /// M14/F19: `cmd/chat.delete` — удаление сообщения из истории
    /// (только admin/moderator, §9 E_PERMISSION) + `evt/chat/deleted`
    /// всем онлайн (история общая — как system/global).
    async fn chat_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatDelete>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "удалять сообщения могут только админ и модератор",
            )];
        }
        match db::chat::delete(&self.state.pool, cmd.id as i64).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "сообщение не найдено",
                )];
            }
            Err(e) => {
                warn!("chat (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let payload = ld2_protocol::evt::ChatDeleted { id: cmd.id };
        if let Some(text) = event_payload_text("evt/chat/deleted", &payload) {
            let world = self.state.world.lock().expect("мир");
            for addr in world.online_chars() {
                world.send_to_char(&addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    async fn chat_send(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatSend>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if let Some(scenario) = cmd.text.strip_prefix("/scenario ") {
            return self.scenario(scenario.trim(), seq).await;
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Копия нужного из конфига (D25): cfg-заимствование мешает &mut ниже.
        let (channels, max_len, cooldown_s, history_limit) = {
            let chat = &self.state.config.gameplay.chat;
            (
                chat.channels.clone(),
                chat.max_len,
                chat.cooldown_s,
                chat.server_history,
            )
        };

        // Канал из конфига §5 (открытый список).
        if !channels.contains(&cmd.channel) {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("нет канала: {}", cmd.channel),
            )];
        }
        // M14/F19: системный чат — только admin/moderator (§9 E_PERMISSION).
        if cmd.channel == "system" && !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "недостаточно прав для system-канала",
            )];
        }
        // Длина текста (§12.2: chat.max_len → E_RATE_LIMITED).
        if cmd.text.chars().count() > max_len as usize {
            return vec![Self::err(
                seq,
                ErrorCode::RateLimited,
                format!("слишком длинное сообщение (до {max_len})"),
            )];
        }
        if cmd.text.trim().is_empty() {
            return vec![Self::err(seq, ErrorCode::Internal, "пустое сообщение")];
        }
        // Кулдаун (§12.2: chat.cooldown_s → E_RATE_LIMITED + retry_after_s).
        if let Some(last) = self.last_chat_at {
            let elapsed = last.elapsed().as_secs_f64();
            if elapsed < cooldown_s {
                let retry = (cooldown_s - elapsed).ceil().max(1.0) as u64;
                return vec![Self::ack_rate_limited(seq, retry)];
            }
        }

        // Рассылка: location — в локации, global/system — всем онлайн,
        // whisper — адресату (по имени) и автору, party/guild — участникам
        // (группы/гильдии — M12.7b/d: до тех пор только автор).
        let name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();
        let location = self.current_location(&char_id).await;
        // Каналы группы/гильдии (ФР-13): участникам своего состава.
        if cmd.channel == "party" {
            let members = {
                let social = self.state.social.lock().expect("соц");
                social.party_members(&char_id)
            };
            if members.is_empty() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")];
            }
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        if cmd.channel == "guild" {
            let guild = db::guilds::guild_of(&self.state.pool, &char_id)
                .await
                .ok()
                .flatten();
            let Some(guild) = guild else {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "вы не в гильдии",
                )];
            };
            let members = db::guilds::members(&self.state.pool, &guild.0)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>();
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        let recipients: Vec<String> = {
            let world = self.state.world.lock().expect("мир");
            match cmd.channel.as_str() {
                "location" => world.chars_in_location(&location),
                "global" | "system" => world.online_chars(),
                "whisper" => {
                    let target = cmd.to.as_deref().and_then(|to| world.char_id_by_name(to));
                    match target {
                        Some(id) if id != char_id => vec![char_id.clone(), id],
                        Some(_) => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::TargetGone,
                                "нельзя шептать себе",
                            )];
                        }
                        None => {
                            return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                        }
                    }
                }
                _ => vec![char_id.clone()],
            }
        };

        self.chat_deliver(seq, &cmd, &char_id, &name, &recipients, history_limit)
            .await
    }

    /// Общая доставка сообщения: история в БД (кольцо server_history,
    /// D25) → рассылка адресатам → ack (§2.2: ack раньше событий).
    async fn chat_deliver(
        &mut self,
        seq: u32,
        cmd: &ChatSend,
        char_id: &str,
        name: &str,
        recipients: &[String],
        history_limit: u32,
    ) -> Vec<Out> {
        let (msg_id, sent_at) = match db::chat::push(
            &self.state.pool,
            &cmd.channel,
            char_id,
            name,
            cmd.text.trim(),
            history_limit,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("chat (история): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        self.last_chat_at = Some(std::time::Instant::now());

        let payload = ld2_protocol::evt::ChatMessage {
            // M14/F19: id строки — для cmd/chat.delete (модерация).
            id: Some(msg_id.max(0) as u64),
            channel: cmd.channel.clone(),
            from: char_id.to_string(),
            name: name.to_string(),
            text: cmd.text.trim().to_string(),
            tags: None,
            sent_at,
            extra: Default::default(),
        };
        let text = event_payload_text("evt/chat/message", &payload);
        if let Some(text) = text {
            let world = self.state.world.lock().expect("мир");
            for addr in recipients {
                world.send_to_char(addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// Реплей истории чата при выборе персонажа (§5 `server_history`):
    /// события `evt/chat/message` после snapshot (кольцо переживает рестарт).
    async fn chat_history_events(&self) -> Vec<Out> {
        let limit = self.state.config.gameplay.chat.server_history;
        if limit == 0 {
            return Vec::new();
        }
        let rows = match db::chat::recent(&self.state.pool, limit).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("chat (реплей истории): {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(id, channel, from, name, text, sent_at)| {
                // M14/F22: реплей помечается — клиент не показывает
                // прошлую сессию новому входу (кольцо D25 в БД живёт,
                // совместимость — через extra §11).
                let mut extra = serde_json::Map::new();
                extra.insert("replay".to_string(), serde_json::Value::Bool(true));
                let payload = ld2_protocol::evt::ChatMessage {
                    // M14/F19: id переживает рестарт — реплей тоже удаляем.
                    id: Some(id.max(0) as u64),
                    channel,
                    from,
                    name,
                    text,
                    tags: None,
                    sent_at,
                    extra,
                };
                event_payload_text("evt/chat/message", &payload).map(Out::Text)
            })
            .collect()
    }

    /// `/scenario death` (ФР-06): общий поток смерти (death.rs) — теряется
    /// **только лут**, опыт/пул сохраняются (skills.md §2.2); респаун в спавне.
    async fn scenario(&mut self, scenario: &str, seq: u32) -> Vec<Out> {
        // `/scenario give <def_id>` — dev-выдача предмета (тесты M12.5+).
        if let Some(def_id) = scenario.strip_prefix("give ") {
            return self.scenario_give(def_id.trim(), seq).await;
        }
        // `/scenario gold <n>` — dev-начисление золота (тесты M12.5c).
        if let Some(amount) = scenario.strip_prefix("gold ") {
            return self.scenario_gold(amount.trim(), seq).await;
        }
        // `/scenario crime <kind>` — dev-нарушение (тесты M12.5d).
        if let Some(kind) = scenario.strip_prefix("crime ") {
            return self.scenario_crime(kind.trim(), seq).await;
        }
        // `/scenario kit` — тестовый набор рюкзака (M16/F29b):
        // предметы разных типов/редкостей, улучшения, экипировка,
        // монеты.
        if scenario == "kit" {
            return self.scenario_kit(seq).await;
        }
        if scenario != "death" {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("неизвестный сценарий: {scenario}"),
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let events = crate::death::kill_character(&self.state, &char_id).await;

        // Свежее состояние после респауна: ack со sv, сессия и мир синхронны.
        let mut outs = vec![out_ack(Ack::ok(seq, None))];
        if let Ok(Some(row)) = db::characters::get(&self.state.pool, &char_id).await {
            let state_version = row.state_version as u64;
            outs[0] = out_ack(Ack::ok(seq, Some(state_version)));
            let (location_id, _) = self.resolve_location(&row.location_id, &char_id).await;
            self.location_id = Some(location_id.clone());
            if let Ok(state) = self.character_state(&row).await {
                self.char = Some(state);
            }
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, &location_id);
            world.end_combat(&char_id);
        }
        for (addr, text) in events {
            if !text.is_empty() && addr == char_id {
                outs.push(Out::Text(text));
            }
        }
        outs
    }

    /// `/scenario give <def_id>` (dev): выдать предмет персонажу —
    /// сокеты посоха создаются сразу (ADR-002, иначе UI не увидит слоты).
    /// `/scenario kit` (M16/F29b): тестовый набор в рюкзак — предметы
    /// разных типов и редкостей с улучшениями +N, полная экипировка
    /// (слоты из конфига) и стак монет. Наполнение окна рюкзака для
    /// смоука; повторный вызов добавляет стаки (слоты переносятся).
    async fn scenario_kit(&mut self, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];

        // (def_id, qty, улучшение +N, слот): экипировка — уникальные
        // инстансы, расходники/материалы — в существующие стаки.
        // M17/F30b: экипировка по новым слотам (12 из 15 — ring2 и
        // артефакты остаются пустыми для смоука).
        let grants: [(&str, i64, i32, Option<&str>); 18] = [
            ("sword_knight", 1, 3, Some("main_hand")),
            ("shield_wood", 1, 1, Some("off_hand")),
            ("helmet_leather", 1, 2, Some("head")),
            ("shoulders_leather", 1, 1, Some("shoulders")),
            ("bracers_leather", 1, 1, Some("bracers")),
            ("gloves_cloth", 1, 2, Some("gloves")),
            ("armor_bronze", 1, 4, Some("body")),
            ("greaves_iron", 1, 3, Some("legs")),
            ("boots_traveler", 1, 5, Some("feet")),
            ("cloak_traveler", 1, 2, Some("cloak")),
            ("ring_silver", 1, 1, Some("ring1")),
            ("amulet_talisman", 1, 1, Some("amulet")),
            ("staff_apprentice", 1, 1, None),
            ("potion_heal", 3, 0, None),
            ("rune_fire", 2, 0, None),
            ("scroll_frost", 1, 0, None),
            ("herb", 5, 0, None),
            ("tool_quill", 1, 0, None),
        ];
        for (def_id, qty, upgrade, slot) in grants {
            let Some(def) = db::items::def(pool, def_id).await.ok().flatten() else {
                warn!("scenario kit: нет определения {def_id} — пропуск");
                continue;
            };
            let mut row = if let Some(slot) = slot {
                // Слот занят — прежний предмет снимается (паритет
                // cmd/item.equip).
                if let Ok(Some(prev)) = db::items::find_equipped_in_slot(pool, &char_id, slot).await
                {
                    let _ = db::items::set_equipped(pool, &prev.uid, None).await;
                }
                let Ok(mut row) = db::items::insert(pool, &char_id, def_id, qty).await else {
                    warn!("scenario kit: insert {def_id} — пропуск");
                    continue;
                };
                if upgrade > 0 {
                    row.upgrade = upgrade;
                    let _ = db::items::set_upgrade(pool, &row.uid, upgrade).await;
                }
                let _ = db::items::set_equipped(pool, &row.uid, Some(slot)).await;
                row
            } else {
                match db::items::grant_stacking(pool, &char_id, def_id, qty).await {
                    Ok((row, _)) => row,
                    Err(e) => {
                        warn!("scenario kit (стак {def_id}): {e}");
                        continue;
                    }
                }
            };
            // Посох в рюкзаке тоже должен иметь пустые сокеты (окно
            // инкрустации открывается по предмету, без экипировки).
            if def.sockets > 0 && row.sockets.as_array().is_none_or(|a| a.is_empty()) {
                row.sockets = Self::empty_sockets(def.sockets);
                let _ = db::items::set_sockets(pool, &row.uid, row.sockets.clone()).await;
            }
            let item = item_snapshot(&row, &def);
            outs.push(out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            ));
        }

        // Монеты — стак (F29a): патч qty, если стак уже есть.
        match db::items::grant_stacking_by_type(pool, &char_id, db::items::MONEY_TYPE, 500).await {
            Ok(Some((row, stacked))) => {
                let ev = if stacked {
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(row.qty));
                    out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState {
                            uid: row.uid,
                            patch,
                        })
                        .unwrap_or_default(),
                    )
                } else {
                    let def = db::items::def(pool, &row.def_id).await.ok().flatten();
                    match def {
                        Some(def) => out_event(
                            "evt/item/added",
                            None,
                            serde_json::to_value(ItemAdded {
                                item: item_snapshot(&row, &def),
                            })
                            .unwrap_or_default(),
                        ),
                        None => return outs,
                    }
                };
                outs.push(ev);
            }
            Ok(None) => warn!("scenario kit: валюта не найдена в контенте"),
            Err(e) => warn!("scenario kit (монеты): {e}"),
        }
        outs
    }

    async fn scenario_give(&mut self, def_id: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(def) = (match db::items::def(pool, def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("scenario give (определение {def_id}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::ItemGone,
                "нет такого определения",
            )];
        };
        if db::items::count(pool, &char_id).await.unwrap_or(i64::MAX)
            >= i64::from(self.state.config.gameplay.inventory.slots)
        {
            return vec![Self::err(
                seq,
                ErrorCode::InventoryFull,
                "инвентарь заполнен",
            )];
        }
        let mut row = match db::items::insert(pool, &char_id, def_id, i64::from(def.qty)).await {
            Ok(row) => row,
            Err(e) => {
                warn!("scenario give (insert): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if def.sockets > 0 {
            row.sockets = Self::empty_sockets(def.sockets);
            if let Err(e) = db::items::set_sockets(pool, &row.uid, row.sockets.clone()).await {
                warn!("scenario give (сокеты): {e}");
            }
        }
        let item = item_snapshot(&row, &def);
        vec![
            Self::ack_payload(seq, &serde_json::json!({})),
            out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            ),
        ]
    }

    /// `cmd/socket.insert` (§7, ADR-002): руна → свободный сокет посоха.
    /// ack `{staff}`; руна расходуется (`evt/item/removed`/qty), сокеты —
    /// патчем `evt/item/state`.
    async fn socket_insert(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketInsert>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let Some(staff) = (match db::items::find(pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.insert (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.insert (определение посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // M17/F30a: сокеты принимает любой предмет с `sockets > 0`
        // (посохи и мечи равноправны); каст руны — только посох.
        if staff_def.sockets == 0 {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "у предмета нет сокетов",
            )];
        }
        let Some(rune) = (match db::items::find(pool, &cmd.rune_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.insert (поиск руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "руны нет")];
        };
        let Some(rune_def) = (match db::items::def(pool, &rune.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.insert (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        // M17/F30c: в сокет идут руны и самоцветы (броня).
        if !matches!(rune_def.item_type.as_str(), "rune" | "gem") {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "в сокет идут руны и самоцветы",
            )];
        }

        // Ленивая инициализация: пустой массив → слоты по определению.
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if sockets.is_empty() {
            sockets = Self::empty_sockets_list(staff_def.sockets);
        }
        let Some(slot) = sockets.get_mut(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        if slot.get("rune").is_some_and(|r| !r.is_null()) {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет уже занят")];
        }
        let charges = rune_def
            .stats
            .get("charges")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        let max = rune_def
            .stats
            .get("max")
            .and_then(|v| v.as_i64())
            .unwrap_or(charges);
        let spell = rune_def
            .stats
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // M17/F30a: damage руны пишется в сокет — прибавляется к урону
        // оружия (stats::derive) и попадает в снапшот.
        let damage = rune_def.stats.get("damage").and_then(|v| v.as_i64());
        *slot = serde_json::json!({
            "index": cmd.socket_index,
            "def_id": rune_def.id,
            "rune": {
                "name": rune_def.name,
                "spell": spell,
                "charges": charges,
                "max": max,
                "damage": damage,
            }
        });
        let sockets_value = Value::Array(sockets);
        if let Err(e) = db::items::set_sockets(pool, &staff.uid, sockets_value).await {
            warn!("socket.insert (запись): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }

        // Руна расходуется (стак −1 либо удаление).
        let mut outs = Vec::new();
        if rune.qty > 1 {
            let new_qty = i64::from(rune.qty) - 1;
            if let Err(e) = db::items::set_qty(pool, &rune.uid, new_qty).await {
                warn!("socket.insert (qty руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            outs.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: rune.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            if let Err(e) = db::items::delete(pool, &rune.uid).await {
                warn!("socket.insert (удаление руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            outs.push(out_event(
                "evt/item/removed",
                None,
                serde_json::to_value(ItemRemoved {
                    uid: rune.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ));
        }

        // ack `{staff}` (§7): посох с новыми сокетами.
        let staff_row = match db::items::find(pool, &staff.uid, &char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => staff,
            Err(e) => {
                warn!("socket.insert (перечитывание посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let mut result = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "staff": item_snapshot(&staff_row, &staff_def) }),
        )];
        result.extend(outs);
        result
    }

    /// `cmd/socket.extract` (§7): извлечь руну → в инвентарь (заряды
    /// сбрасываются к максимуму — отклонение M12.5b: per-item charges нет).
    /// ack `{staff, rune}`.
    async fn socket_extract(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketExtract>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let Some(staff) = (match db::items::find(pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.extract (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.extract (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if sockets.is_empty() {
            sockets = Self::empty_sockets_list(staff_def.sockets);
        }
        let Some(slot) = sockets.get_mut(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        let has_rune = slot.get("rune").is_some_and(|r| !r.is_null());
        let rune_def_id = slot
            .get("def_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        if !has_rune {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        }
        let Some(rune_def_id) = rune_def_id else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        };
        if db::items::count(pool, &char_id).await.unwrap_or(i64::MAX)
            >= i64::from(self.state.config.gameplay.inventory.slots)
        {
            return vec![Self::err(
                seq,
                ErrorCode::InventoryFull,
                "инвентарь заполнен",
            )];
        }
        *slot = serde_json::json!({ "index": cmd.socket_index, "rune": null });
        if let Err(e) = db::items::set_sockets(pool, &staff.uid, Value::Array(sockets)).await {
            warn!("socket.extract (запись): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let Some(rune_def) = (match db::items::def(pool, &rune_def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.extract (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        let rune_row = match db::items::insert(pool, &char_id, &rune_def_id, 1).await {
            Ok(row) => row,
            Err(e) => {
                warn!("socket.extract (возврат руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let staff_row = match db::items::find(pool, &staff.uid, &char_id).await {
            Ok(Some(row)) => row,
            _ => staff,
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "staff": item_snapshot(&staff_row, &staff_def),
                "rune": item_snapshot(&rune_row, &rune_def),
            }),
        )]
    }

    /// `cmd/magic.rune_cast` (§7, D4): мана + заряд руны → урон цели
    /// (`stats.damage`), `evt/magic/cast` всем в локации, XP убийства — в
    /// школу (`stats.school`). Ошибки: `E_NO_RUNE_SLOT`, `E_NO_CHARGES`,
    /// `E_NOT_ENOUGH_MP`, `E_TARGET_GONE`.
    async fn magic_rune_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicRuneCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(staff) = (match db::items::find(&pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("rune_cast (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(&pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения посоха",
            )];
        };
        let sockets: Vec<Value> = serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        let Some(slot) = sockets.get(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        let Some(rune_info) = slot.get("rune").filter(|r| !r.is_null()) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        };
        let charges = rune_info
            .get("charges")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if charges <= 0 {
            return vec![Self::err(seq, ErrorCode::NoCharges, "руна разряжена")];
        }
        let rune_def_id = slot
            .get("def_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(rune_def) = (match db::items::def(&pool, &rune_def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        if staff_def.item_type != "staff" || staff_def.sockets == 0 {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "у предмета нет сокетов",
            )];
        }

        // Мана (D4: каст тратит ману И заряд).
        let cost = i64::from(self.state.config.gameplay.magic.mana.rune_cost);
        let Some(row) = (match db::characters::get(&pool, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("rune_cast (персонаж): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        if i64::from(row.mp_cur) < cost {
            return vec![Self::err(seq, ErrorCode::NotEnoughMp, "не хватает маны")];
        }
        let spell = rune_info
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = rune_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = rune_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Цель (опциональна): моб должен быть в локации персонажа.
        let location = self.current_location(&char_id).await;
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        // Расход: мана и заряд руны (D27 — сразу в БД).
        let new_mp = (i64::from(row.mp_cur) - cost).max(0) as i32;
        if let Err(e) = db::characters::set_mp(&pool, &char_id, new_mp).await {
            warn!("rune_cast (mp): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if let Some(slot) = sockets.get_mut(cmd.socket_index as usize) {
            if let Some(r) = slot.get_mut("rune") {
                r["charges"] = serde_json::json!(charges - 1);
            }
        }
        if let Err(e) = db::items::set_sockets(&pool, &staff.uid, Value::Array(sockets)).await {
            warn!("rune_cast (заряды): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();

        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Rune,
            target: cmd.target_id.clone(),
            charges_left: Some((charges - 1) as i32),
            mp_left: Some(new_mp),
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text("evt/magic/cast", &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }

        // cast-событие — всем в локации (свой — уже в `mine`).
        let mut others: Vec<(String, String)> = Vec::new();
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    others.push((addr, text.clone()));
                }
            }
        }
        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Rune,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: format!("Заклинание «{spell}»: {mob_name} −{damage}"),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        plan.push((
                            addr.clone(),
                            event_payload_text("evt/combat/hit", &hit).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                "evt/entity/hp",
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            // XP убийства — в школу заклинания (skills.md §2.3).
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
                if text.is_empty() {
                    continue;
                }
                if addr == char_id {
                    mine.push(Out::Text(text));
                } else {
                    others.push((addr, text));
                }
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        {
            let world = self.state.world.lock().expect("мир");
            for (addr, text) in others {
                world.send_to_char(&addr, text);
            }
        }
        mine
    }

    /// `cmd/magic.scroll_cast` (§7): свиток расходуется, мана не тратится;
    /// `scroll_requires_target` → цель обязательна (`E_TARGET_GONE`).
    async fn magic_scroll_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicScrollCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(scroll) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("scroll_cast (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "свитка нет")];
        };
        let Some(scroll_def) = (match db::items::def(&pool, &scroll.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("scroll_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения свитка",
            )];
        };
        if scroll_def.item_type != "scroll" {
            return vec![Self::err(seq, ErrorCode::ItemGone, "это не свиток")];
        }

        let location = self.current_location(&char_id).await;
        let requires_target = self.state.config.gameplay.magic.scroll_requires_target;
        if requires_target && cmd.target_id.is_none() {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не указана")];
        }
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        let spell = scroll_def
            .stats
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = scroll_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = scroll_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Расход свитка — до событий (§7: consumable).
        let mut consumable = Vec::new();
        if scroll.qty > 1 {
            let new_qty = i64::from(scroll.qty) - 1;
            if let Err(e) = db::items::set_qty(&pool, &scroll.uid, new_qty).await {
                warn!("scroll_cast (qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            consumable.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: scroll.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            if let Err(e) = db::items::delete(&pool, &scroll.uid).await {
                warn!("scroll_cast (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            consumable.push(out_event(
                "evt/item/removed",
                None,
                serde_json::to_value(ItemRemoved {
                    uid: scroll.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ));
        }

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();
        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Scroll,
            target: cmd.target_id.clone(),
            charges_left: None,
            mp_left: None,
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text("evt/magic/cast", &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }
        mine.extend(consumable);

        // cast-событие — остальным в локации (свой уже в `mine`).
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }

        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Scroll,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: format!("Заклинание «{spell}»: {mob_name} −{damage}"),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        plan.push((
                            addr.clone(),
                            event_payload_text("evt/combat/hit", &hit).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                "evt/entity/hp",
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
                if text.is_empty() {
                    continue;
                }
                if addr == char_id {
                    mine.push(Out::Text(text));
                } else {
                    let world = self.state.world.lock().expect("мир");
                    world.send_to_char(&addr, text);
                }
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        mine
    }

    /// Пустые слоты посоха (форма хранения JSONB, ADR-002).
    fn empty_sockets(count: i32) -> Value {
        Value::Array(Self::empty_sockets_list(count))
    }

    fn empty_sockets_list(count: i32) -> Vec<Value> {
        (0..count.max(0))
            .map(|i| serde_json::json!({ "index": i, "rune": null }))
            .collect()
    }

    /// `/scenario gold <n>` (dev): начислить золото (тесты M12.5c).
    async fn scenario_gold(&mut self, amount: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Ok(n) = amount.parse::<i64>() else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректная сумма")];
        };
        // F29a: золото — стак монет (первое начисление создаёт стак).
        match db::items::grant_stacking_by_type(
            &self.state.pool,
            &char_id,
            db::items::MONEY_TYPE,
            n,
        )
        .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "валюта не найдена в контенте",
                )];
            }
            Err(e) => {
                warn!("scenario gold: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `/scenario crime <kind>` (dev): нарушение через общий поток §10
    /// (blend + статус + журнал), как при реальном триггере.
    async fn scenario_crime(&mut self, kind: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some((_, text)) = self.apply_crime(&char_id, kind).await {
            outs.push(Out::Text(text));
        }
        outs
    }

    /// `cmd/upgrade.preview` (§7, ФР-10): превью «сейчас → после» —
    /// `preview_id`, `delta.upgrade`, `cost.gold`, `chance` (если
    /// `shows_chance`). `E_UPGRADE_MAX` — уровень уже предельный.
    async fn upgrade_preview(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradePreview>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("upgrade.preview (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("upgrade.preview (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };

        let cfg = &self.state.config.gameplay.upgrade;
        let level = def.upgrade + item.upgrade;
        if level >= cfg.max_level as i32 {
            return vec![Self::err(seq, ErrorCode::UpgradeMax, "предельный уровень")];
        }
        let next = level + 1;
        let cost = crate::systems::upgrade_cost(i64::from(level), cfg);
        let chance = crate::systems::upgrade_chance(i64::from(level), cfg);
        let preview_id = format!("pv_{:x}", crate::time::now_ms());
        self.preview_state = Some(UpgradePreviewState {
            preview_id: preview_id.clone(),
            item_uid: item.uid.clone(),
            level,
        });

        let mut payload = serde_json::json!({
            "preview_id": preview_id,
            "delta": { "upgrade": format!("+{next}") },
            "cost": { "gold": cost },
        });
        if cfg.shows_chance {
            payload["chance"] = serde_json::json!(chance);
        }
        vec![Self::ack_payload(seq, &payload)]
    }

    /// `cmd/upgrade.start` (§7): золото списывается всегда, бросок — по
    /// шансу из конфига; провал — по `features."upgrade.rules"`
    /// (keep/level_down/break_on_fail). Итог — `evt/upgrade/result`
    /// (+ `evt/item/state`/`evt/item/removed`). `E_UPGRADE_PREVIEW_STALE` —
    /// превью не то/устарело.
    async fn upgrade_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradeStart>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        // Превью: одно на соединение, uid + уровень должны совпасть.
        let preview = self.preview_state.clone();
        self.preview_state = None; // одноразово (§7: stale после старта)
        let Some(preview) = preview else {
            return vec![Self::err(seq, ErrorCode::UpgradePreviewStale, "нет превью")];
        };
        if preview.preview_id != cmd.preview_id || preview.item_uid != cmd.item_uid {
            return vec![Self::err(
                seq,
                ErrorCode::UpgradePreviewStale,
                "превью устарело",
            )];
        }

        let Some(item) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("upgrade.start (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("upgrade.start (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        let cfg = self.state.config.gameplay.upgrade.clone();
        let level = def.upgrade + item.upgrade;
        if level >= cfg.max_level as i32 {
            return vec![Self::err(seq, ErrorCode::UpgradeMax, "предельный уровень")];
        }
        if level != preview.level {
            // Уровень изменился после превью — бросать по свежим данным нельзя.
            return vec![Self::err(
                seq,
                ErrorCode::UpgradePreviewStale,
                "предмет изменился",
            )];
        }

        // Золото списывается всегда (до броска); F29a: монеты-стаки.
        let cost = crate::systems::upgrade_cost(i64::from(level), &cfg);
        let money_events = match db::items::consume_money(&pool, &char_id, cost).await {
            Ok(Some(changes)) => Self::money_events(changes),
            Ok(None) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает золота: {cost}"),
                )];
            }
            Err(e) => {
                warn!("upgrade.start (монеты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };

        let chance = crate::systems::upgrade_chance(i64::from(level), &cfg);
        let success = crate::systems::upgrade_roll(crate::time::now_ms()) < chance;
        let rule = crate::systems::UpgradeFailRule::parse(
            self.state
                .config
                .gameplay
                .features
                .get("upgrade.rules")
                .and_then(|v| match v {
                    crate::config::FeatureValue::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("keep"),
        );
        let cost_json = serde_json::json!({ "gold": cost });

        let mut outs = Vec::new();
        // F29a: оплата монетами — события списания до результата.
        outs.extend(money_events);
        if success {
            let new_level = level + 1;
            if let Err(e) = db::items::set_upgrade(&pool, &item.uid, item.upgrade + 1).await {
                warn!("upgrade.start (запись): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            outs.push(out_event(
                "evt/upgrade/result",
                None,
                serde_json::to_value(UpgradeResult {
                    uid: item.uid.clone(),
                    ok: true,
                    new_upgrade: Some(new_level as u32),
                    item_destroyed: Some(false),
                    cost: serde_json::from_value(cost_json.clone()).unwrap_or_default(),
                })
                .unwrap_or_default(),
            ));
            let mut patch = serde_json::Map::new();
            patch.insert("upgrade".into(), serde_json::json!(new_level));
            outs.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: item.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            let (new_upgrade, destroyed) = match rule {
                crate::systems::UpgradeFailRule::Keep => (level, false),
                crate::systems::UpgradeFailRule::LevelDown => ((level - 1).max(0), false),
                crate::systems::UpgradeFailRule::Break => (level, true),
            };
            if destroyed {
                if let Err(e) = db::items::delete(&pool, &item.uid).await {
                    warn!("upgrade.start (разрушение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                outs.push(out_event(
                    "evt/item/removed",
                    None,
                    serde_json::to_value(ItemRemoved {
                        uid: item.uid.clone(),
                        reason: ItemRemovalReason::Consumed,
                    })
                    .unwrap_or_default(),
                ));
            } else if new_upgrade != level {
                if let Err(e) =
                    db::items::set_upgrade(&pool, &item.uid, new_upgrade - def.upgrade).await
                {
                    warn!("upgrade.start (понижение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                let mut patch = serde_json::Map::new();
                patch.insert("upgrade".into(), serde_json::json!(new_upgrade));
                outs.push(out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                ));
            }
            let result = UpgradeResult {
                uid: item.uid.clone(),
                ok: false,
                new_upgrade: if destroyed {
                    None
                } else {
                    Some(new_upgrade as u32)
                },
                item_destroyed: Some(destroyed),
                cost: serde_json::from_value(cost_json).unwrap_or_default(),
            };
            outs.insert(
                0,
                out_event(
                    "evt/upgrade/result",
                    None,
                    serde_json::to_value(result).unwrap_or_default(),
                ),
            );
        }

        // ack → события (§2.2).
        let mut result = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        result.extend(outs);
        result
    }

    /// F29a: события списания стаков монет — патч `qty` уцелевшего
    /// стака либо `evt/item/removed` опустевшего (Consumed).
    fn money_events(changes: Vec<(String, Option<i64>)>) -> Vec<Out> {
        changes
            .into_iter()
            .map(|(uid, new_qty)| match new_qty {
                Some(qty) => {
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(qty));
                    out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState { uid, patch }).unwrap_or_default(),
                    )
                }
                None => out_event(
                    "evt/item/removed",
                    None,
                    serde_json::to_value(ItemRemoved {
                        uid,
                        reason: ItemRemovalReason::Consumed,
                    })
                    .unwrap_or_default(),
                ),
            })
            .collect()
    }

    /// Оплата тренеру (M14): сначала **проверки** (золото и предметы),
    /// потом списания — иначе отказ «нет предмета» съедал бы уже
    /// уплаченное золото. Нехватка → `E_INGREDIENTS` (гейт тренера и
    /// остальные проверки выполнены до вызова).
    async fn pay_trainer(
        &self,
        seq: u32,
        char_id: &str,
        price: &trainers::TrainerPrice,
        gold: i64,
    ) -> Result<Vec<Out>, Out> {
        let pool = &self.state.pool;

        // 1) Проверки.
        if gold > 0 {
            // F29a: баланс — сумма стаков монет (type=money).
            let have = match db::items::count_by_type(pool, char_id, db::items::MONEY_TYPE).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("тренер (монеты): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            };
            if have < gold {
                return Err(Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает золота: {gold}"),
                ));
            }
        }
        for (item_type, need) in &price.items {
            let have = match db::items::count_by_type(pool, char_id, item_type).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("тренер (предмет {item_type}): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            };
            if have < *need {
                return Err(Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("нужен предмет: {item_type} ×{need} (есть {have})"),
                ));
            }
        }

        // 2) Списания (все проверки прошли); F29a: монеты — стаки,
        // изменения уходят событиями в outs вызывающего.
        let mut events = Vec::new();
        if gold > 0 {
            match db::items::consume_money(pool, char_id, gold).await {
                Ok(Some(changes)) => events.extend(Self::money_events(changes)),
                Ok(None) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("не хватает золота: {gold}"),
                    ));
                }
                Err(e) => {
                    warn!("тренер (монеты): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        for (item_type, need) in &price.items {
            match db::items::consume_by_type(pool, char_id, item_type, *need).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err(Self::err(
                        seq,
                        ErrorCode::Ingredients,
                        format!("нужен предмет: {item_type} ×{need}"),
                    ));
                }
                Err(e) => {
                    warn!("тренер (списание {item_type}): {e}");
                    return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
                }
            }
        }
        Ok(events)
    }

    /// `cmd/crime.log` (§7): журнал нарушений (новые первые), limit —
    /// из payload (по умолчанию 50, максимум 500).
    async fn crime_log(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CrimeLog>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let limit = cmd.limit.unwrap_or(50).clamp(1, 500) as i64;
        let entries = match db::crime::log(&self.state.pool, &char_id, limit).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(kind, at, delta_s, total_s)| CrimeLogEntry {
                    kind,
                    at,
                    delta_s,
                    total_s,
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("crime.log: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "entries": entries }),
        )]
    }

    /// Статус из строки БД: severity — из `crime.kinds` конфига (D25).
    fn crime_status(&self, kind: &str, expires_at: i64) -> CrimeStatus {
        let severity = self
            .state
            .config
            .gameplay
            .crime
            .kinds
            .iter()
            .find(|k| k.kind == kind)
            .map(|k| crate::auth::severity(&k.severity))
            .unwrap_or(Severity::Unknown);
        CrimeStatus {
            kind: kind.to_string(),
            severity,
            expires_at,
        }
    }

    /// Новое нарушение (§10): blend-эскалация срока → статус + журнал в БД
    /// (D27) → `evt/crime/added` адресату. `None` — неизвестный kind.
    async fn apply_crime(&self, char_id: &str, kind: &str) -> Option<(String, String)> {
        let cfg = &self.state.config.gameplay.crime;
        // §5: ключи base — с суффиксом срока (`marauder_s`); kind — без.
        let base = cfg
            .base
            .get(kind)
            .or_else(|| cfg.base.get(&format!("{kind}_s")))
            .copied()? as i64;
        let now = crate::time::now_epoch();
        let t_cur = db::crime::total_term(&self.state.pool, char_id, now)
            .await
            .unwrap_or_else(|e| {
                warn!("crime (общий срок {char_id}): {e}");
                0
            });
        let (total, delta) =
            crate::systems::crime_blend(t_cur, base, cfg.t_max_s as i64, cfg.blend_k);
        if delta <= 0 {
            return None; // потолок §10 — вклада нет, статус не растёт
        }
        let (expires_at, _) =
            match db::crime::add_status(&self.state.pool, char_id, kind, now, delta, total).await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("crime (добавление {kind} {char_id}): {e}");
                    return None;
                }
            };
        let payload = CrimeAdded {
            status: self.crime_status(kind, expires_at),
            log_entry: CrimeLogEntry {
                kind: kind.to_string(),
                at: now,
                delta_s: delta,
                total_s: total,
            },
        };
        let text = crate::session::event_text(
            "evt/crime/added",
            None,
            serde_json::to_value(&payload).unwrap_or_default(),
        );
        if text.is_empty() {
            return None;
        }
        Some((char_id.to_string(), text))
    }

    // ── сделка (ФР-15, M12.7c) ──────────────────────────────────────────────

    /// Payload `evt/trade/state` для конкретного участника (зеркальная
    /// перспектива §8: `you` — своя сторона, `them` — чужая).
    async fn trade_state_payload(
        &self,
        char_id: &str,
        trade: &crate::social::Trade,
    ) -> serde_json::Value {
        let pool = &self.state.pool;
        let (mine, theirs) = if trade.a == char_id {
            (&trade.side_a, &trade.side_b)
        } else {
            (&trade.side_b, &trade.side_a)
        };
        let snapshots = |side: &crate::social::TradeSide| {
            let pool = pool.clone();
            let uids: Vec<String> = side.items.iter().map(|(u, _)| u.clone()).collect();
            async move {
                let mut out = Vec::new();
                for uid in uids {
                    if let Ok(Some(item)) = db::items::find_any(&pool, &uid).await {
                        if let Ok(Some(def)) = db::items::def(&pool, &item.def_id).await {
                            out.push(item_snapshot(&item, &def));
                        }
                    }
                }
                out
            }
        };
        let you = snapshots(mine).await;
        let them = snapshots(theirs).await;
        serde_json::json!({
            "offer_id": trade.id,
            "version": trade.version,
            "you": you,
            "them": them,
            "you_gold": mine.gold,
            "them_gold": theirs.gold,
            "you_locked": mine.locked,
            "them_locked": theirs.locked,
        })
    }

    /// Рассылка `evt/trade/state` обоим участникам (зеркала разные).
    async fn trade_broadcast_state(&self, trade: &crate::social::Trade) {
        let (a, b) = (trade.a.clone(), trade.b.clone());
        let payload_a = self.trade_state_payload(&a, trade).await;
        let payload_b = self.trade_state_payload(&b, trade).await;
        let text_a = event_payload_text("evt/trade/state", &payload_a);
        let text_b = event_payload_text("evt/trade/state", &payload_b);
        let world = self.state.world.lock().expect("мир");
        if let Some(t) = text_a {
            world.send_to_char(&a, t);
        }
        if let Some(t) = text_b {
            world.send_to_char(&b, t);
        }
    }

    /// Закрыть сделку событием `evt/trade/closed` (§8).
    async fn trade_close_event(
        &self,
        offer_id: &str,
        participants: &[String],
        reason: &str,
        result: Option<serde_json::Value>,
    ) {
        let mut payload = serde_json::json!({ "offer_id": offer_id, "reason": reason });
        if let Some(result) = result {
            payload["result"] = result;
        }
        let Some(text) = event_payload_text("evt/trade/closed", &payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in participants {
            world.send_to_char(addr, text.clone());
        }
    }

    /// `cmd/trade.open` (§7): открыть сделку — оба онлайн и в одной
    /// локации (ФР-15) → ack `{offer_id}` + `evt/trade/state` обоим.
    async fn trade_open(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeOpen>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "нельзя торговать с собой",
            )];
        }
        let location = self.current_location(&char_id).await;
        {
            let world = self.state.world.lock().expect("мир");
            let partner_loc = world.char_location(&cmd.player_id);
            if partner_loc.as_deref() != Some(location.as_str()) {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в вашей локации",
                )];
            }
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в сети",
                )];
            }
        }
        let opened = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .open_trade(&char_id, &cmd.player_id)
                .map(|id| social.trade(&id).cloned().expect("создана"))
        };
        match opened {
            Ok(trade) => {
                let offer_id = trade.id.clone();
                self.trade_broadcast_state(&trade).await;
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "offer_id": offer_id }),
                )]
            }
            Err(e) => vec![Self::err(seq, ErrorCode::TradeStale, e.message())],
        }
    }

    /// `cmd/trade.add_item` (§7): предмет в предложение (проверка
    /// владельца) → version+1, блокировки сняты → state обоим.
    async fn trade_add_item(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeAddItem>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        // Владелец и остаток стака — до изменения состава.
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("trade.add (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let qty = cmd
            .qty
            .unwrap_or(i64::from(item.qty))
            .clamp(1, i64::from(item.qty));

        let added = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_add_item(&char_id, &cmd.offer_id, &cmd.item_uid, qty)
                .cloned()
        };
        let trade = match added {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    crate::social::TradeError::Gone => ErrorCode::TradePartnerGone,
                    _ => ErrorCode::ItemGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.set_gold` (§7): золото стороны (баланс — из БД).
    async fn trade_set_gold(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeSetGold>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let balance =
            match db::items::count_by_type(&self.state.pool, &char_id, db::items::MONEY_TYPE).await
            {
                Ok(gold) => gold,
                Err(e) => {
                    warn!("trade.gold (баланс): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        if cmd.gold > balance {
            return vec![Self::err(
                seq,
                ErrorCode::TradeStale,
                format!("золота меньше: {balance}"),
            )];
        }
        let set = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_set_gold(&char_id, &cmd.offer_id, cmd.gold)
                .cloned()
        };
        let trade = match set {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    _ => ErrorCode::TradePartnerGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.propose` / `cmd/trade.accept` (§7): блокировка стороны
    /// со сверкой `version` (устаревшая → `E_TRADE_STALE`); обе стороны
    /// заблокированы → исполнение в одной транзакции (D27).
    async fn trade_lock_cmd(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradePropose>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let both_locked = {
            let mut social = self.state.social.lock().expect("соц");
            match social.trade_lock(&char_id, &cmd.offer_id, cmd.version) {
                Ok(locked) => locked,
                Err(e) => {
                    drop(social);
                    let code = match e {
                        crate::social::TradeError::Stale => ErrorCode::TradeStale,
                        _ => ErrorCode::TradePartnerGone,
                    };
                    return vec![Self::err(seq, code, e.message())];
                }
            }
        };
        let trade = {
            let social = self.state.social.lock().expect("соц");
            social.trade(&cmd.offer_id).cloned()
        };
        let Some(trade) = trade else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        if !both_locked {
            self.trade_broadcast_state(&trade).await;
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }
        // Оба заблокированы → исполнение одной транзакцией (предметы+золото).
        match self.trade_execute(&trade).await {
            Ok(result) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "done",
                    Some(result),
                )
                .await;
                vec![Self::ack_payload(seq, &serde_json::json!({}))]
            }
            Err(e) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "failed",
                    Some(serde_json::json!({ "error": e })),
                )
                .await;
                vec![Self::err(seq, ErrorCode::TradeStale, e)]
            }
        }
    }

    /// Исполнение сделки: перемещение предметов и золота сторон **одной**
    /// транзакцией (D27); при любом конфликте — откат и `failed`.
    async fn trade_execute(
        &self,
        trade: &crate::social::Trade,
    ) -> Result<serde_json::Value, String> {
        let pool = &self.state.pool;
        let mut tx = pool.begin().await.map_err(|e| format!("tx: {e}"))?;
        let sides = [
            (&trade.a, &trade.b, &trade.side_a),
            (&trade.b, &trade.a, &trade.side_b),
        ];
        let mut moved = Vec::new();
        for (from, to, side) in sides {
            for (uid, qty) in &side.items {
                // Предмет всё ещё у стороны? Частичный перенос стака.
                let cur: Option<(i32, String)> =
                    sqlx::query_as("SELECT qty, character_id FROM items WHERE uid = $1 FOR UPDATE")
                        .bind(uid)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(|e| format!("читаю {uid}: {e}"))?;
                let Some((cur_qty, owner)) = cur else {
                    return Err(format!("предмет {uid} исчез"));
                };
                if owner != *from {
                    return Err(format!("предмет {uid} не у владельца"));
                }
                if i64::from(cur_qty) < *qty {
                    return Err(format!("предмет {uid}: не хватает стака"));
                }
                if i64::from(cur_qty) == *qty {
                    sqlx::query("UPDATE items SET character_id = $2 WHERE uid = $1")
                        .bind(uid)
                        .bind(to)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("перенос {uid}: {e}"))?;
                } else {
                    sqlx::query("UPDATE items SET qty = qty - $2 WHERE uid = $1")
                        .bind(uid)
                        .bind(qty)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("уменьшение {uid}: {e}"))?;
                    let def_id: String =
                        sqlx::query_scalar("SELECT def_id FROM items WHERE uid = $1")
                            .bind(uid)
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(|e| format!("деф {uid}: {e}"))?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(qty)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("вставка {uid}: {e}"))?;
                }
                moved.push(serde_json::json!({ "uid": uid, "qty": qty }));
            }
            // F29a: деньги — стаки монет: списание у `from` (стаки
            // убывают, пустые удаляются) и начисление `to` (в старый
            // стак либо новым) — той же транзакцией, что и предметы.
            if side.gold > 0 {
                let rows: Vec<(String, i32)> = sqlx::query_as(
                    "SELECT i.uid, i.qty FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid FOR UPDATE",
                )
                .bind(from)
                .bind(db::items::MONEY_TYPE)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| format!("монеты {from}: {e}"))?;
                let mut left = side.gold;
                for (uid, qty) in rows {
                    if left <= 0 {
                        break;
                    }
                    let take = i64::from(qty).min(left);
                    left -= take;
                    let rest = qty - take as i32;
                    if rest <= 0 {
                        sqlx::query("DELETE FROM items WHERE uid = $1")
                            .bind(&uid)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("списание {uid}: {e}"))?;
                    } else {
                        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
                            .bind(&uid)
                            .bind(rest)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("стак {uid}: {e}"))?;
                    }
                }
                if left > 0 {
                    return Err(format!("недостаточно монет у {from}"));
                }
                let target: Option<String> = sqlx::query_scalar(
                    "SELECT i.uid FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid LIMIT 1 FOR UPDATE",
                )
                .bind(to)
                .bind(db::items::MONEY_TYPE)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("стак {to}: {e}"))?;
                if let Some(uid) = target {
                    sqlx::query("UPDATE items SET qty = qty + $2 WHERE uid = $1")
                        .bind(&uid)
                        .bind(side.gold)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("зачисление {uid}: {e}"))?;
                } else {
                    let def_id: String = sqlx::query_scalar(
                        "SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1",
                    )
                    .bind(db::items::MONEY_TYPE)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| format!("определение валюты: {e}"))?
                    .ok_or_else(|| "нет определения валюты в контенте".to_string())?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(side.gold)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("зачисление {to}: {e}"))?;
                }
            }
        }
        tx.commit().await.map_err(|e| format!("commit: {e}"))?;
        Ok(serde_json::json!({ "moved": moved }))
    }

    /// `cmd/trade.cancel` (§7): отмена любой стороной.
    async fn trade_cancel(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeCancel>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let closed = {
            let mut social = self.state.social.lock().expect("соц");
            let is_member = social
                .trade(&cmd.offer_id)
                .is_some_and(|t| t.a == char_id || t.b == char_id);
            if is_member {
                social.close_trade(&cmd.offer_id)
            } else {
                None
            }
        };
        let Some((participants, trade)) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        self.trade_close_event(&trade.id, &participants, "cancelled", None)
            .await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    // ── гильдия (ФР-16, M12.7d) ─────────────────────────────────────────────

    /// `GuildState` из БД: участники + online по реестру мира.
    async fn guild_state(&self, guild_id: &str) -> Option<ld2_protocol::types::GuildState> {
        let row = db::guilds::find(&self.state.pool, guild_id).await.ok()??;
        let members = db::guilds::members(&self.state.pool, guild_id)
            .await
            .unwrap_or_default();
        let world = self.state.world.lock().expect("мир");
        let members = members
            .into_iter()
            .map(|(id, name, rank)| ld2_protocol::types::GuildMember {
                online: world.char_name(&id).is_some(),
                id,
                name,
                rank,
                extra: Default::default(),
            })
            .collect();
        Some(ld2_protocol::types::GuildState {
            id: row.0,
            name: row.1,
            members,
            extra: Default::default(),
        })
    }

    /// Рассылка `evt/guild/updated` адресатам (свой — в outs после ack).
    fn guild_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let Some(text) = event_payload_text("evt/guild/updated", payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Гильдия персонажа для снапшота (§3 `guild: null` — не состоит).
    async fn guild_state_of(&self, char_id: &str) -> Option<ld2_protocol::types::GuildState> {
        let guild = db::guilds::guild_of(&self.state.pool, char_id)
            .await
            .ok()
            .flatten()?;
        self.guild_state(&guild.0).await
    }

    /// `cmd/guild.browse` (§7): поиск гильдий → `{guilds[]}`.
    async fn guild_browse(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildBrowse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let guilds = match db::guilds::browse(&self.state.pool, cmd.query.as_deref()).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("guild.browse: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guilds": guilds }),
        )]
    }

    /// `cmd/guild.create` (§7): создание (правило сервера: имя уникально,
    /// автор — лидер) → ack `{guild}` + журнал.
    async fn guild_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildCreate>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let name = cmd.name.trim();
        if name.is_empty() || name.chars().count() > 50 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "имя гильдии: 1..=50 символов",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        let guild_id = match db::guilds::create(&self.state.pool, name, &char_id).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "имя занято")];
            }
            Err(e) => {
                warn!("guild.create: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.join` (§7): вступление (открытое или по приглашению —
    /// `guild_id` из инвайта) → ack `{guild}`.
    async fn guild_join(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildJoin>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        // Приглашение (если есть) подтверждает вступление и закрывается.
        let invited = {
            let mut social = self.state.social.lock().expect("соц");
            let mut found = None;
            if let Some((invite_id, _)) = social
                .guild_invites_of(&char_id)
                .into_iter()
                .find(|(_, gid)| cmd.guild_id.as_deref().is_none_or(|g| g == gid))
            {
                if let Some(invite) = social.take_guild_invite(&char_id, &invite_id) {
                    found = Some(invite.guild_id);
                }
            }
            found
        };
        let guild_id = match cmd.guild_id.clone().or(invited) {
            Some(id) => id,
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нет приглашения в гильдию",
                )];
            }
        };
        let Some(guild) = (match db::guilds::find(&self.state.pool, &guild_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.join (гильдия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::GuildPermission, "гильдии нет")];
        };
        match db::guilds::join(&self.state.pool, &guild_id, &char_id, "member").await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "уже состоите")];
            }
            Err(e) => {
                warn!("guild.join: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        // Событие — всем участникам (включая нового).
        if let Some(state) = &state {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let _ = guild;
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.leave` (§7): выход (лидер передаёт ранг первому).
    async fn guild_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Перед выходом лидера — ранг первому оставшемуся (атомарно в БД).
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        if my_rank.as_deref() == Some("leader") {
            let _ = self.promote_first_member(&char_id).await;
        }
        let closed = match db::guilds::leave(&self.state.pool, &char_id).await {
            Ok(id) => id,
            Err(e) => {
                warn!("guild.leave: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(guild_id) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        // Оставшимся — обновлённый состав; ушедшему — guild: null.
        if let Some(state) = self.guild_state(&guild_id).await {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let null = serde_json::json!({ "guild": serde_json::Value::Null });
        self.guild_broadcast(std::slice::from_ref(&char_id), &null, &mut outs, "");
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// Передать лидерство первому участнику при выходе лидера.
    async fn promote_first_member(&self, leaving: &str) -> Result<(), sqlx::Error> {
        let Some(guild) = db::guilds::guild_of(&self.state.pool, leaving).await? else {
            return Ok(());
        };
        let members = db::guilds::members(&self.state.pool, &guild.0).await?;
        let Some(first) = members
            .into_iter()
            .map(|(id, _, _)| id)
            .find(|id| id != leaving)
        else {
            return Ok(());
        };
        db::guilds::set_rank(&self.state.pool, &guild.0, &first, "leader").await?;
        Ok(())
    }

    /// `cmd/guild.invite` (§7): приглашение → `evt/guild/updated` адресату.
    async fn guild_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        let Some(my_rank) = my_rank else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав приглашать",
            )];
        }
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.invite: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        {
            let world = self.state.world.lock().expect("мир");
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
            }
        }
        if db::guilds::rank_of(&self.state.pool, &cmd.player_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "игрок уже в гильдии")];
        }
        let from_name = self.current_name();
        let invite_id = {
            let mut social = self.state.social.lock().expect("соц");
            social.guild_invite(&cmd.player_id, &char_id, &from_name, &guild.0, &guild.1)
        };
        let payload = {
            let social = self.state.social.lock().expect("соц");
            let invite = social.guild_invite_payload(&invite_id);
            let mut payload = serde_json::json!({ "guild": serde_json::Value::Null });
            if let Some(invite) = invite {
                payload["invite"] = invite;
            }
            payload
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        self.guild_broadcast(
            std::slice::from_ref(&cmd.player_id),
            &payload,
            &mut outs,
            "",
        );
        outs
    }

    /// `cmd/guild.kick` (§7): исключение (leader/officer).
    async fn guild_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(my_rank) = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
        else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав исключать",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя исключить себя (выйдите через guild.leave)",
            )];
        }
        // Нельзя исключить лидера; офицер не исключает офицера.
        let target_rank = db::guilds::rank_of(&self.state.pool, &cmd.player_id)
            .await
            .ok()
            .flatten();
        match target_rank.as_deref() {
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Some("leader") => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нельзя исключить лидера",
                )];
            }
            Some("officer") if my_rank == "officer" => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "офицер не может исключить офицера",
                )];
            }
            _ => {}
        }
        match db::guilds::kick(&self.state.pool, &cmd.player_id).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.kick: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let guild_id = db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .map(|g| g.0);
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(guild_id) = &guild_id {
            if let Some(state) = self.guild_state(guild_id).await {
                let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
                let payload = serde_json::json!({ "guild": state });
                self.guild_broadcast(&members, &payload, &mut outs, &char_id);
            }
            let null = serde_json::json!({ "guild": serde_json::Value::Null });
            self.guild_broadcast(std::slice::from_ref(&cmd.player_id), &null, &mut outs, "");
            self.guild_log_event(guild_id, &mut outs).await;
        }
        outs
    }

    /// `cmd/guild.rank_set` (§7): смена ранга (атомарно; только leader).
    async fn guild_rank_set(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildRankSet>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !matches!(cmd.rank.as_str(), "leader" | "officer" | "member") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "неизвестный ранг",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some("leader")
        {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "только лидер меняет ранги",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя менять ранг себе",
            )];
        }
        match db::guilds::set_rank(&self.state.pool, &guild.0, &cmd.player_id, &cmd.rank).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(state) = self.guild_state(&guild.0).await {
            let mut members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            if !members.contains(&cmd.player_id) {
                members.push(cmd.player_id.clone());
            }
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        self.guild_log_event(&guild.0, &mut outs).await;
        outs
    }

    /// Реплей журнала гильдии → события `evt/guild/log` (ФР-16: системный
    /// лог при входе; кольцо — последние 50).
    async fn guild_log_event(&self, guild_id: &str, outs: &mut Vec<Out>) {
        let entries = match db::guilds::log_entries(&self.state.pool, guild_id).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("guild (журнал {guild_id}): {e}");
                return;
            }
        };
        for (kind, text, at) in entries {
            let payload = serde_json::json!({
                "entry": { "kind": kind, "text": text, "at": at }
            });
            if let Some(t) = event_payload_text("evt/guild/log", &payload) {
                outs.push(Out::Text(t));
            }
        }
    }

    // ── группа (ФР-14, M12.7b) ──────────────────────────────────────────────

    /// Текущее имя персонажа (для участников группы §8).
    fn current_name(&self) -> String {
        self.char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let char_id = self.selected_char_id.clone()?;
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default()
    }

    /// Рассылка `evt/party/updated` адресатам (свой конверт — в ack-ветке
    /// нельзя: события строго после своего ack, §2.2 — поэтому событие
    /// автору идёт через `outs`, остальным — в каналы соединений).
    fn party_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let text = event_payload_text("evt/party/updated", payload);
        let Some(text) = text else { return };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Payload `evt/party/updated` `{party}` (+ опц. `invite`, §11).
    fn party_payload(
        party: Option<ld2_protocol::types::PartyState>,
        invite: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({ "party": party });
        if let Some(invite) = invite {
            payload["invite"] = invite;
        }
        payload
    }

    /// `cmd/party.create` (§7): создать группу (лидер — автор) → `{party}`.
    async fn party_create(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.create_party(&char_id, &name) {
            Some(party) => {
                drop(social);
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )]
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::Internal, "вы уже в группе")]
            }
        }
    }

    /// `cmd/party.invite` (§7): приглашение → `evt/party/updated` c
    /// `invite` адресату (§11).
    async fn party_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Адресат по id: в мире (онлайн) — иначе отказ.
        let target_name = {
            let world = self.state.world.lock().expect("мир");
            match world.char_name(&cmd.player_id) {
                Some(name) => name,
                None => {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                }
            }
        };
        let _ = target_name;
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.invite(&char_id, &name, &cmd.player_id) {
            Ok(invite_id) => {
                let invite = social.invite_payload(&invite_id);
                let payload = Self::party_payload(None, invite);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &payload,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::PartyFull, e.message())]
            }
        }
    }

    /// `cmd/party.accept` (§7): принять приглашение → `{party}` +
    /// `evt/party/updated` всем участникам.
    async fn party_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, true).await
    }

    /// `cmd/party.decline` (§7): отклонить → `{}`.
    async fn party_decline(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, false).await
    }

    /// Общий обработчик accept/decline.
    async fn party_answer(&mut self, env: &Envelope, seq: u32, accept: bool) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyAccept>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let max = self.state.config.gameplay.party_max_members;
        let mut social = self.state.social.lock().expect("соц");
        if !accept {
            if social.decline_invite(&char_id, &cmd.invite_id) {
                return vec![Self::ack_payload(seq, &serde_json::json!({}))];
            }
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "приглашение не найдено",
            )];
        }
        match social.accept_invite(&char_id, &name, &cmd.invite_id, max) {
            Ok((party, members)) => {
                let payload = Self::party_payload(Some(party.clone()), None);
                drop(social);
                let mut outs = vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )];
                self.party_broadcast(&members, &payload, &mut outs, &char_id);
                outs
            }
            Err(e) => {
                drop(social);
                let code = match e {
                    crate::social::InviteError::Full => ErrorCode::PartyFull,
                    _ => ErrorCode::TargetGone,
                };
                vec![Self::err(seq, code, e.message())]
            }
        }
    }

    /// `cmd/party.leave` (§7): выйти → `{}` + `evt/party/updated` бывшим
    /// участникам (`party: null` — если группа распалась).
    async fn party_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.leave_party(&char_id) {
            Some((party, members)) => {
                // Оставшимся — обновлённый состав; ушедшему — party: null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &char_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                let left = Self::party_payload(None, None);
                self.party_broadcast(&remaining, &payload, &mut outs, "");
                self.party_broadcast(std::slice::from_ref(&char_id), &left, &mut outs, "");
                outs
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")]
            }
        }
    }

    /// `cmd/party.kick` (§7): исключить участника (только лидер).
    async fn party_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.kick_member(&char_id, &cmd.player_id) {
            Ok((party, members)) => {
                // Оставшимся — состав без исключённого; исключённому — null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &cmd.player_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(&remaining, &payload, &mut outs, &char_id);
                let removed = Self::party_payload(None, None);
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &removed,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::GuildPermission, e.message())]
            }
        }
    }

    // ── профессии и крафт (ФР-08, M12.6) ───────────────────────────────────

    /// Профессия персонажа → снапшот §6 (`xp_to_next` из конфига, D25).
    fn profession_snapshot(&self, id: &str, xp: i64, rank: i32) -> ProfessionSnapshot {
        let cfg = &self.state.config.gameplay.professions;
        let rank_u32 = rank.max(1) as u32;
        let xp_to_next = if rank_u32 >= cfg.rank_max {
            0
        } else {
            ((cfg.xp_to_next_base as f64) * cfg.xp_growth.powi(rank)).round() as i64
        };
        ProfessionSnapshot {
            id: id.to_string(),
            rank: rank_u32,
            xp,
            xp_to_next,
            extra: Default::default(),
        }
    }

    /// `cmd/craft.list` (§7): мои профессии `{professions[]}`.
    async fn craft_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let rows = match db::professions::learned(&self.state.pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let professions: Vec<ProfessionSnapshot> = rows
            .iter()
            .map(|(id, xp, rank)| self.profession_snapshot(id, *xp, *rank))
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "professions": professions }),
        )]
    }

    /// `cmd/craft.recipes` (§7): список рецептов `{recipes[]}` (payload
    /// skills.md §5.4; фильтр по профессии опционален).
    async fn craft_recipes(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftRecipes>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let rows = match db::craft::recipes(&self.state.pool, cmd.profession.as_deref()).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.recipes: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let recipes: Vec<serde_json::Value> = rows.iter().map(recipe_payload).collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "recipes": recipes }),
        )]
    }

    /// `cmd/craft.learn` (§7): изучение профессии; лимит первичных —
    /// `profession_limit` (D25) → `E_PROFESSION_LIMIT`.
    async fn craft_learn(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftLearn>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;

        let prof_type = match sqlx::query_scalar::<_, String>(
            "SELECT prof_type FROM professions WHERE id = $1",
        )
        .bind(&cmd.profession_id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(t)) => t,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::Profession, "нет такой профессии")];
            }
            Err(e) => {
                warn!("craft.learn (профессия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Уже изучена → идемпотентный ack без повторной оплаты (§7, M14).
        // Профессии живут в character_professions, а не в character_skills.
        match db::professions::learned(pool, &char_id).await {
            Ok(rows) => {
                if let Some((_, xp, rank)) =
                    rows.into_iter().find(|(id, _, _)| id == &cmd.profession_id)
                {
                    let snapshot = self.profession_snapshot(&cmd.profession_id, xp, rank);
                    return vec![Self::ack_payload(
                        seq,
                        &serde_json::json!({ "profession": snapshot }),
                    )];
                }
            }
            Err(e) => {
                warn!("craft.learn (изученные): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // M14: обучение только у тренера в локации (E_NO_TRAINER).
        let Some(price) = (match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Profession,
            &cmd.profession_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("craft.learn (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этой профессии",
            )];
        };

        if prof_type == "primary" {
            let limit = i64::from(self.state.config.gameplay.profession_limit);
            match db::professions::primary_count(pool, &char_id).await {
                Ok(count) if count >= limit => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::ProfessionLimit,
                        format!("лимит первичных профессий: {limit}"),
                    )];
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("craft.learn (счётчик): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // M14: оплата тренеру (новая профессия — ранг 0 → базовая цена).
        let gold = trainers::gold_price(&price, 0);
        // F29a: оплата монетами — события списания в конце ответа.
        let money_events = match self.pay_trainer(seq, &char_id, &price, gold).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };

        match db::professions::learn(pool, &char_id, &cmd.profession_id).await {
            Ok(_) => {}
            Err(e) => {
                warn!("craft.learn: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let snapshot = self.profession_snapshot(&cmd.profession_id, 0, 1);
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "profession": snapshot }),
        )];
        outs.push(out_event(
            "evt/craft/profession",
            None,
            serde_json::to_value(CraftProfession {
                profession: snapshot,
            })
            .unwrap_or_default(),
        ));
        outs.extend(money_events);
        outs
    }

    /// `cmd/craft.start` (§7): проверки (ранг/станок/инструмент/ингредиенты)
    /// → списание ингредиентов + джоб в БД (D27); итог — `evt/craft/result`
    /// по готовности (тик-луп). Ошибки: `E_RANK_CAP`, `E_STATION_REQUIRED`,
    /// `E_TOOL_MISSING`, `E_INGREDIENTS`.
    async fn craft_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftStart>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.qty < 1 || cmd.qty > 100 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "qty должно быть 1..=100",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(recipe) = (match db::craft::recipe(&pool, &cmd.recipe_id).await {
            Ok(r) => r,
            Err(e) => {
                warn!("craft.start (рецепт): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "рецепта нет")];
        };

        // Ранг профессии.
        let learned = match db::professions::learned(&pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.start (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let prof_rank = learned
            .iter()
            .find(|(id, _, _)| *id == recipe.profession)
            .map(|(_, _, rank)| *rank)
            .unwrap_or(0);
        if prof_rank < recipe.rank {
            return vec![Self::err(
                seq,
                ErrorCode::RankCap,
                format!("нужен ранг {} профессии {}", recipe.rank, recipe.profession),
            )];
        }

        // Станок: сущность с `extra.station` в текущей локации.
        if let Some(station) = recipe.station.as_deref() {
            let location = self.current_location(&char_id).await;
            let has_station = match db::locations::get(&pool, &location).await {
                Ok(Some(row)) => {
                    let entities: Vec<Entity> =
                        serde_json::from_value(row.entities.clone()).unwrap_or_default();
                    entities
                        .iter()
                        .any(|e| e.extra.get("station").and_then(|s| s.as_str()) == Some(station))
                }
                _ => false,
            };
            if !has_station {
                return vec![Self::err(
                    seq,
                    ErrorCode::StationRequired,
                    format!("нужен станок: {station}"),
                )];
            }
        }

        // Инструмент: предмет с `item_type` в инвентаре.
        if let Some(tool) = recipe.tool_type.as_deref() {
            let has_tool = match db::items::inventory_has_type(&pool, &char_id, tool).await {
                Ok(has) => has,
                Err(e) => {
                    warn!("craft.start (инструмент): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if !has_tool {
                return vec![Self::err(
                    seq,
                    ErrorCode::ToolMissing,
                    format!("нет инструмента: {tool}"),
                )];
            }
        }

        // Ингредиенты: qty × inputs.
        let inputs: Vec<ContentInput> =
            serde_json::from_value(recipe.inputs.clone()).unwrap_or_default();
        for input in &inputs {
            let need = input.qty * cmd.qty;
            let have = match db::items::count_by_type(&pool, &char_id, &input.item_type).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("craft.start (ингредиенты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if have < need {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает {}: {have}/{need}", input.item_type),
                )];
            }
        }
        for input in &inputs {
            let need = input.qty * cmd.qty;
            if let Err(e) =
                db::items::consume_by_type(&pool, &char_id, &input.item_type, need).await
            {
                warn!("craft.start (списание): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // Джоб: материалы уже списаны, рестарт не теряет их (D27).
        let finishes_at =
            crate::time::now_epoch() as f64 + f64::from(recipe.time_s) * cmd.qty as f64;
        if let Err(e) = db::craft::enqueue(&pool, &char_id, &recipe, cmd.qty, finishes_at).await {
            warn!("craft.start (джоб): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    // ── квесты (ФР-12, M12.6) ───────────────────────────────────────────────

    /// Журнал квестов: все контент-квесты + прогресс персонажа (§7).
    async fn quest_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let (defs, progress) = match (
            db::quests::defs(&self.state.pool).await,
            db::quests::progress(&self.state.pool, &char_id).await,
        ) {
            (Ok(d), Ok(p)) => (d, p),
            (Err(e), _) | (_, Err(e)) => {
                warn!("quest.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let quests: Vec<serde_json::Value> = defs
            .iter()
            .map(|def| {
                let row = progress.iter().find(|(id, _, _)| id == &def.0);
                serde_json::to_value(db::quests::build_state(def, row)).unwrap_or_default()
            })
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "quests": quests }),
        )]
    }

    /// `cmd/quest.accept` (§7): взять квест → ack `{quest}` +
    /// `evt/quest/updated`.
    async fn quest_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "accept").await
    }

    /// `cmd/quest.turn_in` (§7): сдать выполненный → награда (XP в пул +
    /// предметы) → ack `{quest}` + события.
    async fn quest_turn_in(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "turn_in").await
    }

    /// `cmd/quest.abandon` (§7): бросить → снова «доступен».
    async fn quest_abandon(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "abandon").await
    }

    /// Общий переход квеста (accept/turn_in/abandon) — одна точка проверок.
    async fn quest_transition(&mut self, env: &Envelope, seq: u32, op: &str) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(value) = Self::payload_of::<Value>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(quest_id) = value.get("quest_id").and_then(|q| q.as_str()) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет quest_id")];
        };
        let quest_id = quest_id.to_string();
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let defs = match db::quests::defs(pool).await {
            Ok(defs) => defs,
            Err(e) => {
                warn!("quest (квесты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(def) = defs.iter().find(|d| d.0 == quest_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "квеста нет")];
        };
        let progress = match db::quests::progress(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("quest (прогресс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);

        let mut outs = Vec::new();
        match op {
            "accept" => {
                if row.is_some() {
                    return vec![Self::err(seq, ErrorCode::Internal, "квест уже взят")];
                }
                let goal_count = def.3.as_array().map(|a| a.len()).unwrap_or(0);
                if let Err(e) = db::quests::accept(pool, &char_id, &quest_id, goal_count).await {
                    warn!("quest (accept): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            "turn_in" => {
                let Some((_, state, _)) = row else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "квест не взят")];
                };
                if state != "turn_in" {
                    return vec![Self::err(seq, ErrorCode::Internal, "цели ещё не выполнены")];
                }
                // Награда: XP в пул (D3/D4) + предметы (слоты — из конфига).
                let reward_xp = def.4.get("xp").and_then(|x| x.as_i64()).unwrap_or(0);
                let reward_items: Vec<(String, i64)> = def
                    .4
                    .get("items")
                    .and_then(|i| i.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| {
                                Some((
                                    it.get("type")?.as_str()?.to_string(),
                                    it.get("qty").and_then(|q| q.as_i64()).unwrap_or(1),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if reward_xp > 0 {
                    if let Err(e) = db::characters::add_xp_pool(pool, &char_id, reward_xp).await {
                        warn!("quest (награда XP): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
                for (item_type, qty) in &reward_items {
                    if let Err(e) = db::items::insert_by_type(pool, &char_id, item_type, *qty).await
                    {
                        warn!("quest (награда предмет): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
                if let Err(e) = db::quests::set_state(pool, &char_id, &quest_id, "done").await {
                    warn!("quest (done): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            _ => {
                // abandon: активный квест сбрасывается; завершённый — нельзя.
                if let Some((_, state, _)) = row {
                    if state == "done" {
                        return vec![Self::err(seq, ErrorCode::Internal, "квест завершён")];
                    }
                }
                if let Err(e) = db::quests::abandon(pool, &char_id, &quest_id).await {
                    warn!("quest (abandon): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // Свежее состояние после перехода → ack `{quest}` + событие.
        let progress = db::quests::progress(pool, &char_id)
            .await
            .unwrap_or_default();
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);
        let state = db::quests::build_state(def, row);
        outs.push(Self::ack_payload(
            seq,
            &serde_json::json!({ "quest": state }),
        ));
        outs.push(out_event(
            "evt/quest/updated",
            None,
            serde_json::json!({ "quest": state }),
        ));
        // Сдача: обновить пул в снапшоте сессии.
        if op == "turn_in" {
            if let Ok(Some(row)) = db::characters::get(pool, &char_id).await {
                if let Some(snap) = self.char.as_mut() {
                    snap.xp_pool = row.xp_pool;
                }
            }
        }
        outs
    }

    // ── помощники ────────────────────────────────────────────────────────────

    /// Окно rate-limit auth-команд по IP (D30); `Some(retry_after_s)` — отклон.
    fn rate_limit(&self) -> Option<u64> {
        self.state.rate.check(self.peer.ip())
    }

    /// M14/F19: может ли аккаунт писать в system-чат и удалять сообщения.
    fn is_moderator(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "moderator")
    }

    fn require_auth(&self, seq: u32) -> Option<Out> {
        if self.authenticated {
            None
        } else {
            Some(Self::err(
                seq,
                ErrorCode::AuthExpired,
                "нет активной сессии",
            ))
        }
    }

    fn payload_of<T: DeserializeOwned>(env: &Envelope, seq: u32) -> Result<T, Out> {
        serde_json::from_value(env.payload.clone().unwrap_or(Value::Null)).map_err(|e| {
            Self::err(
                seq,
                ErrorCode::Internal,
                format!("некорректный payload: {e}"),
            )
        })
    }

    fn err(seq: u32, code: ErrorCode, message: impl Into<String>) -> Out {
        out_ack(Ack::error(seq, None, &code, message))
    }

    fn ack_payload<T: serde::Serialize>(seq: u32, payload: &T) -> Out {
        match Ack::ok_with_payload(seq, None, payload) {
            Ok(ack) => out_ack(ack),
            Err(e) => Self::err(seq, ErrorCode::Internal, format!("encode: {e}")),
        }
    }

    fn ack_rate_limited(seq: u32, retry_after_s: u64) -> Out {
        let mut ack = Ack::error(
            seq,
            None,
            &ErrorCode::RateLimited,
            "слишком много попыток — повторите позже",
        );
        if let Some(err) = ack.error.as_mut() {
            err.retry_after_s = Some(retry_after_s);
        }
        out_ack(ack)
    }
}

/// Сводка §5 `characters[]`: `profile` — поле `CharacterSummary` (D19,
/// ADR-005 пришло с M11); `location` — **имя** локации из сида (M12.3).
fn summary_of(row: &db::characters::CharacterRow, location_name: String) -> CharacterSummary {
    CharacterSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        profile: Some(row.profile.clone()),
        location: location_name,
        summary: None,
        crime: Vec::new(),
        online: false, // реестр online — M12.7 (whisper/группа)
        extra: Default::default(),
    }
}

/// Проводной вид предмета §6: поля инстанса + определение из сида (D26).
fn item_snapshot(item: &db::items::ItemRow, def: &db::items::ItemDefRow) -> ItemSnapshot {
    ItemSnapshot {
        uid: item.uid.clone(),
        item_type: def.item_type.clone(),
        name: def.name.clone(),
        rarity: def.rarity.clone(),
        upgrade: def.upgrade + item.upgrade,
        stats: def.stats.clone().as_object().cloned().unwrap_or_default(),
        requires: def
            .requires
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        sockets: {
            let sockets: Vec<Socket> =
                serde_json::from_value(item.sockets.clone()).unwrap_or_default();
            if sockets.is_empty() {
                None
            } else {
                Some(sockets)
            }
        },
        qty: Some(i64::from(item.qty)),
        gold_value: Some(i64::from(def.gold_value)),
        slot: def.slot.clone(),
        hands: Some(def.hands),
        // M17/F30b: навык оружия в снапшот — включает вклад навыка в
        // производные характеристики (stats.rs, док модуля).
        extra: {
            let mut m = serde_json::Map::new();
            if let Some(skill) = &def.skill {
                m.insert("skill".into(), serde_json::Value::String(skill.clone()));
            }
            m
        },
    }
}

/// Payload рецепта для `cmd/craft.recipes` (skills.md §5.4, открытый §6).
fn recipe_payload(r: &db::craft::RecipeRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "name": r.name,
        "profession": r.profession,
        "rank": r.rank,
        "station": r.station,
        "tool": r.tool_type.as_ref().map(|t| serde_json::json!({ "type": t })),
        "requires": r.requires,
        "inputs": r.inputs,
        "time_s": r.time_s,
        "xp": { "profession": i64::from(r.xp_profession) },
        "output": { "type": r.output_type, "qty": i64::from(r.output_qty) },
        "station_label": r.station_label,
    })
}

/// id определений лута сущности (§6 `extra.loot` — открытый объект).
fn entity_loot_defs(entity: &Entity) -> Vec<String> {
    entity
        .extra
        .get("loot")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Локация-заглушка: id отсутствует в сиде/БД (D26). pub(crate) —
/// используется death.rs для респаун-событий.
pub(crate) fn stub_location(location_id: &str) -> LocationSnapshot {
    location_snapshot(location_id)
}

/// Локация снапшота: до сида контента (M12.3, D26) — заглушка по id
/// (таблицы locations ещё нет: имя = id, описание/выходы/сущности пустые).
fn location_snapshot(location_id: &str) -> LocationSnapshot {
    LocationSnapshot {
        id: location_id.to_string(),
        name: location_id.to_string(),
        flags: Vec::new(),
        text: String::new(),
        exits: Vec::new(),
        entities: Vec::new(),
        extra: Default::default(),
    }
}

/// M17/F30d: родительный падеж основных характеристик для ошибок
/// требований (локализация сервера — русская, §8).
fn attr_genitive(id: &str) -> &str {
    match id {
        "str" => "силы",
        "dex" => "ловкости",
        "vit" => "стойкости",
        "int" => "интеллекта",
        other => other,
    }
}

/// Стартовые профили `char.create` (ADR-003: Воин/Лучник/Маг — M17/F30b
/// «Вор» переименован в «Лучник»); расширение списка — capability (§7).
const PROFILES: &[&str] = &["warrior", "archer", "mage"];

/// Ack → текст конверта (§2.2).
fn out_ack(ack: Ack) -> Out {
    Out::Text(serde_json::to_string(&ack).expect("ack сериализуется"))
}

/// Событие → текст конверта (§2.3) с `ts`; сверх лимита §1 — закрытие
/// (нарезка крупных событий — M12.8, §1).
pub(crate) fn out_event(ty: &str, state_version: Option<u64>, payload: Value) -> Out {
    let text = event_text(ty, state_version, payload);
    if text.is_empty() {
        Out::Close
    } else {
        Out::Text(text)
    }
}

/// Событие в список адресованной рассылки (адресат, текст) — планы боя/каста.
fn push_evt<T: serde::Serialize>(
    plan: &mut Vec<(String, String)>,
    addr: &str,
    ty: &str,
    payload: &T,
) {
    if let Some(text) = event_payload_text(ty, payload) {
        plan.push((addr.to_string(), text));
    }
}

/// JSON-текст конверта события из payload'а (`None` — сверх лимита §1).
fn event_payload_text<T: serde::Serialize>(ty: &str, payload: &T) -> Option<String> {
    let text = event_text(ty, None, serde_json::to_value(payload).unwrap_or_default());
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Текст конверта события (§2.3); пустая строка — сверх лимита §1
/// (death.rs адресует события строками).
pub(crate) fn event_text(ty: &str, state_version: Option<u64>, payload: Value) -> String {
    let mut env = Envelope::new(ty, Some(payload));
    env.state_version = state_version;
    env.ts = Some(crate::time::now_rfc3339());
    env.to_json_limited().unwrap_or_else(|e| {
        warn!("событие {ty} не влезает в лимит 64 KiB: {e}");
        String::new()
    })
}
