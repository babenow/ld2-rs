//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{AuthLogin, AuthRegister, AuthResume};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{Pong, TimeSync};
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
        // Список персонажей заполняется в фазе «персонажи» (M12.2).
        let config = auth::session_config(&cfg, access.clone(), refresh, Vec::new());
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
