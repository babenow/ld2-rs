//! Секции `[discovery]`, `[server]`, `[database]`, `[auth]`.

use super::*;

/// M19 (ADR-013): discovery подключения — `GET /.well-known/ld2.json`
/// отдаёт `{ address, version, min_client? }`; клиент хранит домен и
/// резолвит адрес сервера без перевыпуска (ROADMAP M19).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiscoverySection {
    /// WS-адрес для документа (`ws://…` dev / `wss://…` prod);
    /// пусто/нет — discovery выключен (404).
    pub address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// `host:port` WS-слушателя (в M12.1 — HTTP `/healthz`).
    pub bind: String,
    /// Имя сервера → `evt/session.start` §5 `server.name`.
    pub name: String,
    /// Регион → §5 `server.region`.
    pub region: String,
    /// Опционально (§5 `server.min_client`): если задан и версия клиента
    /// меньше — вход блокируется. В TOML отсутствует ключ, не `null`.
    #[serde(default)]
    pub min_client: Option<String>,
    /// Уровень логирования: EnvFilter-директива (`"info"`, `"info,sqlx=warn"`).
    pub log_level: String,
    /// M24: разрешить `/scenario …` всем авторизованным (dev/тесты;
    /// дыра из бэклога — раньше префикс не проверялся вовсе). В
    /// прод-конфиге ключ отсутствует → false; админ проходит всегда.
    #[serde(default)]
    pub allow_scenario: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseSection {
    /// `postgres://user:pass@host:5432/db` — секрет, chmod 600, не коммитить.
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    pub access_ttl_s: u64,
    pub refresh_ttl_s: u64,
    /// Открытая регистрация `cmd/auth.register` (D28).
    pub register_enabled: bool,
    pub min_login_len: usize,
    pub min_password_len: usize,
}
