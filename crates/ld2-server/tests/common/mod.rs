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

/// Сервер на эфемерном порту (свой state, миграции + сид контента) → адрес.
pub async fn start_server_with(db_url: &str, tweak: impl FnOnce(&mut Config)) -> String {
    let cfg = test_config(db_url, tweak);
    let pool = db::connect(&cfg.database).await.expect("БД + миграции");
    ld2_server::content::seed(&pool, &cfg.world.content_dir)
        .await
        .expect("сид контента");
    let state = Arc::new(AppState::new(cfg, pool));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = app::serve(listener, state).await;
    });
    addr.to_string()
}

/// Сервер с конфигом-шаблоном → ws-url (`ws://addr/ws`).
pub async fn start_ws_server(db_url: &str) -> String {
    format!("ws://{}/ws", start_server_with(db_url, |_| {}).await)
}
