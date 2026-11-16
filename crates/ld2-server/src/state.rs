//! Общее состояние сервера, разделяемое всеми WS-соединениями (D31: один
//! процесс, один шард).

use std::sync::{Arc, Mutex};

use sqlx::PgPool;

use crate::auth::AuthRateLimiter;
use crate::config::Config;
use crate::social::SocialState;
use crate::world::World;

/// Разделяемое состояние: конфиг (D25), пул PostgreSQL (D24) и
/// rate-limit auth-команд по IP (D30).
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub rate: AuthRateLimiter,
    /// Мир в памяти: мобы/трупы, бой, реестр соединений (M12.5, D31).
    pub world: Mutex<World>,
    /// Остановка фоновых задач (тик-луп): `true` → завершаемся. Нужна и
    /// prod (graceful shutdown), и тестам («рестарт» сервера, M12.6).
    shutdown: tokio::sync::watch::Sender<bool>,
    /// Социальное состояние в памяти: группы (ФР-14, M12.7b) — эфемерно.
    pub social: Mutex<SocialState>,
    /// M18/F35: диалоги NPC в памяти (`content/dialogs.json`, при старте;
    /// без БД — в отличие от кулдаунов `every_s`).
    pub dialogs: std::collections::HashMap<String, crate::dialogs::DialogNpc>,
    /// M18/F39: магазины NPC в памяти (`content/shops.json`, при старте).
    pub shops: std::collections::HashMap<String, crate::shops::ShopNpc>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        // M18/F35: сломанный dialogs.json не валит сервер — warn + пусто
        // (валидность контента ловят контент-тесты в CI).
        let dialogs = crate::dialogs::load(&config.world.content_dir)
            .map_err(|e| tracing::warn!("диалоги NPC не загружены: {e}"))
            .unwrap_or_default();
        // M18/F39: сломанный shops.json — warn + пусто (контент-тесты в CI).
        let shops = crate::shops::load(&config.world.content_dir)
            .map_err(|e| tracing::warn!("магазины NPC не загружены: {e}"))
            .unwrap_or_default();
        Self {
            config: Arc::new(config),
            pool,
            rate: AuthRateLimiter::new(),
            world: Mutex::new(World::default()),
            shutdown,
            social: Mutex::new(SocialState::default()),
            dialogs,
            shops,
        }
    }

    /// Подписка на остановку (тик-луп, graceful shutdown).
    pub fn shutdown_rx(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Запросить остановку фоновых задач (graceful shutdown/тесты).
    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}
