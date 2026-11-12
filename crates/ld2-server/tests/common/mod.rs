//! Общий подъём тестового сервера (гейт: `LD2_TEST_DATABASE_URL`).
//!
//! Каждый test-бинарь использует свою часть helper'ов — отсюда `dead_code`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use ld2_server::app;
use ld2_server::config::Config;
use ld2_server::db;
use ld2_server::state::AppState;

/// URL тестовой БД; `None` → тест печатает «пропуск» и выходит.
pub fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Конфиг-шаблон (`server.example.toml`) с тестовой БД, контентом из
/// репозитория и правками теста.
pub fn test_config(db_url: &str, tweak: impl FnOnce(&mut Config)) -> Config {
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("server.example.toml");
    let mut cfg = Config::load(&example).expect("шаблон валиден");
    cfg.database.url = db_url.to_string();
    // Контент — из репозитория (CWD тестов — корень crate, M12.3/D26).
    cfg.world.content_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    tweak(&mut cfg);
    cfg
}

/// Управляемый тестовый сервер: можно остановить («рестарт», M12.6) —
/// `stop()` гасит тик-луп и accept-loop через `request_shutdown`.
pub struct TestServer {
    pub ws: String,
    state: std::sync::Arc<AppState>,
}

impl TestServer {
    /// Остановить сервер (graceful): тик-луп и WS-приём завершаются.
    pub async fn stop(self) {
        self.state.request_shutdown();
        // Немного даём accept-loop/tiku завершиться до следующего старта.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Сервер на эфемерном порту с хэндлом остановки.
pub async fn start_server_guarded(db_url: &str, tweak: impl FnOnce(&mut Config)) -> TestServer {
    let addr = start_server_with(db_url, tweak).await;
    let state = TEST_STATE
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|m| m.get(&addr).cloned())
        .expect("state запущенного сервера");
    TestServer {
        ws: format!("ws://{addr}/ws"),
        state,
    }
}

/// Реестр state'ов запущенных серверов (по адресу) — для остановки в тестах.
static TEST_STATE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<AppState>>>,
> = std::sync::OnceLock::new();

/// Сервер на эфемерном порту (свой state, миграции + сид контента) → адрес.
pub async fn start_server_with(db_url: &str, tweak: impl FnOnce(&mut Config)) -> String {
    // Логи сервера в вывод теста (диагностика падений; инициализация — разовая).
    static LOG: std::sync::Once = std::sync::Once::new();
    LOG.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
    });
    let cfg = test_config(db_url, tweak);
    let pool = db::connect(&cfg.database).await.expect("БД + миграции");
    ld2_server::content::seed(&pool, &cfg.world.content_dir)
        .await
        .expect("сид контента");
    let state = Arc::new(AppState::new(cfg, pool));
    ld2_server::tick::init(&state).await.expect("мир + тик-луп");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let key = addr.to_string();
    let registry = TEST_STATE.get_or_init(Default::default);
    if let Ok(mut map) = registry.lock() {
        map.insert(key.clone(), state.clone());
    }
    let key_for_task = key.clone();
    tokio::spawn(async move {
        let _ = app::serve(listener, state).await;
        if let Ok(mut map) = registry.lock() {
            map.remove(&key_for_task);
        }
    });
    key
}

/// Сервер с конфигом-шаблоном → ws-url (`ws://addr/ws`).
pub async fn start_ws_server(db_url: &str) -> String {
    format!("ws://{}/ws", start_server_with(db_url, |_| {}).await)
}

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Регистрация + персонаж + выбор: клиент в мире (общий e2e-флоу).
/// Возвращает (клиент, логин, char_id).
pub async fn in_world(ws: &str) -> (WsClient, String, String) {
    in_world_profile(ws, "warrior").await
}

/// M17/F30b: тот же e2e-флоу с произвольным стартовым профилем
/// (`warrior`/`archer`/`mage`).
pub async fn in_world_profile(ws: &str, profile: &str) -> (WsClient, String, String) {
    let mut c = WsClient::connect(ws).await;
    let login = checks::unique_login();
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "регистрация: {ack}");
    c.wait_event("evt/session.start").await;

    // nanos может совпасть внутри одного процесса (параллельные тесты,
    // гранулярность часов) — добавляем счётчик.
    static NAME_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let name = format!(
        "Следопыт_{:x}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время")
            .as_nanos(),
        NAME_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": name, "profile": profile }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание персонажа: {ack}");
    let char_id = ack["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();

    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "выбор персонажа: {ack}");
    c.wait_event("evt/state/snapshot").await;
    (c, login, char_id)
}
