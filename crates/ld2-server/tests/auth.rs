//! Авторизация против реального сервера (гейт: LD2_TEST_DATABASE_URL,
//! ADR-006, D28–D30): регистрация/вход/refresh/logout/resume, коды §9,
//! rate-limit по IP.

mod common;

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;
use serde_json::json;

#[tokio::test]
async fn register_login_logout_resume_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    // Регистрация = немедленный вход (ADR-006): ack + session.start.
    let mut c = WsClient::connect(&ws).await;
    let login = checks::unique_login();
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "регистрация: {ack}");
    let start = c.wait_event("evt/session.start").await;
    let refresh = start["payload"]["session"]["refresh"]["token"]
        .as_str()
        .expect("refresh-токен §5")
        .to_string();
    let access1 = ack["payload"]["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    assert!(
        start["payload"]["characters"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "новый аккаунт — без персонажей (ADR-006)"
    );

    // Идемпотентность ретрая (ADR-006): тот же seq → точный кэшированный
    // ack, а НЕ E_LOGIN_TAKEN.
    c.send_with_seq(
        "cmd/auth.register",
        json!({ "login": login, "password": "longenough1", "email": checks::unique_email() }),
        seq,
    )
    .await;
    let dup = c.recv_json().await;
    assert_eq!(dup, ack, "повтор seq — кэшированный ack (§2.4.2)");

    // session.refresh ротирует access-токен (§7).
    let seq = c.send("cmd/session.refresh", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "refresh: {ack}");
    let access2 = ack["payload"]["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    assert_ne!(access1, access2, "access ротируется");

    // logout отзывает refresh (§4.5).
    let seq = c.send("cmd/auth.logout", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    let mut c2 = WsClient::connect(&ws).await;
    let seq = c2
        .send("cmd/auth.resume", json!({ "refresh_token": refresh }))
        .await;
    let ack = c2.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_AUTH_EXPIRED",
        "после logout resume невозможен: {ack}"
    );

    // Повторный вход паролем работает (пароль не отзывается).
    let mut c3 = WsClient::connect(&ws).await;
    let seq = c3
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    assert_eq!(c3.ack(seq).await["result"], "ok");
}

#[tokio::test]
async fn credential_errors_and_register_disabled() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };

    // Неверный пароль → E_BAD_CREDENTIALS; занятый/слабый — как в контракте.
    let ws = common::start_ws_server(&db_url).await;
    let mut c = WsClient::connect(&ws).await;
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": checks::unique_login(), "password": "wrong" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_BAD_CREDENTIALS");

    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": "ok_name", "password": "short", "email": checks::unique_email() }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_WEAK_PASSWORD");

    let taken = checks::unique_login();
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": taken, "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": taken, "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_LOGIN_TAKEN");

    // auth.register_enabled = false (D25) → E_REGISTER_DISABLED.
    let ws_off = common::start_server_with(&db_url, |cfg| {
        cfg.auth.register_enabled = false;
    })
    .await;
    let mut c2 = WsClient::connect(&format!("ws://{ws_off}/ws")).await;
    let seq = c2
        .send(
            "cmd/auth.register",
            json!({ "login": checks::unique_login(), "password": "longenough1", "email": checks::unique_email() }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["error"]["code"], "E_REGISTER_DISABLED");
}

#[tokio::test]
async fn rate_limit_auth_commands_per_ip() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let mut c = WsClient::connect(&ws).await;

    // Окно — 20 попыток (auth.rs): первые 20 отвечают, 21-я — троттлинг.
    for i in 0..20 {
        let seq = c
            .send(
                "cmd/auth.login",
                json!({ "login": "no_such_login", "password": "wrong" }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(
            ack["error"]["code"], "E_BAD_CREDENTIALS",
            "попытка {i}: {ack}"
        );
    }
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": "no_such_login", "password": "wrong" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_RATE_LIMITED",
        "21-я попытка: {ack}"
    );
    assert!(
        ack["error"]["retry_after_s"].is_u64(),
        "retry_after_s обязателен при троттлинге (§9): {ack}"
    );
}
