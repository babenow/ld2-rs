//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{
    AuthLogin, AuthRegister, AuthResume, CharCreate, CharDelete, CharSelect, CharXpInvest,
    ChatSend, EntityInfo, ItemDrop, ItemEquip, ItemUnequip, ItemUse, LocationMove, StateResync,
};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{
    CharDeath, CharStats, CharXp, ItemAdded, ItemRemovalReason, ItemRemoved, ItemState,
    LocationChanged, LostItem, Pong, Respawn, StateSnapshot, TimeSync,
};
use ld2_protocol::types::{
    CharacterState, CharacterSummary, Entity, EquipMap, Hp, ItemSnapshot, LocationSnapshot, Mp,
    Requires, SkillSnapshot,
};
use ld2_protocol::ErrorCode;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::warn;

use crate::auth;
use crate::db;
use crate::state::SharedState;

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
    #[allow(dead_code)] // журнал/логика персонажей — M12.2 (фаза «персонажи»)
    login: Option<String>,
    authenticated: bool,
    /// Выбранный персонаж (`char.select`, §3): снапшот и ack-версии — его.
    selected_char_id: Option<String>,
    char: Option<CharacterState>,
    /// Текущая локация персонажа (M12.3): разрешается при входе/переходе.
    location_id: Option<String>,
}

