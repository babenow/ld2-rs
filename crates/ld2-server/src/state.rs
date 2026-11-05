//! Общее состояние сервера, разделяемое всеми WS-соединениями (D31: один
//! процесс, один шард).

use std::sync::{Arc, Mutex};

use sqlx::PgPool;

use crate::auth::AuthRateLimiter;
use crate::config::Config;
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
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Self {
            config: Arc::new(config),
            pool,
            rate: AuthRateLimiter::new(),
            world: Mutex::new(World::default()),
            shutdown,
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
