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

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Регистрация + персонаж + выбор: клиент в мире (общий e2e-флоу).
/// Возвращает (клиент, логин, char_id).
pub async fn in_world(ws: &str) -> (WsClient, String, String) {
    let mut c = WsClient::connect(ws).await;
    let login = checks::unique_login();
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "регистрация: {ack}");
    c.wait_event("evt/session.start").await;

    let name = format!(
        "Следопыт_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время")
            .as_nanos()
    );
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": name, "profile": "warrior" }),
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