impl Session {
    pub fn new(state: SharedState, peer: SocketAddr) -> Self {
        Self {
            state,
            peer,
            ack_cache: HashMap::new(),
            seq_order: VecDeque::new(),
            account_id: None,
            login: None,
            authenticated: false,
            selected_char_id: None,
            char: None,
            location_id: None,
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
            // лист/инвентарь/опыт (ФР-04/07, M12.4)
            "cmd/char.xp_invest" => self.char_xp_invest(env, seq).await,
            "cmd/item.use" => self.item_use(env, seq).await,
            "cmd/item.equip" => self.item_equip(env, seq).await,
            "cmd/item.unequip" => self.item_unequip(env, seq).await,
            "cmd/item.drop" => self.item_drop(env, seq).await,
            // dev-сценарии (`/scenario …`); чат — ФР-13 в фазе M12.7
            "cmd/chat.send" => self.chat_send(env, seq).await,
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
        // Сначала дешёвая проверка занятости: argon2 дорогой (защита от DoS).
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
        let hash = auth::hash_password(&cmd.password);
        match db::accounts::create_account(&self.state.pool, &cmd.login, &hash).await {
            Ok(Some(id)) => self.finish_login(seq, id, cmd.login).await,
            Ok(None) => vec![Self::err(seq, ErrorCode::LoginTaken, "логин занят")],
            Err(e) => {
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
        self.authenticated = false;
        self.account_id = None;
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
        let config = auth::session_config(&cfg, access.clone(), refresh, chars);
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
        self.location_id = Some(location_id);
        self.selected_char_id = Some(row.id.clone());
        if let Some(account_id) = self.account_id {
            if let Err(e) = db::accounts::set_last_char(&self.state.pool, account_id, &row.id).await
            {
                warn!("last_char_id не обновлён: {e}");
            }
        }
        let snapshot = StateSnapshot {
            state_version,
            char: self.char.clone().expect("выше"),
            location,
            party: None,
            guild: None,
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
                        self.location_id = Some(location_id);
                        self.selected_char_id = Some(row.id.clone());
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
        // Лут статической сущности (§6 `extra.loot` — id определений):
        // взятый не показываем (однократное взятие, M12.4).
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
        let snapshot = StateSnapshot {
            state_version,
            char: char_state,
            location,
            party: None,
            guild: None,
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
        let attrs: BTreeMap<String, i64> =
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
                rank: rank_u32,
                xp,
                xp_to_next: xp_to_next(rank_u32, &self.state.config.skills),
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
        for item in db::items::inventory(pool, &row.id).await? {
            let Some(def) = db::items::def(pool, &item.def_id).await? else {
                warn!("нет определения {} для предмета {}", item.def_id, item.uid);
                continue;
            };
            if let Some(slot) = &item.equipped_slot {
                equip.insert(slot.clone(), Some(item.uid.clone()));
            }
            inventory.push(item_snapshot(&item, &def));
        }

        Ok(CharacterState {
            id: row.id.clone(),
            name: row.name.clone(),
            attrs,
            skills,
            professions: Vec::new(), // M12.6
            hp: Hp {
                cur: i64::from(row.hp_cur),
                max: i64::from(row.hp_max),
            },
            mp: Mp {
                cur: i64::from(row.mp_cur),
                max: i64::from(row.mp_max),
            },
            xp_pool: row.xp_pool,
            equip,
            inventory,
            crime: Default::default(), // M12.5
            quests: Vec::new(),        // M12.6
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
        let cfg = &self.state.config.skills;
        xp += cmd.points;
        loop {
            let need = xp_to_next(rank.max(0) as u32, cfg);
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
        let pool_left = pool_now - cmd.points;
        if let Err(e) =
            db::skills::invest_tx(pool, &char_id, &cmd.skill_id, xp, rank, pool_left).await
        {
            warn!("xp_invest (транзакция): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        vec![
            Self::ack_payload(seq, &serde_json::json!({ "pool_left": pool_left })),
            out_event(
                "evt/char/xp",
                None,
                serde_json::to_value(CharXp {
                    skill_id: cmd.skill_id,
                    amount: cmd.points,
                    skill_xp: xp,
                    rank: Some(rank.max(0) as u32),
                    xp_to_next: Some(xp_to_next(rank.max(0) as u32, cfg)),
                    pool_left: Some(pool_left),
                })
                .unwrap_or_default(),
            ),
        ]
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
        let pool = &self.state.pool;
        let current = self.location_id.clone().unwrap_or_default();
        let Some(loc_row) = (match db::locations::get(pool, &current).await {
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
        // Лимит слотов (D25) — до транзакции.
        match db::items::count(pool, char_id).await {
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
        let Some(def) = (match db::items::def(pool, &def_id).await {
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
        match db::items::take_loot_tx(pool, loot_uid, char_id, &def_id, i64::from(def.qty)).await {
            Ok(None) => vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")],
            Ok(Some(row)) => {
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
                        format!("не хватает {attr}: {have} < {need}"),
                    )];
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
        vec![Self::err(
            seq,
            ErrorCode::Internal,
            "чат появится в фазе M12.7",
        )]
    }

    /// `/scenario death` (ФР-06): теряется **только лут** — опыт навыков и
    /// XP-пул сохраняются (skills.md §2.2); респаун в спавне, hp полное.
    async fn scenario(&mut self, scenario: &str, seq: u32) -> Vec<Out> {
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
        let pool = &self.state.pool;

        // Весь инвентарь теряется (ФР-06) — возвращаем строки для lost_items.
        let lost_rows = match db::items::delete_all(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("/scenario death (инвентарь): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let mut lost_items = Vec::with_capacity(lost_rows.len());
        for row in &lost_rows {
            let (name, rarity) = match db::items::def(pool, &row.def_id).await {
                Ok(Some(def)) => (def.name, Some(def.rarity)),
                Ok(None) => (row.def_id.clone(), None),
                Err(e) => {
                    warn!("/scenario death (определение {}): {e}", row.def_id);
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            lost_items.push(LostItem {
                uid: row.uid.clone(),
                name,
                rarity,
                extra: Default::default(),
            });
        }

        // Респаун: hp полное + позиция = спавн + бамп — одним UPDATE (D27).
        let spawn = match db::locations::spawn_id(pool).await {
            Ok(spawn) => spawn.unwrap_or_default(),
            Err(e) => {
                warn!("/scenario death (спавн): {e}");
                String::new()
            }
        };
        let target = if spawn.is_empty() {
            self.location_id.clone().unwrap_or_default()
        } else {
            spawn
        };
        let Some((hp_cur, hp_max, sv)) =
            (match db::characters::respawn(pool, &char_id, &target).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("/scenario death (респаун): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            })
        else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        let state_version = sv as u64;

        // Обновить сессию под новое состояние.
        let (location_id, location) = self.resolve_location(&target, &char_id).await;
        self.location_id = Some(location_id);
        if let Ok(Some(row)) = db::characters::get(pool, &char_id).await {
            if let Ok(state) = self.character_state(&row).await {
                self.char = Some(state);
            }
        }

        let death = CharDeath {
            victim_char: true,
            lost_items,
            corpse: format!("cor_{}", char_id),
            respawn: Respawn {
                location_id: target,
                delay_s: 0,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let mut outs = vec![
            out_ack(Ack::ok(seq, Some(state_version))),
            out_event(
                "evt/char/death",
                Some(state_version),
                serde_json::to_value(&death).unwrap_or_default(),
            ),
        ];
        for item in &death.lost_items {
            outs.push(out_event(
                "evt/item/removed",
                Some(state_version),
                serde_json::to_value(ItemRemoved {
                    uid: item.uid.clone(),
                    reason: ItemRemovalReason::LostOnDeath,
                })
                .unwrap_or_default(),
            ));
        }
        let mut patch = serde_json::Map::new();
        patch.insert(
            "hp".into(),
            serde_json::json!({ "cur": hp_cur, "max": hp_max }),
        );
        outs.push(out_event(
            "evt/char/stats",
            Some(state_version),
            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
        ));
        outs.push(out_event(
            "evt/location/changed",
            Some(state_version),
            serde_json::to_value(LocationChanged { location }).unwrap_or_default(),
        ));
        outs
    }

    // ── помощники ────────────────────────────────────────────────────────────

    /// Окно rate-limit auth-команд по IP (D30); `Some(retry_after_s)` — отклон.
    fn rate_limit(&self) -> Option<u64> {
        self.state.rate.check(self.peer.ip())
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

/// Порог ранга: `round(base_xp × growth^rank)` (skills.md §2.2,
/// конфиг `[skills]`, D25).
fn xp_to_next(rank: u32, cfg: &crate::config::SkillsSection) -> i64 {
    ((cfg.base_xp as f64) * cfg.growth.powi(rank as i32)).round() as i64
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
        sockets: None, // сокеты рун — M12.5 (ADR-002)
        qty: Some(i64::from(item.qty)),
        gold_value: Some(i64::from(def.gold_value)),
        extra: Default::default(),
    }
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

/// Стартовые профили `char.create` (ADR-003: Воин/Вор/Маг) — тройка v1;
/// расширение списка — capability сервера (§7).
const PROFILES: &[&str] = &["warrior", "rogue", "mage"];

/// Ack → текст конверта (§2.2).
fn out_ack(ack: Ack) -> Out {
    Out::Text(serde_json::to_string(&ack).expect("ack сериализуется"))
}

/// Событие → текст конверта (§2.3) с `ts`; сверх лимита §1 — закрытие
/// (нарезка крупных событий — M12.8, §1).
fn out_event(ty: &str, state_version: Option<u64>, payload: Value) -> Out {
    let mut env = Envelope::new(ty, Some(payload));
    env.state_version = state_version;
    env.ts = Some(crate::time::now_rfc3339());
    match env.to_json_limited() {
        Ok(text) => Out::Text(text),
        Err(e) => {
            warn!("событие {ty} не влезает в лимит 64 KiB: {e}");
            Out::Close
        }
    }
}
