//! M36: бездействие персонажа — предупреждение и перевод в офлайн.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn afk_warns_then_goes_offline() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Короткий таймаут (3 с) — предупреждение и офлайн за один тест.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.afk_timeout_s = 3;
        })
        .await
    );
    let (mut idle, _login, char_id) = in_world(&ws).await;
    let (mut watcher, _login2, _watcher_id) = in_world(&ws).await;

    // Предупреждение на последних 5 % (не позже секунды до конца).
    let warn = idle.wait_event("evt/session/afk").await;
    assert_eq!(warn["payload"]["offline"], serde_json::Value::Null);
    assert!(
        warn["payload"]["remaining_s"].as_i64().unwrap_or(0) >= 1,
        "предупреждение с остатком: {warn}"
    );

    // Офлайн: экран выбора персонажа (аккаунт остаётся), персонаж
    // исчезает для соседей.
    let offline = idle.wait_event("evt/session/afk").await;
    assert_eq!(offline["payload"]["offline"], true, "офлайн: {offline}");
    let gone = watcher.wait_event("evt/entity/despawn").await;
    assert_eq!(gone["payload"]["entity_id"], char_id, "исчез для всех");

    // Аккаунт не разлогинен — персонажа можно выбрать снова.
    let seq = idle
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let ack = idle.ack(seq).await;
    assert_eq!(ack["result"], "ok", "повторный вход в мир: {ack}");
}
