//! M27: hot reload контента (фаза 1) на живом сервере с PostgreSQL.
//!
//! Гейт: `LD2_TEST_DATABASE_URL` (как остальные e2e) **и**
//! `LD2_TEST_CONTENT_RELOAD=1` — тест меняет `meta.content_version`
//! глобальной тестовой БД, поэтому запускается отдельно от общего прогона
//! (иначе параллельные тесты пересеивают контент друг друга).

mod common;

use std::path::{Path, PathBuf};

use serde_json::json;

/// Уникальный временный каталог с копией контента репозитория.
fn temp_content() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ld2-content-reload-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("временный каталог");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    for name in ld2_server::content::CONTENT_FILES {
        std::fs::copy(src.join(name), dir.join(name)).expect("копия контента");
    }
    dir
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("файл контента")).expect("JSON")
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).expect("сериализация"),
    )
    .expect("запись контента");
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_applies_content_and_invalidates_caches() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    if std::env::var_os("LD2_TEST_CONTENT_RELOAD").is_none() {
        eprintln!("пропуск: LD2_TEST_CONTENT_RELOAD не задан (тест меняет версию контента БД)");
        return;
    }

    let dir = temp_content();
    let server = common::start_server_guarded(&db_url, |cfg| {
        cfg.world.content_dir = dir.clone();
        cfg.world.content_poll_s = 0; // триггер — вручную, без фонового poll
    })
    .await;
    let state = server.state();

    // Кэш карты «прогрет» — hot reload обязан его сбросить.
    let map = ld2_server::map::MapCache::build(&state.pool)
        .await
        .expect("карта мира");
    *state.map_cache.write().await = Some(std::sync::Arc::new(map));

    let before = state.content().version.clone();

    // Правка контента: новый предмет + наценка магазина.
    let items_path = dir.join("items.json");
    let mut items = read_json(&items_path);
    items["items"].as_array_mut().expect("items[]").push(json!({
        "id": "reload_probe",
        "type": "material",
        "name": "Проба перезагрузки",
        "rarity": "common",
        "upgrade": 0,
        "gold_value": 3
    }));
    write_json(&items_path, &items);

    let shops_path = dir.join("shops.json");
    let mut shops = read_json(&shops_path);
    let npc = shops["npcs"]
        .as_array_mut()
        .expect("npcs[]")
        .iter_mut()
        .find(|npc| npc["id"] == json!("npc_elder"))
        .expect("npc_elder в shops.json");
    npc["markup"] = json!(2.5);
    write_json(&shops_path, &shops);

    let outcome = ld2_server::reload::reload_state(state)
        .await
        .expect("hot reload контента");
    assert!(outcome.applied, "новая версия контента должна примениться");
    assert_ne!(outcome.version, before, "версия контента изменилась");

    // 1) БД: новый предмет засижен (без рестарта сервера).
    let def = ld2_server::db::items::def(&state.pool, "reload_probe")
        .await
        .expect("запрос item_defs");
    assert!(def.is_some(), "новый предмет в БД");

    // 2) Память: магазины перечитаны из файла, версия подменена.
    let content = state.content();
    assert!((content.shops["npc_elder"].markup - 2.5).abs() < f64::EPSILON);
    assert_eq!(content.version, outcome.version);

    // 3) Кэш карты сброшен (построится заново по новому контенту).
    assert!(state.map_cache.read().await.is_none(), "кэш карты сброшен");

    // 4) Битый контент не применяется: старое состояние сохраняется.
    let mut broken = read_json(&items_path);
    let first = broken["items"][0].clone();
    broken["items"].as_array_mut().expect("items[]").push(first);
    write_json(&items_path, &broken);

    let err = ld2_server::reload::reload_state(state).await;
    assert!(err.is_err(), "дубликат id отклоняется валидацией");
    assert_eq!(
        state.content().version,
        outcome.version,
        "после ошибки версия в памяти прежняя"
    );

    server.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
