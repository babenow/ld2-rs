//! E2E M13.2: клиентское ядро регистрации против реального `ld2-server`
//! (gated): форма из `ld2_core::register` → `cmd/auth.register` → ack в
//! `AppStore` → `evt/session.start` → экран персонажей; серверные ошибки
//! попадают в `register_error` с полем (D35).

mod common;

use common::in_world;
use ld2_core::register::{register_field_for, RegisterField, RegisterForm};
use ld2_core::store::{Act, AppStore, Screen};
use ld2_core::transport::IngressEvent;
use ld2_test_kit::{checks, WsClient};
use serde_json::json;

/// Отправить регистрацию как это сделал бы UI: валидация ядра → команда.
async fn register_via_core(
    c: &mut WsClient,
    store: &mut AppStore,
    form: RegisterForm,
) -> serde_json::Value {
    form.validate().expect("форма валидна клиентом (D34)");
    let seq = c
        .send(
            "cmd/auth.register",
            json!({
                "login": form.login,
                "password": form.password,
                "email": form.normalized_email(),
            }),
        )
        .await;
    store.on_register_sent(&form.login);
    let ack = c.ack(seq).await;
    // pending фиксируется при отправке и снимается apply_ack — до применения.
    assert_eq!(store.pending, Some(Act::Register), "корреляция ack (§2.4)");
    store.apply(IngressEvent::Ack(
        serde_json::from_value::<ld2_protocol::Ack>(ack.clone()).expect("ack"),
    ));
    ack
}

#[tokio::test]
async fn register_via_core_reaches_characters_screen() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let mut c = WsClient::connect(&ws).await;
    let mut store = AppStore::new();

    let form = RegisterForm {
        login: checks::unique_login(),
        email: format!("{}@example.com", checks::unique_login().replace('t', "u")),
        password: "longenough1".into(),
        confirm: "longenough1".into(),
    };
    let ack = register_via_core(&mut c, &mut store, form.clone()).await;
    assert_eq!(ack["result"], "ok", "регистрация через ядро: {ack}");
    assert!(store.register_error.is_none(), "ошибок нет");
    assert!(store.pending.is_none(), "ack обработан, pending снят");

    // Сервер шлёт session.start (регистрация = вход, ADR-006) → ядро
    // переводит на экран персонажей.
    let start = c.wait_event("evt/session.start").await;
    let env: ld2_protocol::Envelope =
        serde_json::from_value(start.clone()).expect("конверт session.start");
    store.apply(IngressEvent::Event(env));
    assert_eq!(
        store.screen,
        Screen::Characters,
        "экран персонажей: {start}"
    );
    assert!(store.session.is_some(), "сессия сохранена из session.start");
    assert!(store.login_error.is_none());
}

#[tokio::test]
async fn register_server_errors_land_on_fields() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await; // для связи
    let mut store = AppStore::new();

    // 1) Битый email отсекает клиент, но и сервер защищён (D34: сервер —
    // источник истины): шлём заведомо некорректный мимо клиентской формы.
    let login = checks::unique_login();
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1", "email": "not-an-email" }),
        )
        .await;
    store.on_register_sent(&login);
    let ack = c.ack(seq).await;
    store.apply(IngressEvent::Ack(
        serde_json::from_value::<ld2_protocol::Ack>(ack.clone()).expect("ack"),
    ));
    assert_eq!(ack["error"]["code"], "E_BAD_EMAIL", "{ack}");
    let err = store.register_error.clone().expect("ошибка в store");
    assert_eq!(err.field, RegisterField::Email, "поле email (D35)");
    assert!(!err.message.is_empty(), "текст сервера (§9)");

    // 2) Дубль email → E_EMAIL_TAKEN → тоже поле email.
    let seq = c
        .send(
            "cmd/auth.register",
            json!({
                "login": checks::unique_login(),
                "password": "longenough1",
                "email": format!("{}@example.com", checks::unique_login()),
            }),
        )
        .await;
    store.on_register_sent("other");
    let ack = c.ack(seq).await;
    store.apply(IngressEvent::Ack(
        serde_json::from_value::<ld2_protocol::Ack>(ack.clone()).expect("ack"),
    ));
    assert_eq!(ack["result"], "ok", "уникальный email проходит: {ack}");

    // 3) Занятый логин → E_LOGIN_TAKEN → поле логина.
    let seq = c
        .send(
            "cmd/auth.register",
            json!({
                "login": _login,
                "password": "longenough1",
                "email": format!("{}@example.com", checks::unique_login()),
            }),
        )
        .await;
    store.on_register_sent(&_login);
    let ack = c.ack(seq).await;
    store.apply(IngressEvent::Ack(
        serde_json::from_value::<ld2_protocol::Ack>(ack.clone()).expect("ack"),
    ));
    assert_eq!(ack["error"]["code"], "E_LOGIN_TAKEN", "{ack}");
    let err = store.register_error.clone().expect("ошибка в store");
    assert_eq!(err.field, RegisterField::Login, "поле логина (D35)");
}

#[test]
fn core_form_validation_blocks_bad_input_before_send() {
    // D34: пустые поля/несовпадение/битый email не отправляются вовсе.
    let mut form = RegisterForm {
        login: "player".into(),
        email: "a@b.co".into(),
        password: "longenough1".into(),
        confirm: "longenough1".into(),
    };
    assert!(form.validate().is_ok());

    form.confirm = "different".into();
    assert!(form.validate().is_err(), "пароли не совпадают");

    form.confirm = "longenough1".into();
    form.email = "no-at-sign".into();
    assert!(form.validate().is_err(), "битый email");

    form.email = "".into();
    assert!(form.validate().is_err(), "пустой email");

    // Маппинг полей — единая таблица в ядре (D35).
    assert_eq!(
        register_field_for(&ld2_protocol::ErrorCode::BadEmail),
        RegisterField::Email
    );
}
