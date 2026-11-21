//! M26/F26a: `cmd/map.world` — полнота графа мира, структура выходов,
//! чанки в пределах лимита WS (MAX_MESSAGE_BYTES = 64 КБ: полный
//! граф ~0.5 МБ в одно сообщение не влезает — transport дропает).

mod common;

use std::collections::HashSet;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn map_world_chunks_cover_all_locations() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, _) = in_world(&ws).await;

    // Контент — источник истины для полноты графа.
    let content_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/locations.json");
    let content: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&content_path).expect("чтение content/locations.json"),
    )
    .expect("json контента");
    let expected: HashSet<&str> = content["locations"]
        .as_array()
        .expect("locations")
        .iter()
        .map(|l| l["id"].as_str().expect("id"))
        .collect();

    // Сборка графа чанками: ack {locations, offset, total}.
    let mut all: Vec<serde_json::Value> = Vec::new();
    let mut offset = 0usize;
    let mut chunks = 0usize;
    let mut total: usize;
    loop {
        let seq = a.send("cmd/map.world", json!({ "offset": offset })).await;
        let ack = a.ack(seq).await;
        assert_eq!(ack["result"], "ok", "map.world: {ack}");
        let locs = ack["payload"]["locations"]
            .as_array()
            .expect("locations в ack");
        let next = ack["payload"]["offset"].as_u64().expect("offset") as usize;
        total = ack["payload"]["total"].as_u64().expect("total") as usize;
        assert!(
            next > offset || locs.is_empty(),
            "чанк не продвигается: {offset} → {next}"
        );
        // Каждое сообщение — в пределах лимита WS (больше дропается).
        let bytes = serde_json::to_vec(&ack).expect("encode ack").len();
        assert!(
            bytes <= ld2_protocol::envelope::MAX_MESSAGE_BYTES,
            "чанк {bytes} > лимита {}",
            ld2_protocol::envelope::MAX_MESSAGE_BYTES
        );
        all.extend(locs.iter().cloned());
        chunks += 1;
        offset = next;
        if offset >= total {
            break;
        }
        assert!(chunks < 1000, "слишком много чанков");
    }
    assert!(chunks > 1, "граф должен чанковаться (несколько сообщений)");

    // Полнота: каждая локация контента — в графе.
    let ids: HashSet<&str> = all.iter().map(|l| l["id"].as_str().expect("id")).collect();
    assert_eq!(ids.len(), all.len(), "id уникальны");
    assert_eq!(ids, expected, "полнота графа");
    assert!(ids.contains("l_village"), "спавн-локация в графе");

    // Структура: имя/описание непустые, каждый выход ведёт в граф.
    for loc in &all {
        assert!(
            !loc["name"].as_str().unwrap_or_default().is_empty(),
            "имя локации: {loc}"
        );
        assert!(loc["exits"].as_array().is_some(), "exits — массив: {loc}");
        for exit in loc["exits"].as_array().expect("exits") {
            assert!(
                !exit["label"].as_str().unwrap_or_default().is_empty(),
                "метка выхода: {exit}"
            );
            let to = exit["to"].as_str().expect("to");
            assert!(ids.contains(to), "висячий выход {exit} из {}", loc["id"]);
        }
    }

    let total_bytes: usize = all
        .iter()
        .map(|l| serde_json::to_vec(l).map(|v| v.len()).unwrap_or(0))
        .sum();
    eprintln!(
        "карта мира: {} локаций, {chunks} чанков, ~{} КБ графа",
        total,
        total_bytes / 1024
    );
}
