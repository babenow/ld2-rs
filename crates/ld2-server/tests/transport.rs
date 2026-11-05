//! WS-транспорт §1/§2 против реального сервера (гейт: LD2_TEST_DATABASE_URL).
//!
//! Общий набор `ld2-test-kit` (D32): те же проверки гоняются и на моке
//! (`shared_contract_suite_passes_on_mock`). Auth/персонажи — M12.2, позже.

use std::path::PathBuf;
use std::sync::Arc;

use ld2_server::app;
use ld2_server::config::Config;
use ld2_server::db;
use ld2_server::state::AppState;
use ld2_test_kit::checks;
use ld2_test_kit::WsClient;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Сервер на эфемерном порту с конфигом-шаблоном и тестовой БД → ws-url.
async fn start_server(db_url: &str) -> String {
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("server.example.toml");
    let mut cfg = Config::load(&example).expect("шаблон валиден");
    cfg.database.url = db_url.to_string();

    let pool = db::connect(&cfg.database).await.expect("БД + миграции");
    let state = Arc::new(AppState {
        config: Arc::new(cfg),
        pool,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = app::serve(listener, state).await;
    });
    format!("ws://{addr}/ws")
}

#[tokio::test]
async fn transport_checks_pass_on_real_server() {
    let Some(db_url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws_url = start_server(&db_url).await;

    let mut c = WsClient::connect(&ws_url).await;
    checks::ping_and_time_sync(&mut c).await;
    checks::seq_window_deduplicates(&mut c).await;
    checks::unknown_command(&mut c).await;
    // лимит фрейма закрывает соединение — только последней на этом клиенте
    checks::frame_size_limit(&mut c).await;
}
