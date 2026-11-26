//! Группа (ФР-14, M12.7b): создание, приглашение, принятие/отклонение,
//! исключение лидера, выход; лимит состава из конфига.

mod common;

use common::in_world;
use serde_json::json;

/// Подключить ещё одного игрока.
async fn second_player(ws: &str) -> (ld2_test_kit::WsClient, String, String) {
    in_world(ws).await
}

#[tokio::test]
async fn party_create_invite_accept_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = second_player(&ws).await;
    let _ = char_b;

    // A создаёт группу → ack {party}, A — лидер.
    let seq = a.send("cmd/party.create", json!({})).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    assert_eq!(ack["payload"]["party"]["leader"], char_a);
    let party_id = ack["payload"]["party"]["id"]
        .as_str()
        .expect("party id")
        .to_string();

    // A уже в группе → повторный create отказ.
    let seq = a.send("cmd/party.create", json!({})).await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "повтор: {ack}");

    // Приглашение B (онлайн) → evt/party/updated c invite адресату.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "инвайт");
    let invite = b.wait_event("evt/party/updated").await;
    assert!(
        invite["payload"]["party"].is_null(),
        "партия ещё не объединена"
    );
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .expect("invite_id")
        .to_string();
    assert_eq!(invite["payload"]["invite"]["from"], char_a);

    // Чужой invite_id → отказ.
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": "inv_nope" }))
        .await;
    let ack = b.ack(seq).await;
    assert_ne!(ack["result"], "ok", "чужой инвайт: {ack}");

    // B принимает → ack {party} обоим + состав из двух.
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "принятие: {ack}");
    let members = ack["payload"]["party"]["members"]
        .as_array()
        .expect("members");
    assert_eq!(members.len(), 2, "двое в группе: {members:?}");
    assert_eq!(ack["payload"]["party"]["id"], party_id);
    // A получает обновление состава.
    let upd = a.wait_event("evt/party/updated").await;
    assert_eq!(
        upd["payload"]["party"]["members"].as_array().unwrap().len(),
        2,
        "A видит двоих"
    );
    // B тоже (событие после ack).
    let upd_b = b.wait_event("evt/party/updated").await;
    assert_eq!(upd_b["payload"]["party"]["id"], party_id);

    // Уже в группе → приглашение в другую отказ.
    let seq = b
        .send("cmd/party.invite", json!({ "player_id": char_a }))
        .await;
    let ack = b.ack(seq).await;
    assert_ne!(ack["result"], "ok", "B уже в группе: {ack}");

    // B выходит → состав одного, A получает обновление.
    let seq = b.send("cmd/party.leave", json!({})).await;
    assert_eq!(b.ack(seq).await["result"], "ok", "выход");
    let upd_a = a.wait_event("evt/party/updated").await;
    assert_eq!(
        upd_a["payload"]["party"]["members"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "остался один"
    );
    let upd_b = b.wait_event("evt/party/updated").await;
    assert!(upd_b["payload"]["party"].is_null(), "у B группы нет");
}

#[tokio::test]
async fn party_invite_offline_and_self_rejected() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;

    let seq = a.send("cmd/party.create", json!({})).await;
    assert_eq!(a.ack(seq).await["result"], "ok");

    // Пригласить себя → отказ.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_a }))
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "себя: {ack}");

    // Пригласить несуществующего/офлайн → E_TARGET_GONE.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": "nobody_here" }))
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "офлайн: {ack}");

    // Выход без группы → отказ.
    drop(a);
    let (mut c, _, _) = second_player(&ws).await;
    let seq = c.send("cmd/party.leave", json!({})).await;
    let ack = c.ack(seq).await;
    assert_ne!(ack["result"], "ok", "выход без группы: {ack}");
}

#[tokio::test]
async fn party_leader_can_kick_and_limit_applies() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Лимит 2: третий участник не влезает.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.party_max_members = 2;
        })
        .await
    );
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;
    let (mut c, _, char_c) = in_world(&ws).await;

    let seq = a.send("cmd/party.create", json!({})).await;
    assert_eq!(a.ack(seq).await["result"], "ok");

    // Инвайт + принятие B.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let invite = b.wait_event("evt/party/updated").await;
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    a.wait_event("evt/party/updated").await;
    b.wait_event("evt/party/updated").await;

    // Группа полна (2/2) → приглашение C отклоняется при принятии.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_c }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "инвайт C");
    let invite = c.wait_event("evt/party/updated").await;
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();
    let seq = c
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_PARTY_FULL", "лимит: {ack}");

    // Не-лидер исключить не может.
    let seq = b
        .send("cmd/party.kick", json!({ "player_id": char_a }))
        .await;
    let ack = b.ack(seq).await;
    assert_ne!(ack["result"], "ok", "не лидер: {ack}");

    // Лидер исключает B → B получает party: null, A — состав без B.
    let seq = a
        .send("cmd/party.kick", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "кик");
    let upd_b = b.wait_event("evt/party/updated").await;
    assert!(upd_b["payload"]["party"].is_null(), "B исключён: {upd_b}");
    let upd_a = a.wait_event("evt/party/updated").await;
    let members = upd_a["payload"]["party"]["members"]
        .as_array()
        .expect("members");
    assert_eq!(members.len(), 1, "остался лидер: {members:?}");
}

#[tokio::test]
async fn party_decline_removes_invite() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    let seq = a.send("cmd/party.create", json!({})).await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let invite = b.wait_event("evt/party/updated").await;
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Отклонение → ок, повторный ответ по нему — отказ.
    let seq = b
        .send("cmd/party.decline", json!({ "invite_id": invite_id }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok", "отклонение");
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_ne!(ack["result"], "ok", "после отказа: {ack}");

    // B не в группе → снапшот без партии.
    let seq = b
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    let snap = b.wait_event("evt/state/snapshot").await;
    assert!(snap["payload"]["party"].is_null(), "B без группы");
}

/// M14: у приглашающего ещё нет группы — она создаётся автоматически
/// (окно группы скрыто, пока участников < 2).
#[tokio::test]
async fn party_invite_autocreates_party_for_inviter() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = second_player(&ws).await;

    // A группу НЕ создавал — приглашение порождает её само.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(
        a.ack(seq).await["result"],
        "ok",
        "инвайт без предварительного create"
    );
    let invite = b.wait_event("evt/party/updated").await;
    assert!(
        invite["payload"]["party"].is_null(),
        "состав B ещё не обновлён: {invite}"
    );
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .expect("invite_id")
        .to_string();
    assert_eq!(invite["payload"]["invite"]["from"], char_a);

    // B принимает → группа из двух, лидер — пригласивший A.
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "принятие: {ack}");
    assert_eq!(ack["payload"]["party"]["leader"], char_a, "лидер — A");
    let members = ack["payload"]["party"]["members"]
        .as_array()
        .expect("members");
    assert_eq!(members.len(), 2, "автогруппа из двух: {members:?}");

    // A видит состав (evt/party/updated после принятия).
    let upd = a.wait_event("evt/party/updated").await;
    assert_eq!(
        upd["payload"]["party"]["members"].as_array().unwrap().len(),
        2,
        "A видит двоих"
    );
}
