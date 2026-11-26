//! RF1c: авторизация соединения (ADR-006, D29/D30) — регистрация,
//! вход, resume по refresh-токену, выход, ротация access-токена и общий
//! хвост входа `finish_login`.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{AuthLogin, AuthRegister, AuthResume};
use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::ErrorCode;
use tracing::warn;

use crate::{auth, db};

use super::{out_ack, out_event, presence, Out, Session};

impl Session {
    /// Регистрация = немедленный вход (ADR-006); лимиты — из конфига (D25).
    pub(super) async fn auth_register(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

    pub(super) async fn auth_login(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

    pub(super) async fn auth_resume(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
    pub(super) async fn auth_logout(&mut self, seq: u32) -> Vec<Out> {
        if let Some(account_id) = self.account_id {
            if let Err(e) = db::accounts::revoke_refreshes(&self.state.pool, account_id).await {
                warn!("отзыв refresh не удался: {e}");
            }
        }
        // Сессия выходит из мира: бой и рассылка (M12.5).
        if let Some(char_id) = self.selected_char_id.take() {
            let gone = {
                let world = self.state.world.lock().expect("мир");
                world.char_location(&char_id)
            };
            let mine = {
                let mut world = self.state.world.lock().expect("мир");
                world.unbind_char(&char_id, &self.peer.to_string())
            };
            // Присутствие: соседи видят уход из мира (только наша сессия).
            if mine {
                if let Some(location_id) = gone {
                    presence::announce_despawn(&self.state, &location_id, &char_id, None);
                }
            }
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
    pub(super) async fn auth_refresh(&mut self, seq: u32) -> Vec<Out> {
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
    pub(super) async fn finish_login(
        &mut self,
        seq: u32,
        account_id: i64,
        login: String,
    ) -> Vec<Out> {
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
}
