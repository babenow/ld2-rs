//! M19 (ADR-013): `GET /.well-known/ld2.json` на живом сервере
//! (гейт: LD2_TEST_DATABASE_URL): включённый discovery — 200 + адрес +
//! версия контракта + min_client; секция не настроена — 404.

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn discovery_doc_returns_address_and_version() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // multi_thread: ureq блокирует поток, сервер-таск работает рядом.
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.discovery.address = Some("wss://game.example.com/ws".into());
        cfg.server.min_client = Some("0.17.0".into());
    })
    .await;

    let response = ureq::get(&format!("http://{addr}/.well-known/ld2.json"))
        .call()
        .expect("ответ well-known");
    assert_eq!(response.status(), 200);
    let doc: serde_json::Value =
        serde_json::from_str(&response.into_string().expect("тело")).expect("JSON");
    assert_eq!(
        doc["address"], "wss://game.example.com/ws",
        "адрес из конфига: {doc}"
    );
    assert_eq!(doc["version"], 1, "версия WS-контракта: {doc}");
    assert_eq!(doc["min_client"], "0.17.0", "min_client из [server]: {doc}");
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_doc_disabled_returns_404() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Шаблон (example) содержит [discovery] — выключаем явно.
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.discovery.address = None;
    })
    .await;

    let err = ureq::get(&format!("http://{addr}/.well-known/ld2.json"))
        .call()
        .expect_err("discovery выключен —404");
    match err {
        ureq::Error::Status(code, _) => {
            assert_eq!(code, 404, "выключенный discovery — 404");
        }
        other => panic!("ожидалась HTTP-ошибка404, получено: {other}"),
    }
}
