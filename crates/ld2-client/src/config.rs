//! Конфигурация клиента: `<config_dir>/config.toml` (ТЗ ФР-19).
//!
//! Максимум серверных лимитов живёт в `evt/session.start` (protocol.md §5) —
//! здесь только локальные настройки: хост, язык.

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Dev-адрес мока (PLAN-M0.md Фаза 2). В release-сборке `ws://` запрещён (ТЗ §8).
pub const DEFAULT_HOST: &str = "ws://127.0.0.1:24817/ws";

/// Каталоги данных приложения (macOS: `~/Library/Application Support/ld2/...`).
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("ru", "ld2", "ld2").context("не удалось определить каталоги данных")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Адрес сервера: `wss://` в release, `ws://` — только dev/localhost.
    pub host: String,
    /// Код языка UI: `ru` | `en` (D8: таблицы ru/en с M0).
    pub lang: String,
    /// Последний логин — предзаполнение формы и ключ keyring (ФР-01).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            lang: "ru".to_string(),
            last_login: None,
        }
    }
}

/// Загрузка/сохранение `config.toml`; файл создаётся с дефолтом при первом запуске.
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn open() -> Result<Self> {
        let dirs = project_dirs()?;
        Ok(Self {
            path: dirs.config_dir().join("config.toml"),
        })
    }

    pub fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            let cfg = Config::default();
            self.save(&cfg)?;
            tracing::info!("создан конфиг по умолчанию: {}", self.path.display());
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("чтение конфига {}", self.path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("разбор конфига {}", self.path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, cfg: &Config) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(cfg).context("сериализация конфига")?;
        std::fs::write(&self.path, text)
            .with_context(|| format!("запись конфига {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trip() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).expect("сериализация");
        let parsed: Config = toml::from_str(&text).expect("разбор");
        assert_eq!(cfg, parsed);
        assert_eq!(parsed.host, DEFAULT_HOST);
        assert_eq!(parsed.lang, "ru");
    }
}
