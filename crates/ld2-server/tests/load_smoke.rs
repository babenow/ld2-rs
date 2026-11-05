//! Нагрузочный смоук (M12.8, НФР §8): burst ≥200 событий/с **без потери
//! порядка** — ack по seq идут строго по порядку отправки, события не
//! теряются и не перемешиваются (§2.4.3: порядок внутри соединения).

mod common;

use common::in_world;
use serde_json::json;
use std::time::Duration;

/// Burst из `N` пингов без ожидания ack → строгий порядок ответов:
/// ack(seq=i) → evt/pong, для i = 1..=N (ровно один ack на команду).
#[tokio::test]
async fn burst_ping_keeps_ack_and_event_order() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // server_history = 0 → без реплея истории при select (очистка очереди
    // для строгой проверки порядка ack/событий).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.chat.server_history = 0;
        })
        .await
    );
    let (mut c, _, _) = in_world(&ws).await;

    const N: u32 = 300; // 300 команд подряд ≫ 200/с burst
    let first_seq = c.current_seq() + 1;
    for _ in 0..N {
        c.send("cmd/ping", json!({})).await;
    }

    // Чтение N пар (ack, pong) строго по порядку; общий дедлайн.
    let read = async {
        for i in 0..N {
            let ack = c.recv_json().await;
            assert_eq!(ack["type"], "ack", "ack #{i}: ожидался ack, получил {ack}");
            assert_eq!(
                ack["seq"],
                first_seq + i,
                "порядок ack по seq (§2.4.3): {ack}"
            );
            assert_eq!(ack["result"], "ok", "ack #{i}: {ack}");
            let evt = c.recv_json().await;
            assert_eq!(
                evt["type"], "evt/pong",
                "событие #{i}: ожидался pong, получил {evt}"
            );
        }
    };
    tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .expect("чтение burst-ответов (порядок ack/событий сохранён)");
}

/// События рассылки (чат в location) не теряются и приходят в порядке
/// отправки при непрерывном потоке (кулдаун отключён конфигом).
#[tokio::test]
async fn burst_chat_messages_keep_order_for_observers() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // cooldown 0 → непрерывный поток (в проде 1.5 с — тестовый прогон).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.chat.cooldown_s = 0.0;
            cfg.gameplay.chat.server_history = 0; // без реплея в очередь
        })
        .await
    );
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, _) = in_world(&ws).await; // наблюдатель в той же локации

    const N: u32 = 150;
    for i in 0..N {
        let seq = a
            .send(
                "cmd/chat.send",
                json!({ "channel": "location", "text": format!("burst {i}") }),
            )
            .await;
        // ack отправителя — по порядку seq (свой порядок чтения ниже).
        let ack = a.ack(seq).await;
        assert_eq!(ack["result"], "ok", "send #{i}: {ack}");
    }

    // Наблюдатель получил все N сообщений строго в порядке (§2.4.3).
    let read = async {
        for i in 0..N {
            let msg = b.recv_json().await;
            assert_eq!(msg["type"], "evt/chat/message", "событие #{i}: {msg}");
            let text = msg["payload"]["text"].as_str().unwrap_or_default();
            assert_eq!(
                text,
                format!("burst {i}"),
                "порядок сообщений #{i} (стрим без переупорядочивания)"
            );
        }
    };
    tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .expect("наблюдатель получил поток в порядке");
}
