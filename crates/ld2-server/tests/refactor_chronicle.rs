//! RF0d плана рефакторинга: «золотая хронология» событий.
//!
//! Полный golden-payload хрупок (ts, uid, время), поэтому эталон — только
//! последовательность `(тип, state_version)` в детерминированных сценариях.
//! Тест падает, если при переносе кода потерялось ack-сообщение, изменился
//! порядок или поехала нумерация версий состояния.
//!
//! Эталоны лежат в `tests/golden/refactor/*.txt`. Первый прогон с PostgreSQL
//! создаёт их; после ревью эталоны коммитятся. Перезапись —
//! `LD2_UPDATE_GOLDEN=1 cargo test --test refactor_chronicle`.

mod common;

use std::path::PathBuf;

use common::{in_world, start_ws_server, test_url};
use ld2_test_kit::WsClient;
use serde_json::{json, Value};

/// Последовательность `(ack|evt type, state_version)` — по строке на кадр.
#[derive(Default)]
struct Timeline {
    entries: Vec<String>,
}

impl Timeline {
    fn push_ack(&mut self, command: &str, ack: &Value) {
        self.entries
            .push(format!("ack {command} v={}", version_of(ack)));
    }

    fn push_evt(&mut self, event: &Value) {
        let ty = event["type"].as_str().unwrap_or("?");
        self.entries.push(format!("{ty} v={}", version_of(event)));
    }

    fn render(&self) -> String {
        self.entries.join("\n")
    }

    /// state_version не должен уменьшаться: иначе сломана нумерация §2.3.
    fn assert_monotonic(&self) {
        let mut last = 0u64;
        for entry in &self.entries {
            let raw = entry.rsplit("v=").next().unwrap_or("-");
            if let Ok(version) = raw.parse::<u64>() {
                assert!(
                    version >= last,
                    "state_version уменьшился: {entry} после v={last}\n{}",
                    self.render()
                );
                last = version;
            }
        }
    }
}

fn version_of(env: &Value) -> String {
    match env.get("state_version").and_then(Value::as_u64) {
        Some(v) => v.to_string(),
        None => "-".into(),
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/refactor")
        .join(format!("{name}.txt"))
}

/// Сверка с эталоном; отсутствующий эталон создаётся (нужен PostgreSQL).
fn check_golden(name: &str, timeline: &Timeline) {
    timeline.assert_monotonic();
    let actual = timeline.render();
    let path = golden_path(name);
    let update = std::env::var("LD2_UPDATE_GOLDEN").ok().as_deref() == Some("1");
    if update || !path.exists() {
        let parent = path.parent().expect("каталог эталонов");
        std::fs::create_dir_all(parent).expect("каталог эталонов");
        std::fs::write(&path, format!("{actual}\n")).expect("запись эталона");
        eprintln!(
            "эталон {} записан — закоммитьте его, чтобы хронология стала контрактом",
            path.display()
        );
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("чтение эталона");
    assert_eq!(
        expected.trim(),
        actual.trim(),
        "хронология «{name}» разошлась с эталоном {} \
         (если изменение намеренное: LD2_UPDATE_GOLDEN=1)",
        path.display()
    );
}

/// Отправка команды + запись ack в хронологию.
async fn send_ack(
    client: &mut WsClient,
    timeline: &mut Timeline,
    command: &str,
    payload: Value,
) -> Value {
    let seq = client.send(command, payload).await;
    let ack = client.ack(seq).await;
    timeline.push_ack(command, &ack);
    ack
}

/// Ожидание события + запись в хронологию.
async fn wait_evt(client: &mut WsClient, timeline: &mut Timeline, ty: &str) -> Value {
    let event = client.wait_event(ty).await;
    timeline.push_evt(&event);
    event
}

#[tokio::test]
async fn event_timeline_session() {
    let Some(db_url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_ws_server(&db_url).await;
    let mut client = WsClient::connect(&ws).await;
    let mut timeline = Timeline::default();

    let ack = send_ack(
        &mut client,
        &mut timeline,
        "cmd/auth.register",
        json!({
            "login": ld2_test_kit::checks::unique_login(),
            "password": "longenough1",
            "email": ld2_test_kit::checks::unique_email(),
        }),
    )
    .await;
    assert_eq!(ack["result"], "ok", "регистрация: {ack}");
    wait_evt(&mut client, &mut timeline, "evt/session.start").await;

    let ack = send_ack(
        &mut client,
        &mut timeline,
        "cmd/char.create",
        json!({ "name": unique_name(), "profile": "warrior" }),
    )
    .await;
    assert_eq!(ack["result"], "ok", "персонаж: {ack}");
    let char_id = ack["payload"]["character"]["id"]
        .as_str()
        .expect("id персонажа")
        .to_string();

    let ack = send_ack(
        &mut client,
        &mut timeline,
        "cmd/char.select",
        json!({ "char_id": char_id }),
    )
    .await;
    assert_eq!(ack["result"], "ok", "выбор персонажа: {ack}");
    wait_evt(&mut client, &mut timeline, "evt/state/snapshot").await;

    send_ack(&mut client, &mut timeline, "cmd/char.list", json!({})).await;
    send_ack(&mut client, &mut timeline, "cmd/time.sync", json!({})).await;
    wait_evt(&mut client, &mut timeline, "evt/time.sync").await;

    send_ack(&mut client, &mut timeline, "cmd/ping", json!({})).await;
    wait_evt(&mut client, &mut timeline, "evt/pong").await;

    check_golden("session", &timeline);
}

#[tokio::test]
async fn event_timeline_world_reads() {
    let Some(db_url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_ws_server(&db_url).await;
    let (mut client, _, _) = in_world(&ws).await;
    let mut timeline = Timeline::default();

    send_ack(&mut client, &mut timeline, "cmd/location.look", json!({})).await;
    send_ack(
        &mut client,
        &mut timeline,
        "cmd/map.world",
        json!({ "offset": 0 }),
    )
    .await;
    send_ack(
        &mut client,
        &mut timeline,
        "cmd/state.resync",
        json!({ "last_state_version": 1 }),
    )
    .await;
    wait_evt(&mut client, &mut timeline, "evt/state/snapshot").await;
    send_ack(&mut client, &mut timeline, "cmd/quest.list", json!({})).await;
    send_ack(&mut client, &mut timeline, "cmd/craft.list", json!({})).await;
    send_ack(
        &mut client,
        &mut timeline,
        "cmd/guild.browse",
        json!({ "query": "" }),
    )
    .await;
    send_ack(&mut client, &mut timeline, "cmd/crime.log", json!({})).await;

    check_golden("world_reads", &timeline);
}

#[tokio::test]
async fn event_timeline_chat() {
    let Some(db_url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_ws_server(&db_url).await;
    let (mut client, _, _) = in_world(&ws).await;
    let mut timeline = Timeline::default();

    let text = format!("хронология-{}", unique_name());
    let ack = send_ack(
        &mut client,
        &mut timeline,
        "cmd/chat.send",
        json!({ "channel": "location", "text": text }),
    )
    .await;
    assert_eq!(ack["result"], "ok", "чат: {ack}");
    let event = client
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == text)
        .await;
    timeline.push_evt(&event);

    check_golden("chat", &timeline);
}

fn unique_name() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!(
        "Хроника_{:x}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время")
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    )
}
