//! WS-транспорт §1/§2 против реального сервера (гейт: LD2_TEST_DATABASE_URL).
//!
//! Общий набор `ld2-test-kit` (D32): те же проверки гоняются и на моке
//! (`shared_contract_suite_passes_on_mock`). Auth — tests/auth.rs.

mod common;

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;

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
