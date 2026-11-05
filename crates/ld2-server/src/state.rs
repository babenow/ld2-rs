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
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        Self {
            config: Arc::new(config),
            pool,
            rate: AuthRateLimiter::new(),
            world: Mutex::new(World::default()),
        }
    }
}
