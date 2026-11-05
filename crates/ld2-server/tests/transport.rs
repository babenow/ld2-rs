//! WS-транспорт §1/§2 против реального сервера (гейт: LD2_TEST_DATABASE_URL).
//!
//! Общий набор `ld2-test-kit` (D32): те же проверки гоняются и на моке
//! (`shared_contract_suite_passes_on_mock`). Auth — tests/auth.rs.

mod common;

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;
use serde_json::json;

#[tokio::test]
async fn transport_checks_pass_on_real_server() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws_url = common::start_ws_server(&db_url).await;

    let mut c = WsClient::connect(&ws_url).await;
    checks::ping_and_time_sync(&mut c).await;
    checks::seq_window_deduplicates(&mut c).await;
    checks::unknown_command(&mut c).await;
    // лимит фрейма закрывает соединение — только последней на этом клиенте
    checks::frame_size_limit(&mut c).await;
}

/// Полный общий набор `ld2-test-kit` (D32) на реальном сервере — зеркало
/// `shared_contract_suite_passes_on_mock`: один набор, обе реализации.
/// Гейт M12.2: «общий контрактный набор зелёный для мока и сервера».
#[tokio::test]
async fn shared_contract_suite_passes_on_real_server() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    // Аккаунт для hello: регистрация = вход (ADR-006), затем чистый login.
    let login = checks::unique_login();
    let mut setup = WsClient::connect(&ws).await;
    let seq = setup
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(setup.ack(seq).await["result"], "ok");
    setup.wait_event("evt/session.start").await;

    let mut c = WsClient::connect(&ws).await;
    checks::hello(&mut c, &login, "longenough1", 8).await;
    checks::ping_and_time_sync(&mut c).await;
    checks::seq_window_deduplicates(&mut c).await;
    checks::unknown_command(&mut c).await;
    checks::frame_size_limit(&mut c).await; // закрывает соединение — последней

    let mut c2 = WsClient::connect(&ws).await;
    checks::register_flow(&mut c2).await;
}
