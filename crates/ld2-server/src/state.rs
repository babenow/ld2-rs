//! Общее состояние сервера, разделяемое всеми WS-соединениями (D31: один
//! процесс, один шард).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use sqlx::PgPool;

use crate::auth::AuthRateLimiter;
use crate::config::Config;
use crate::dialogs::DialogNpc;
use crate::shops::ShopNpc;
use crate::social::SocialState;
use crate::world::World;

/// Контент, читаемый из файлов в память (не из БД): диалоги/магазины NPC +
/// sha256-версия контента.
///
/// M27 (hot reload, фаза 1): заменяется целиком при перезагрузке контента;
/// читатели берут `Arc`-снимок через [`AppState::content`], поэтому замена
/// не рвёт выполняющиеся команды.
pub struct ContentState {
    /// sha256-версия контента (`meta.content_version`).
    pub version: String,
    /// Диалоги NPC (M18/F35, `content/dialogs.json`).
    pub dialogs: HashMap<String, DialogNpc>,
    /// Магазины NPC (M18/F39, `content/shops.json`).
    pub shops: HashMap<String, ShopNpc>,
}

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
    /// Контент из файлов (диалоги/магазины/версия): снимок в `Arc`,
    /// подменяется hot reload'ом (M27).
    content: RwLock<Arc<ContentState>>,
    /// M26: кэш карты мира (`cmd/map.world`) — строится лениво; hot reload
    /// сбрасывает в `None`, следующий запрос построит заново.
    pub map_cache: tokio::sync::RwLock<Option<Arc<crate::map::MapCache>>>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// `content_version` — версия, применённая сидом при старте (M27:
    /// показывается в `/healthz` и в логе hot reload).
    pub fn new(config: Config, pool: PgPool, content_version: String) -> Self {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        // M18/F35/F39: сломанные файлы не валят сервер — warn + пусто
        // (валидность ловят контент-тесты и `content::validate`).
        let content = ContentState {
            version: content_version,
            dialogs: crate::dialogs::load(&config.world.content_dir)
                .map_err(|e| tracing::warn!("диалоги NPC не загружены: {e}"))
                .unwrap_or_default(),
            shops: crate::shops::load(&config.world.content_dir)
                .map_err(|e| tracing::warn!("магазины NPC не загружены: {e}"))
                .unwrap_or_default(),
        };
        Self {
            config: Arc::new(config),
            pool,
            rate: AuthRateLimiter::new(),
            world: Mutex::new(World::default()),
            shutdown,
            social: Mutex::new(SocialState::default()),
            content: RwLock::new(Arc::new(content)),
            map_cache: tokio::sync::RwLock::new(None),
        }
    }

    /// Снимок контента (M27): `Arc` переживает подмену при hot reload.
    pub fn content(&self) -> Arc<ContentState> {
        self.content.read().expect("контент").clone()
    }

    /// Подменить контент (hot reload; вызывать после успешного сида).
    pub fn set_content(&self, content: ContentState) {
        *self.content.write().expect("контент") = Arc::new(content);
    }

    /// Сбросить кэш карты мира: следующий `cmd/map.world` построит её
    /// заново по обновлённому контенту (M27).
    pub async fn invalidate_map_cache(&self) {
        *self.map_cache.write().await = None;
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
