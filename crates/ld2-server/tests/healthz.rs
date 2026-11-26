//! `/healthz` на живом сервере с PostgreSQL (гейт: LD2_TEST_DATABASE_URL).

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn healthz_returns_ok() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // multi_thread: ureq блокирует поток, сервер-таск должен работать рядом.
    let addr = common::start_server_with(&db_url, |_| {}).await;

    let response = ureq::get(&format!("http://{addr}/healthz"))
        .call()
        .expect("ответ /healthz");
    assert_eq!(response.status(), 200);
    assert_eq!(response.into_string().expect("тело ответа").trim(), "ok");
}
