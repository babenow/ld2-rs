//! Присутствие игроков в локации (M29): снапшот §6 содержит соседей
//! строками `kind: player`, а вход/переход/выход соседей приходят
//! адресными `evt/entity/spawn`/`evt/entity/despawn`.

mod common;

use serde_json::json;

#[tokio::test]
async fn players_in_same_location_are_visible_and_leave_events() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = common::in_world(&ws).await;
    let (mut b, _, char_b) = common::in_world(&ws).await;

    // A видит вход B в игру: evt/entity/spawn с kind=player.
    let spawn = a.wait_event("evt/entity/spawn").await;
    assert_eq!(
        spawn["payload"]["entity"]["id"], char_b,
        "id новичка: {spawn}"
    );
    assert_eq!(spawn["payload"]["entity"]["kind"], "player");
    let location = spawn["payload"]["location_id"]
        .as_str()
        .expect("локация")
        .to_string();
    assert!(
        !spawn["payload"]["entity"]["name"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "имя игрока: {spawn}"
    );

    // Снапшот B: A есть в «Игроках», сам B — нет.
    let seq = b
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    let _ = b.ack(seq).await;
    let snap = b.wait_event("evt/state/snapshot").await;
    let entities = snap["payload"]["location"]["entities"]
        .as_array()
        .expect("entities");
    let players: Vec<&serde_json::Value> =
        entities.iter().filter(|e| e["kind"] == "player").collect();
    assert!(
        players.iter().any(|e| e["id"] == char_a),
        "A виден в снапшоте B: {players:?}"
    );
    assert!(
        !players.iter().any(|e| e["id"] == char_b),
        "себя в «Игроках» не видно: {players:?}"
    );

    // Переход B в лес → A получает despawn из старой локации.
    let seq = b
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "переход: {ack}");
    b.wait_event("evt/location/changed").await;
    let despawn = a.wait_event("evt/entity/despawn").await;
    assert_eq!(despawn["payload"]["entity_id"], char_b);
    assert_eq!(despawn["payload"]["kind"], "player");
    assert_eq!(despawn["payload"]["location_id"], location);
    // Готовая строка журнала: {имя} ушел/ушла — {метка выхода}.
    let went = despawn["payload"]["text"].as_str().unwrap_or_default();
    let b_name = spawn["payload"]["entity"]["name"]
        .as_str()
        .unwrap_or_default();
    assert!(
        went.starts_with(&format!("{b_name} ушел")) || went.starts_with(&format!("{b_name} ушла")),
        "строка ухода: {went:?}"
    );
    assert!(!went.ends_with("— "), "метка выхода обязательна: {went:?}");

    // Возврат B → A снова видит spawn — сюда же строка прихода.
    let seq = b
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "возврат: {ack}");
    let back = a.wait_event("evt/entity/spawn").await;
    assert_eq!(back["payload"]["entity"]["id"], char_b);
    let came = back["payload"]["text"].as_str().unwrap_or_default();
    assert!(
        came == format!("{b_name} пришел") || came == format!("{b_name} пришла"),
        "строка прихода: {came:?}"
    );

    // Дисконнект B → A получает despawn.
    b.close().await;
    let gone = a.wait_event("evt/entity/despawn").await;
    assert_eq!(gone["payload"]["entity_id"], char_b);
    assert_eq!(gone["payload"]["kind"], "player");
}
