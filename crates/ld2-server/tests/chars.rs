//! Персонажи против реального сервера (гейт: LD2_TEST_DATABASE_URL, ФР-02,
//! §3/§5): создание/список/выбор/удаление, лимиты из конфига, персистентность
//! и монотонный `state_version` через reconnect.

mod common;

use ld2_test_kit::checks;
use ld2_test_kit::WsClient;
use serde_json::{json, Value};

fn unique_name(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("время монотонно")
        .as_nanos();
    format!("{prefix}_{nanos:x}")
}

/// Регистрация свежего аккаунта: клиент готов к `char.*`; логин возвращается
/// для повторного входа (тесты reconnect).
async fn registered(ws: &str) -> (WsClient, String) {
    let login = checks::unique_login();
    let mut c = WsClient::connect(ws).await;
    let seq = c
        .send(
            "cmd/auth.register",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let start = c.wait_event("evt/session.start").await;
    assert!(start["payload"]["characters"]
        .as_array()
        .is_some_and(|a| a.is_empty()));
    (c, login)
}

async fn create_char(c: &mut WsClient, name: &str, profile: &str) -> Value {
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": name, "profile": profile }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "create {name}: {ack}");
    ack
}

async fn select_char(c: &mut WsClient, char_id: &str) -> (u64, Value) {
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "select {char_id}: {ack}");
    let state_version = ack["state_version"]
        .as_u64()
        .expect("state_version в ack (§3)");
    let snap = c.wait_event("evt/state/snapshot").await;
    assert_eq!(
        snap["state_version"], state_version,
        "версия снапшота = ack"
    );
    (state_version, snap)
}

#[tokio::test]
async fn character_lifecycle_persists_across_reconnect() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    let (mut c, login) = registered(&ws).await;
    let name = unique_name("Следопыт");
    let ack = create_char(&mut c, &name, "warrior").await;
    let char_id = ack["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    assert_eq!(ack["payload"]["character"]["name"], name);
    assert_eq!(
        ack["payload"]["character"]["profile"], "warrior",
        "ADR-005 в extra"
    );

    let seq = c.send("cmd/char.list", json!({})).await;
    let list = c.ack(seq).await;
    assert_eq!(
        list["payload"]["characters"]
            .as_array()
            .expect("list")
            .len(),
        1
    );

    // Имя занято (глобально среди живых) → E_NAME_TAKEN; слоты свободны,
    // поэтому лимит не маскирует проверку.
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": name, "profile": "mage" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_NAME_TAKEN");

    // Первый выбор: state_version стартует с 1 (бамп с 0, §3).
    let (sv1, snap) = select_char(&mut c, &char_id).await;
    assert_eq!(sv1, 1, "первый бамп");
    assert_eq!(snap["payload"]["char"]["name"], name);
    assert_eq!(
        snap["payload"]["char"]["hp"]["max"], 100,
        "база из миграции 0003"
    );
    assert!(
        snap["payload"]["location"].is_object(),
        "снапшот обязан содержать локацию (§3)"
    );

    // Переподключение: персонаж переживает обрыв, версия монотонна (§3).
    let seq = c.send("cmd/auth.logout", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let mut c2 = WsClient::connect(&ws).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let start = c2.wait_event("evt/session.start").await;
    assert_eq!(
        start["payload"]["characters"]
            .as_array()
            .expect("chars")
            .len(),
        1,
        "персонаж пережил переподключение"
    );
    let (sv2, _) = select_char(&mut c2, &char_id).await;
    assert_eq!(sv2, sv1 + 1, "state_version монотонен через reconnect (§3)");
}

#[tokio::test]
async fn character_validation_and_slot_limit() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Лимит слотов — из конфига (D25, DoD M2): ставим 1.
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.gameplay.char_slot_limit = 1;
    })
    .await;
    let ws = format!("ws://{addr}/ws");
    let (mut c, _) = registered(&ws).await;

    // Невалидный профиль → E_BAD_PROFILE (до заполнения слота).
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": unique_name("Воин"), "profile": "paladin" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_BAD_PROFILE");

    // Слишком короткое имя → E_NAME_TAKEN («занято или некорректно», §9).
    let seq = c
        .send("cmd/char.create", json!({ "name": "x", "profile": "mage" }))
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_NAME_TAKEN");

    create_char(&mut c, &unique_name("Единственный"), "rogue").await;

    // Второй персонаж при limit = 1 → E_CHAR_LIMIT (конфиг, не код).
    // Проверка лимита идёт раньше занятости имени — как в моке, поэтому
    // E_NAME_TAKEN здесь маскируется лимитом (покрыт в lifecycle-тесте).
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": unique_name("Лишний"), "profile": "warrior" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_CHAR_LIMIT");
}

#[tokio::test]
async fn character_delete_rules() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    let (mut a, _) = registered(&ws).await;
    let doomed_name = unique_name("Удаляемый");
    let doomed = create_char(&mut a, &doomed_name, "warrior").await;
    let doomed_id = doomed["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let keeper = create_char(&mut a, &unique_name("Оставшийся"), "mage").await;
    let keeper_id = keeper["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();

    // Удаляем выбранного: сессия сбрасывается, список уменьшается.
    select_char(&mut a, &doomed_id).await;
    let seq = a
        .send("cmd/char.delete", json!({ "char_id": doomed_id }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let seq = a.send("cmd/char.list", json!({})).await;
    let list = a.ack(seq).await;
    assert_eq!(
        list["payload"]["characters"]
            .as_array()
            .expect("list")
            .len(),
        1
    );

    // Последнего удалить нельзя → E_CHAR_LIMIT (паритет с моком).
    let seq = a
        .send("cmd/char.delete", json!({ "char_id": keeper_id }))
        .await;
    assert_eq!(a.ack(seq).await["error"]["code"], "E_CHAR_LIMIT");

    // Чужой персонаж → «не найден» (E_NAME_TAKEN, паритет с моком).
    let (mut b, _) = registered(&ws).await;
    let foreign = create_char(&mut b, &unique_name("Чужой"), "rogue").await;
    let foreign_id = foreign["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let seq = a
        .send("cmd/char.delete", json!({ "char_id": foreign_id }))
        .await;
    assert_eq!(a.ack(seq).await["error"]["code"], "E_NAME_TAKEN");

    // Удалённое имя освобождается (частичный индекс миграции 0003).
    let seq = b
        .send(
            "cmd/char.create",
            json!({ "name": doomed_name, "profile": "mage" }),
        )
        .await;
    assert_eq!(
        b.ack(seq).await["result"],
        "ok",
        "имя после delete свободно"
    );
}
