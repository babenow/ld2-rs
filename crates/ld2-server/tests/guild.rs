//! Гильдии (ФР-16, M12.7d): создание/поиск, вступление по приглашению,
//! ранги и права, журнал `evt/guild/log`, персистентность в БД (D27).

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn guild_create_browse_join_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    // Поиск до создания — пусто (или не содержит нашего имени).
    let seq = a
        .send(
            "cmd/guild.browse",
            json!({ "query": "Стальные_волки_нет_таких" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "browse: {ack}");
    assert!(ack["payload"]["guilds"].as_array().unwrap().is_empty());

    // A создаёт гильдию → ack {guild} с A-лидером + журнал.
    let unique = format!(
        "Волки_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let seq = a.send("cmd/guild.create", json!({ "name": unique })).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    assert_eq!(ack["payload"]["guild"]["members"][0]["rank"], "leader");
    let guild_id = ack["payload"]["guild"]["id"]
        .as_str()
        .expect("guild id")
        .to_string();
    // Журнал после ack (evt/guild/log).
    let log = a.wait_event("evt/guild/log").await;
    assert_eq!(log["payload"]["entry"]["kind"], "create");

    // Повторное создание с тем же именем → отказ; A уже в гильдии → отказ.
    let seq = a.send("cmd/guild.create", json!({ "name": unique })).await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "дубликат имени: {ack}");
    let seq = a
        .send("cmd/guild.create", json!({ "name": "Другая" }))
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "уже состоит: {ack}");

    // Поиск находит созданную (запрос по полному имени — тесты делят БД).
    let seq = a
        .send("cmd/guild.browse", json!({ "query": &unique }))
        .await;
    let ack = a.ack(seq).await;
    let found = ack["payload"]["guilds"].as_array().expect("guilds");
    assert!(
        found.iter().any(|g| g["id"] == guild_id),
        "найдена своя: {found:?}"
    );

    // A приглашает B → evt/guild/updated с invite адресату.
    let seq = a
        .send("cmd/guild.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "инвайт");
    let invite_evt = b.wait_event("evt/guild/updated").await;
    let invite_id = invite_evt["payload"].get("invite").expect("invite");
    assert_eq!(invite_id["guild_id"], guild_id);

    // B вступает (по guild_id из приглашения) → ack {guild} + событие A.
    let seq = b
        .send("cmd/guild.join", json!({ "guild_id": guild_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "вступление: {ack}");
    let members = ack["payload"]["guild"]["members"]
        .as_array()
        .expect("members");
    assert_eq!(members.len(), 2, "двое: {members:?}");
    // A получает обновление состава + журнал вступления.
    let upd = a.wait_event("evt/guild/updated").await;
    assert_eq!(
        upd["payload"]["guild"]["members"].as_array().unwrap().len(),
        2
    );

    // B уже в гильдии → повторный join отказ.
    let seq = b
        .send("cmd/guild.join", json!({ "guild_id": guild_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_ne!(ack["result"], "ok", "повтор: {ack}");

    // Снапшот B несёт гильдию.
    let seq = b
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    let snap = b.wait_event("evt/state/snapshot").await;
    assert_eq!(
        snap["payload"]["guild"]["id"], guild_id,
        "гильдия в снапшоте"
    );
    let _ = char_a;
}

#[tokio::test]
async fn guild_ranks_and_permissions() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut leader, _, char_leader) = in_world(&ws).await;
    let (mut member, _, char_member) = in_world(&ws).await;

    let unique = format!(
        "Ранги_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let seq = leader
        .send("cmd/guild.create", json!({ "name": unique }))
        .await;
    let ack = leader.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    let guild_id = ack["payload"]["guild"]["id"].as_str().unwrap().to_string();
    leader.wait_event("evt/guild/log").await;

    // Приглашение + вступление.
    let seq = leader
        .send("cmd/guild.invite", json!({ "player_id": char_member }))
        .await;
    assert_eq!(leader.ack(seq).await["result"], "ok");
    let evt = member.wait_event("evt/guild/updated").await;
    assert!(evt["payload"].get("invite").is_some(), "инвайт: {evt}");
    let seq = member
        .send("cmd/guild.join", json!({ "guild_id": guild_id }))
        .await;
    assert_eq!(member.ack(seq).await["result"], "ok");
    leader.wait_event("evt/guild/updated").await;
    member.wait_event("evt/guild/updated").await;

    // Обычный участник не может приглашать/исключать/менять ранги.
    let seq = member
        .send("cmd/guild.invite", json!({ "player_id": char_leader }))
        .await;
    let ack = member.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_GUILD_PERMISSION", "инвайт: {ack}");
    let seq = member
        .send("cmd/guild.kick", json!({ "player_id": char_leader }))
        .await;
    let ack = member.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_GUILD_PERMISSION", "кик: {ack}");
    let seq = member
        .send(
            "cmd/guild.rank_set",
            json!({ "player_id": char_leader, "rank": "officer" }),
        )
        .await;
    let ack = member.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_GUILD_PERMISSION", "ранг: {ack}");

    // Лидер делает участника офицером → событие состава.
    let seq = leader
        .send(
            "cmd/guild.rank_set",
            json!({ "player_id": char_member, "rank": "officer" }),
        )
        .await;
    assert_eq!(leader.ack(seq).await["result"], "ok", "ранг лидером");
    // Событие получают оба участника — забираем у обоих.
    let upd = leader.wait_event("evt/guild/updated").await;
    member.wait_event("evt/guild/updated").await;
    let officers = upd["payload"]["guild"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["rank"] == "officer")
        .count();
    assert_eq!(officers, 1, "один офицер");

    // Лидер не может исключить себя; может исключить офицера (как лидер).
    let seq = leader
        .send("cmd/guild.kick", json!({ "player_id": char_leader }))
        .await;
    let ack = leader.ack(seq).await;
    assert_ne!(ack["result"], "ok", "кик себя: {ack}");
    let seq = leader
        .send("cmd/guild.kick", json!({ "player_id": char_member }))
        .await;
    assert_eq!(leader.ack(seq).await["result"], "ok", "кик офицера");
    let upd = member.wait_event("evt/guild/updated").await;
    assert!(upd["payload"]["guild"].is_null(), "исключён: {upd}");
}

#[tokio::test]
async fn guild_survives_restart_and_leave() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, login, _) = in_world(&ws).await;

    let unique = format!(
        "Рестарт_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let seq = a.send("cmd/guild.create", json!({ "name": unique })).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    a.wait_event("evt/guild/log").await;
    drop(a);

    // Рестарт: гильдия и лидерство переживают (write-through, D27).
    let ws2 = common::start_ws_server(&db_url).await;
    let mut c2 = ld2_test_kit::WsClient::connect(&ws2).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    c2.wait_event("evt/session.start").await;
    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let snap = c2.wait_event("evt/state/snapshot").await;
    assert!(
        snap["payload"]["guild"]["id"].is_string(),
        "гильдия в снапшоте после рестарта"
    );
    assert_eq!(snap["payload"]["guild"]["members"][0]["rank"], "leader");

    // Выход: гильдия остаётся (последний участник вышел — лидерство
    // передаётся некому, но строка гильдии сохраняется).
    let seq = c2.send("cmd/guild.leave", json!({})).await;
    assert_eq!(c2.ack(seq).await["result"], "ok", "выход");
    let evt = c2.wait_event("evt/guild/updated").await;
    assert!(evt["payload"]["guild"].is_null(), "после выхода: {evt}");

    // Снапшот без гильдии.
    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let snap = c2.wait_event("evt/state/snapshot").await;
    assert!(snap["payload"]["guild"].is_null(), "не состоит");
}
