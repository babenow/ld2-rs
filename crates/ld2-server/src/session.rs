//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{AuthLogin, AuthRegister, AuthResume, CharCreate, CharDelete, CharSelect};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{Pong, StateSnapshot, TimeSync};
use ld2_protocol::types::{CharacterState, CharacterSummary, Hp, LocationSnapshot, Mp};
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
        // §5 characters[]: живые персонажи аккаунта (ошибка — не ломает вход).
        let chars = match db::characters::list(&self.state.pool, account_id).await {
            Ok(rows) => rows.iter().map(summary_of).collect(),
            Err(e) => {
                warn!("список персонажей при входе: {e}");
                Vec::new()
            }
        };
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
                let characters: Vec<CharacterSummary> = rows.iter().map(summary_of).collect();
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
            Ok(Some(row)) => vec![Self::ack_payload(
                seq,
                &serde_json::json!({ "character": summary_of(&row) }),
            )],
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
        let pool = &self.state.pool;
        let Some(row) = (match db::characters::find(pool, account_id, &cmd.char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("char.select: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            // Паритет с моком: не найден/чужой → «персонаж не найден».
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        // §3: state_version монотонен на сессию персонажа и переживает
        // reconnect — бамп атомарный в БД.
        let state_version = match db::characters::bump_state_version(pool, &row.id).await {
            Ok(v) => v as u64,
            Err(e) => {
                warn!("char.select (state_version): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let snapshot = StateSnapshot {
            state_version,
            char: character_state_of(&row),
            location: location_snapshot(&row.location_id),
            party: None,
            guild: None,
            server_ts: crate::time::now_epoch(),
            extra: Default::default(),
        };
        self.char = Some(snapshot.char.clone());
        self.selected_char_id = Some(row.id.clone());
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

/// Сводка §5 `characters[]`: `profile` кладём в `extra` — поле появится в
/// `ld2-protocol` при мерже ADR-005 (M11), wire-JSON идентичен (D19).
fn summary_of(row: &db::characters::CharacterRow) -> CharacterSummary {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "profile".into(),
        serde_json::Value::String(row.profile.clone()),
    );
    CharacterSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        location: row.location_id.clone(),
        summary: None,
        crime: Vec::new(),
        online: false, // реестр online — M12.7 (whisper/группа)
        extra,
    }
}

/// Лист персонажа §3: M12.2 — база из строки БД; навыки/инвентарь/квесты
/// добавляются фазами M12.4+.
fn character_state_of(row: &db::characters::CharacterRow) -> CharacterState {
    CharacterState {
        id: row.id.clone(),
        name: row.name.clone(),
        attrs: Default::default(),
        skills: Vec::new(),
        professions: Vec::new(),
        hp: Hp {
            cur: i64::from(row.hp_cur),
            max: i64::from(row.hp_max),
        },
        mp: Mp {
            cur: i64::from(row.mp_cur),
            max: i64::from(row.mp_max),
        },
        xp_pool: row.xp_pool,
        equip: Default::default(),
        inventory: Vec::new(),
        crime: Default::default(),
        quests: Vec::new(),
        extra: Default::default(),
    }
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
