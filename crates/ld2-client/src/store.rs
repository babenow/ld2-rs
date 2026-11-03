//! `AppStore` — единое состояние клиента, собираемое **только** из событий
//! сервера (ТЗ §6.2). UI — проекция стора; редьюсер чистый и тестируется без
//! egui (ТЗ §15.5: «UI-фича без reducer-теста не мёржится»).
//!
//! Побочные эффекты (keyring, токен в транспорт) возвращаются из [`AppStore::apply`]
//! и исполняются слоем приложения — редьюсер остаётся детерминированным.

use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::CharacterSummary;
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
    /// Нейтральное сообщение (создан/удалён/транспорт).
    pub notice: Option<String>,
    /// Логин последней попытки входа — предзаполнение формы.
    pub last_login: Option<String>,
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
            notice: None,
            last_login: None,
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
            }
            return vec![];
        }

        match act {
            // Успех логина подтверждён ack'ом; дальше ждём evt/session.start.
            Act::Login => {}
            Act::Select => {}
            Act::Delete => {}
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
                self.snapshot = Some(snap);
                self.screen = Screen::Game;
                self.select_error = None;
                self.notice = None;
                vec![]
            }
            _ => vec![], // остальные события M2 не нужны (M3+)
        }
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
    pub fn back_to_characters(&mut self) {
        self.screen = Screen::Characters;
        self.snapshot = None;
        self.select_error = None;
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
}
