//! `/healthz` на живом сервере с PostgreSQL (гейт: LD2_TEST_DATABASE_URL).

use std::path::PathBuf;

use ld2_server::app;
use ld2_server::config::Config;
use ld2_server::db;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// multi_thread-флавор: `ureq` блокирует поток вызова, а сервер-таск должен
/// работать параллельно (на однопоточном рантайме — дедлок).
#[tokio::test(flavor = "multi_thread")]
async fn healthz_returns_ok() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };

    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("server.example.toml");
    let mut cfg = Config::load(&example).expect("шаблон валиден");
    cfg.database.url = url;

    let pool = db::connect(&cfg.database).await.expect("БД + миграции");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = app::serve(listener, pool).await;
    });

    let response = ureq::get(&format!("http://{addr}/healthz"))
        .call()
        .expect("ответ /healthz");
    assert_eq!(response.status(), 200);
    assert_eq!(response.into_string().expect("тело ответа").trim(), "ok");
}
