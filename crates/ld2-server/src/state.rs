//! Общее состояние сервера, разделяемое всеми WS-соединениями (D31: один
//! процесс, один шард).

use std::sync::Arc;

use sqlx::PgPool;

use crate::auth::AuthRateLimiter;
use crate::config::Config;

/// Разделяемое состояние: конфиг (D25), пул PostgreSQL (D24) и
/// rate-limit auth-команд по IP (D30).
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub rate: AuthRateLimiter,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        Self {
            config: Arc::new(config),
            pool,
            rate: AuthRateLimiter::new(),
        }
    }
}
