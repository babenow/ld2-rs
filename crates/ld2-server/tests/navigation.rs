//! Мир M12.3 против реального сервера (гейт: LD2_TEST_DATABASE_URL):
//! сид контента (D26), навигация «деревня → лес», осмотр сущностей,
//! журнал-события, персист позиции и `state.resync` после reconnect (§3/§4.5).

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

async fn move_exit(c: &mut WsClient, exit_id: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": exit_id }))
        .await;
    c.ack(seq).await
}

async fn navigation_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    let (mut c, login, char_id) = in_world(&ws).await;

    // Спавн из сида (D26): новый персонаж — в деревне.
    let seq = c.send("cmd/location.look", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "look: {ack}");
    let loc = &ack["payload"]["location"];
    assert_eq!(loc["id"], "l_village", "спавн — из content/locations.json");
    assert_eq!(loc["name"], "Деревня Изба");

    // Осмотр статической сущности (§7): Староста в деревне.
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "npc_elder" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "entity.info: {ack}");
    assert_eq!(ack["payload"]["entity"]["name"], "Староста");
    assert!(ack["payload"]["loot"]
        .as_array()
        .is_some_and(|l| l.is_empty()));

    // Повторный select — зафиксировать текущую версию для сравнения (§3).
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let sv_before = c.ack(seq).await["state_version"].as_u64().expect("sv");
    c.wait_event("evt/state/snapshot").await;
    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["result"], "ok", "move: {ack}");
    assert_eq!(ack["payload"]["location"]["id"], "l_forest", "цель выхода");
    let sv_after = ack["state_version"].as_u64().expect("state_version в ack");
    assert_eq!(sv_after, sv_before + 1, "переход бампит версию (§3)");
    let changed = c.wait_event("evt/location/changed").await;
    assert_eq!(changed["payload"]["location"]["id"], "l_forest");
    assert_eq!(changed["state_version"], sv_after);

    // Лес: осмотр волка; несуществующий выход → E_EXIT_BLOCKED.
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "m_3" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["payload"]["entity"]["name"], "Волк");
    assert_eq!(ack["payload"]["entity"]["hp"]["max"], 18);

    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["error"]["code"], "E_EXIT_BLOCKED", "нет выхода: {ack}");

    // Reconnect: logout → login → последний персонаж восстановлен (§4.5),
    // state.resync → полный снапшот с прежней версией (без бампа).
    let seq = c.send("cmd/auth.logout", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let mut c2 = WsClient::connect(&ws).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok", "повторный вход");
    let start = c2.wait_event("evt/session.start").await;
    assert_eq!(
        start["payload"]["characters"][0]["location"], "Черный лес",
        "сводка §5 несёт ИМЯ локации из сида"
    );

    let seq = c2
        .send(
            "cmd/state.resync",
            json!({ "last_state_version": sv_after }),
        )
        .await;
    let ack = c2.ack(seq).await;
    assert_eq!(ack["result"], "ok", "resync: {ack}");
    assert_eq!(
        ack["state_version"], sv_after,
        "resync не бампит версию (§3)"
    );
    let snap = c2.wait_event("evt/state/snapshot").await;
    assert_eq!(snap["state_version"], sv_after);
    assert_eq!(
        snap["payload"]["location"]["id"], "l_forest",
        "позиция пережила reconnect (§4.5)"
    );
    assert_eq!(
        snap["payload"]["char"]["name"],
        start["payload"]["characters"][0]["name"]
    );
}

async fn seed_preserves_manual_edits() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Первый старт: сид применяется (версии в meta ещё нет).
    let _ = common::start_server_with(&db_url, |_| {}).await;

    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");

    // Ручная правка в БД (не в контенте).
    sqlx::query("UPDATE locations SET name = 'Ручное название' WHERE id = 'l_village'")
        .execute(&pool)
        .await
        .expect("ручная правка");

    // «Рестарт»: тот же контент → сид не трогает БД (критерий M12.3).
    let _ = common::start_server_with(&db_url, |_| {}).await;
    let name: String = sqlx::query_scalar("SELECT name FROM locations WHERE id = 'l_village'")
        .fetch_one(&pool)
        .await
        .expect("строка локации");
    assert_eq!(
        name, "Ручное название",
        "идемпотентный сид не перетирает правки"
    );

    // Возвращаем имя для остальных тестов.
    sqlx::query("UPDATE locations SET name = 'Деревня Изба' WHERE id = 'l_village'")
        .execute(&pool)
        .await
        .expect("возврат имени");
    pool.close().await;
}

/// Один тест-обёртка: внутри бинаря тесты выполняются параллельно, а фаза
/// сида правит имя локации, которое проверяет навигация — выполняем
/// последовательно в одном тесте (защита от гонки).
#[tokio::test]
async fn navigation_and_seed_idempotency() {
    navigation_flow().await;
    seed_preserves_manual_edits().await;
}
