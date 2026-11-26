//! Конфигурация сервера: единый файл `server.toml` (D25, PLAN-M12).
//!
//! Все настройки — только здесь (env допускается исключительно для пути к
//! файлу: `--config` / `LD2_SERVER_CONFIG`). Секции `[server]`/`[gameplay]` —
//! единственный источник `evt/session.start` (protocol.md §5): лимиты, фичи,
//! преступность, чат, инвентарь, улучшение, мана.
//!
//! Валидация — fail-fast при старте: [`Config::problems`] собирает **все**
//! проблемы сразу, каждая называет ключ (`server.bind: …`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

mod gameplay;
mod server;
mod validate;
mod world;

pub use gameplay::*;
pub use server::*;
pub use world::*;

/// Полный конфиг `server.toml`. Неизвестные ключи — ошибка (опечатки не
/// должны молча игнорироваться).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerSection,
    pub database: DatabaseSection,
    pub auth: AuthSection,
    pub gameplay: GameplaySection,
    pub world: WorldSection,
    /// Модель опыта навыков (skills.md §2/§8).
    pub skills: SkillsSection,
    /// M19 (ADR-013): discovery подключения — well-known-документ;
    /// секция опциональна (нет → маршрут отдаёт404).
    #[serde(default)]
    pub discovery: DiscoverySection,
}

/// Ошибка загрузки/валидации конфига. Всегда указывает путь к файлу, а
/// [`ConfigError::Invalid`] — имена ключей.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать конфиг {}: {err}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        err: std::io::Error,
    },
    #[error("невалидный TOML в {}: {toml_err}", .path.display())]
    Parse {
        path: PathBuf,
        /// В боксе: вариант ≥128 байт на Windows-ABI → clippy
        /// `result_large_err` (CI, -D warnings).
        #[source]
        toml_err: Box<toml::de::Error>,
    },
    #[error("конфиг {} не прошёл валидацию:\n  - {}", .path.display(), .problems.join("\n  - "))]
    Invalid {
        path: PathBuf,
        problems: Vec<String>,
    },
}
