//! Общее состояние сервера, разделяемое всеми WS-соединениями (D31: один
//! процесс, один шард).

use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;

/// Разделяемое состояние: конфиг (D25) и пул PostgreSQL (D24).
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
}

pub type SharedState = Arc<AppState>;
