//! RF1b: чистые правила сессии — функции без `Session` и без БД.
//!
//! Сюда переезжает то, что считается «на входе/выходе» протокола:
//! снапшоты предметов и локаций, разбор бонусов, слоты рун, производные
//! поля сводок. Всё покрыто юнит-тестами (RF0c) и вызывается из
//! `session.rs`, `tick.rs` и `death.rs` без изменений поведения.

use ld2_protocol::types::{
    CharacterSummary, Entity, ItemBonus, ItemSnapshot, LocationSnapshot, Socket,
};
use tracing::warn;

use crate::consts::{ITEM_TYPE_GEM, ITEM_TYPE_RUNE};
use crate::db;

/// Превью улучшения (одно на соединение; уровень изменился → stale).
#[derive(Debug, Clone)]
pub(super) struct UpgradePreviewState {
    pub(super) preview_id: String,
    pub(super) item_uid: String,
    /// Уровень на момент превью.
    pub(super) level: i32,
    /// M18/F54: качество активного камня (процентные пункты) — бросок
    /// старта использует то же значение, что и показанное превью.
    pub(super) stone_quality: i32,
    /// M18/F54: uid камня на момент превью — между превью и стартом
    /// камень не должен смениться/исчезнуть.
    pub(super) stone_uid: String,
}
/// Сводка §5 `characters[]`: `profile` — поле `CharacterSummary` (D19,
/// ADR-005 пришло с M11); `location` — **имя** локации из сида (M12.3).
pub(super) fn summary_of(
    row: &db::characters::CharacterRow,
    location_name: String,
) -> CharacterSummary {
    CharacterSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        profile: Some(row.profile.clone()),
        location: location_name,
        summary: None,
        crime: Vec::new(),
        online: false, // реестр online — M12.7 (whisper/группа)
        extra: Default::default(),
    }
}

/// M18/F47: разобрать JSON-массив бонусов (битые записи пропускаются
/// с предупреждением).
fn parse_bonuses(value: &serde_json::Value) -> Vec<ItemBonus> {
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(
            |item| match serde_json::from_value::<ItemBonus>(item.clone()) {
                Ok(bonus) => Some(bonus),
                Err(e) => {
                    warn!("бонус предмета пропущен ({item}): {e}");
                    None
                }
            },
        )
        .collect()
}

/// M18/F47/ADR-010: контент-бонусы определения — поле `bonuses` **плюс**
/// раздел `stats.param` («любой параметр прямо в статах»).
pub(super) fn content_bonuses(def: &db::items::ItemDefRow) -> Vec<ItemBonus> {
    let mut out = parse_bonuses(def.bonuses.as_ref().unwrap_or(&serde_json::Value::Null));
    let Some(params) = def.stats.get("param").and_then(|v| v.as_object()) else {
        return out;
    };
    for (id, value) in params {
        let (value, pct) = match value {
            serde_json::Value::Number(n) => (n.as_i64().unwrap_or(0), false),
            serde_json::Value::Object(map) => (
                map.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
                map.get("pct").and_then(|v| v.as_bool()).unwrap_or(false),
            ),
            _ => continue,
        };
        if value == 0 {
            continue;
        }
        out.push(ItemBonus {
            kind: "param".into(),
            id: id.clone(),
            value,
            pct,
        });
    }
    out
}

/// M18/F58 (ADR-012, уточнение заказчика): сколько слотов предмета
/// принимают руны — **отдельная колонка определения**
/// (`item_defs.rune_sockets`, как `sockets`), а не поле в `stats`;
/// `NULL` — не указано → дефолт по типу предмета сохраняет прежнее
/// поведение: броня → 0 (всё под самоцветы), оружие/посох → все слоты
/// рунные. Общая точка для снапшота и `socket.insert`.
pub(super) fn rune_slots(def: &db::items::ItemDefRow) -> i64 {
    match def.rune_sockets {
        Some(n) => i64::from(n).max(0),
        None => {
            if def.item_type == "armor" {
                0
            } else {
                i64::from(def.sockets)
            }
        }
    }
}

/// M18/F58 (ADR-012): принимает ли слот: его `accepts` либо вычисление
/// по индексу. В сырой записи (`items.sockets`) поля может не быть —
/// ленивая инициализация слотов и перезапись при инкрустации его не
/// ставят; тогда считаем по `rune_slots` (первые N — рунные).
pub(super) fn socket_accepts(slot: &serde_json::Value, index: u32, rune_slots: i64) -> String {
    slot.get("accepts")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if i64::from(index) < rune_slots {
                ITEM_TYPE_RUNE.into()
            } else {
                ITEM_TYPE_GEM.into()
            }
        })
}

/// Проводной вид предмета §6: поля инстанса + определение из сида (D26).
/// `pub(crate)`: тикер зарядов (M18/F58c) шлёт патч сокетов клиенту.
pub(crate) fn item_snapshot(
    item: &db::items::ItemRow,
    def: &db::items::ItemDefRow,
) -> ItemSnapshot {
    ItemSnapshot {
        uid: item.uid.clone(),
        item_type: def.item_type.clone(),
        name: def.name.clone(),
        description: (!def.description.trim().is_empty()).then(|| def.description.clone()),
        personal: item.personal,
        rarity: def.rarity.clone(),
        upgrade: def.upgrade + item.upgrade,
        stats: def.stats.clone().as_object().cloned().unwrap_or_default(),
        requires: def
            .requires
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        sockets: {
            let mut sockets: Vec<Socket> =
                serde_json::from_value(item.sockets.clone()).unwrap_or_default();
            if sockets.is_empty() && def.sockets > 0 {
                // M18/F31: предмет с сокетами показывает их даже до
                // первой вставки — окно инкрустации и список целей
                // видят всё оружие/броню с сокетами (ленивая форма).
                let list: Vec<serde_json::Value> = (0..def.sockets.max(0))
                    .map(|i| serde_json::json!({ "index": i, "rune": null }))
                    .collect();
                sockets =
                    serde_json::from_value(serde_json::Value::Array(list)).unwrap_or_default();
            }
            // M18/F58 (ADR-012): тип слота добираем при отправке —
            // `item_defs.rune_sockets` предмета (NULL = не указано →
            // дефолт по типу: броня → gem, оружие/посох → rune, прежнее
            // поведение). Правится так и raw-запись сокета после
            // инкрустации, где поля нет.
            if !sockets.is_empty() {
                let slots = rune_slots(def);
                for socket in &mut sockets {
                    if socket.accepts.is_none() {
                        socket.accepts = Some(if i64::from(socket.index) < slots {
                            ITEM_TYPE_RUNE.into()
                        } else {
                            ITEM_TYPE_GEM.into()
                        });
                    }
                }
            }
            (!sockets.is_empty()).then_some(sockets)
        },
        qty: Some(i64::from(item.qty)),
        gold_value: Some(i64::from(def.gold_value)),
        // M18/F47: бонусы = контент определения (bonuses + stats.param)
        // + свои бонусы инстанса.
        bonuses: {
            let mut list = content_bonuses(def);
            list.extend(parse_bonuses(
                item.bonuses.as_ref().unwrap_or(&serde_json::Value::Null),
            ));
            (!list.is_empty()).then_some(list)
        },
        // M28: контейнер/позиция для сетки сумок (аддитивно §11).
        container: Some(item.container.clone()),
        slot_index: item.slot_index,
        bag_slots: def.bag_slots.and_then(|v| u32::try_from(v).ok()),
        bag_types: def.bag_types.clone(),
        bag_kind: def.bag_kind.clone(),
        // M17/F30g: определение — ключ кулдауна использования.
        def_id: Some(item.def_id.clone()),
        slot: def.slot.clone(),
        hands: Some(def.hands),
        // M17/F30b: навык оружия в снапшот — включает вклад навыка в
        // производные характеристики (stats.rs, док модуля).
        extra: {
            let mut m = serde_json::Map::new();
            if let Some(skill) = &def.skill {
                m.insert("skill".into(), serde_json::Value::String(skill.clone()));
            }
            m
        },
    }
}

/// Payload рецепта для `cmd/craft.recipes` (skills.md §5.4, открытый §6).
pub(super) fn recipe_payload(r: &db::craft::RecipeRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "name": r.name,
        "profession": r.profession,
        "rank": r.rank,
        "station": r.station,
        "tool": r.tool_type.as_ref().map(|t| serde_json::json!({ "type": t })),
        "requires": r.requires,
        "inputs": r.inputs,
        "time_s": r.time_s,
        "xp": { "profession": i64::from(r.xp_profession) },
        "output": { "type": r.output_type, "qty": i64::from(r.output_qty) },
        "station_label": r.station_label,
    })
}

/// id определений лута сущности (§6 `extra.loot` — открытый объект).
pub(super) fn entity_loot_defs(entity: &Entity) -> Vec<String> {
    entity
        .extra
        .get("loot")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Локация-заглушка: id отсутствует в сиде/БД (D26). pub(crate) —
/// используется death.rs для респаун-событий.
pub(crate) fn stub_location(location_id: &str) -> LocationSnapshot {
    location_snapshot(location_id)
}

/// Локация снапшота: до сида контента (M12.3, D26) — заглушка по id
/// (таблицы locations ещё нет: имя = id, описание/выходы/сущности пустые).
pub(super) fn location_snapshot(location_id: &str) -> LocationSnapshot {
    LocationSnapshot {
        id: location_id.to_string(),
        name: location_id.to_string(),
        flags: Vec::new(),
        text: String::new(),
        exits: Vec::new(),
        entities: Vec::new(),
        extra: Default::default(),
    }
}

/// M17/F30d: родительный падеж основных характеристик для ошибок
/// требований (локализация сервера — русская, §8).
pub(super) fn attr_genitive(id: &str) -> &str {
    match id {
        "str" => "силы",
        "dex" => "ловкости",
        "vit" => "стойкости",
        "int" => "интеллекта",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn item_def(item_type: &str, sockets: i32, rune_sockets: Option<i32>) -> db::items::ItemDefRow {
        db::items::ItemDefRow {
            id: "sword_1".into(),
            item_type: item_type.into(),
            name: "Меч".into(),
            description: String::new(),
            personal: false,
            rarity: "common".into(),
            upgrade: 1,
            stats: json!({
                "dmg": 5,
                "param": { "str": 2, "dex": { "value": 3, "pct": true }, "vit": 0, "int": 0 }
            }),
            requires: Some(json!({ "str": 10 })),
            gold_value: 100,
            qty: 1,
            sockets,
            rune_sockets,
            slot: Some("main_hand".into()),
            hands: 1,
            skill: Some("sword".into()),
            cooldown_s: 1.5,
            bonuses: Some(json!([{ "kind": "attr", "id": "str", "value": 1 }])),
            bag_slots: None,
            bag_types: None,
            bag_kind: None,
        }
    }

    fn item_row(sockets: Value, bonuses: Option<Value>) -> db::items::ItemRow {
        db::items::ItemRow {
            uid: "uid-1".into(),
            def_id: "sword_1".into(),
            qty: 2,
            upgrade: 2,
            personal: false,
            equipped_slot: None,
            sockets,
            bonuses,
            container: "backpack".into(),
            slot_index: None,
        }
    }

    #[test]
    fn attr_genitive_translates_core_stats() {
        assert_eq!(attr_genitive("str"), "силы");
        assert_eq!(attr_genitive("dex"), "ловкости");
        assert_eq!(attr_genitive("vit"), "стойкости");
        assert_eq!(attr_genitive("int"), "интеллекта");
        assert_eq!(attr_genitive("luck"), "luck");
    }

    #[test]
    fn location_snapshot_is_stub_by_id() {
        let loc = location_snapshot("l_forest");
        assert_eq!(loc.id, "l_forest");
        assert_eq!(loc.name, "l_forest");
        assert!(loc.flags.is_empty() && loc.exits.is_empty() && loc.entities.is_empty());
        assert!(loc.text.is_empty());
        assert_eq!(stub_location("l_forest"), loc);
    }

    #[test]
    fn parse_bonuses_skips_broken_entries() {
        let parsed = parse_bonuses(&json!([
            { "kind": "attr", "id": "str", "value": 1 },
            { "kind": 1 },
            { "kind": "param", "id": "dmg", "value": -2, "pct": true }
        ]));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].id, "dmg");
        assert!(parsed[1].pct);
        assert!(parse_bonuses(&json!(null)).is_empty());
        assert!(parse_bonuses(&json!({ "kind": "attr" })).is_empty());
    }

    #[test]
    fn content_bonuses_merge_param_section() {
        let def = item_def("weapon", 0, None);
        let list = content_bonuses(&def);
        // bonuses: 1; stats.param: str 2, dex {3, pct}, vit 0 — пропущен.
        assert_eq!(list.len(), 3);
        let dex = list.iter().find(|b| b.id == "dex").expect("dex");
        assert_eq!(dex.value, 3);
        assert!(dex.pct);
        assert!(list.iter().all(|b| b.id != "vit"));
        assert!(list.iter().all(|b| b.kind == "attr" || b.kind == "param"));
    }

    #[test]
    fn rune_slots_explicit_and_defaults() {
        assert_eq!(rune_slots(&item_def("weapon", 3, Some(1))), 1);
        assert_eq!(rune_slots(&item_def("weapon", 3, Some(-2))), 0);
        assert_eq!(rune_slots(&item_def("armor", 3, None)), 0);
        assert_eq!(rune_slots(&item_def("weapon", 3, None)), 3);
        assert_eq!(rune_slots(&item_def("staff", 2, None)), 2);
    }

    #[test]
    fn socket_accepts_explicit_and_indexed() {
        assert_eq!(
            socket_accepts(&json!({ "index": 0, "accepts": "gem" }), 0, 3),
            "gem"
        );
        assert_eq!(socket_accepts(&json!({ "index": 0 }), 0, 2), "rune");
        assert_eq!(socket_accepts(&json!({ "index": 2 }), 2, 2), "gem");
    }

    /// M39: описание из определения уходит в снапшот; пустое — не шлём.
    #[test]
    fn item_snapshot_carries_description_when_present() {
        let mut def = item_def("weapon", 0, None);
        def.description = "Удваивает шанс камня.".into();
        def.personal = true;
        let mut row = item_row(json!([]), None);
        let snap = item_snapshot(&row, &def);
        assert_eq!(snap.description.as_deref(), Some("Удваивает шанс камня."));
        assert!(
            !snap.personal,
            "до первого надевания печати нет (def.personal — лишь триггер)"
        );
        row.personal = true;
        assert!(item_snapshot(&row, &def).personal, "печать на инстансе");

        def.description = "  ".into();
        let snap = item_snapshot(&item_row(json!([]), None), &def);
        assert_eq!(snap.description, None, "пустое описание не отправляется");
    }

    #[test]
    fn item_snapshot_lazily_initializes_sockets() {
        let snap = item_snapshot(&item_row(json!([]), None), &item_def("weapon", 2, None));
        let sockets = snap.sockets.expect("ленивые сокеты");
        assert_eq!(sockets.len(), 2);
        assert!(sockets.iter().all(|s| s.accepts.as_deref() == Some("rune")));
        assert!(sockets.iter().all(|s| s.rune.is_none()));
    }

    #[test]
    fn item_snapshot_defaults_socket_kinds_by_rune_slots() {
        // Броня: 2 слота, рунных — 1.
        let snap = item_snapshot(&item_row(json!([]), None), &item_def("armor", 2, Some(1)));
        let sockets = snap.sockets.expect("сокеты");
        assert_eq!(sockets[0].accepts.as_deref(), Some("rune"));
        assert_eq!(sockets[1].accepts.as_deref(), Some("gem"));
    }

    #[test]
    fn item_snapshot_keeps_existing_sockets_and_accepts() {
        let row = item_row(
            json!([{ "index": 0 }, { "index": 1, "accepts": "gem" }]),
            None,
        );
        let snap = item_snapshot(&row, &item_def("weapon", 2, None));
        let sockets = snap.sockets.expect("сокеты");
        assert_eq!(sockets[0].accepts.as_deref(), Some("rune"));
        assert_eq!(sockets[1].accepts.as_deref(), Some("gem"));
    }

    #[test]
    fn item_snapshot_maps_container_slot_and_bag_capacity() {
        // M28: контейнер/позиция и ёмкость сумки уходят в снапшот.
        let mut def = item_def("weapon", 0, None);
        def.bag_slots = Some(20);
        let mut row = item_row(json!([]), None);
        row.container = "bags".into();
        row.slot_index = Some(2);
        let snap = item_snapshot(&row, &def);
        assert_eq!(snap.container.as_deref(), Some("bags"));
        assert_eq!(snap.slot_index, Some(2));
        assert_eq!(snap.bag_slots, Some(20));
    }

    #[test]
    fn item_snapshot_without_slots_has_no_sockets() {
        let snap = item_snapshot(&item_row(json!([]), None), &item_def("money", 0, None));
        assert!(snap.sockets.is_none());
    }

    #[test]
    fn item_snapshot_merges_bonuses_and_upgrade() {
        let row = item_row(
            json!([]),
            Some(json!([{ "kind": "skill", "id": "sword", "value": 1 }])),
        );
        let snap = item_snapshot(&row, &item_def("weapon", 0, None));
        // upgrade: def 1 + item 2.
        assert_eq!(snap.upgrade, 3);
        assert_eq!(snap.qty, Some(2));
        assert_eq!(snap.gold_value, Some(100));
        assert_eq!(snap.def_id.as_deref(), Some("sword_1"));
        assert_eq!(snap.slot.as_deref(), Some("main_hand"));
        assert_eq!(snap.hands, Some(1));
        assert_eq!(snap.stats.get("dmg"), Some(&json!(5)));
        assert_eq!(
            snap.extra.get("skill").and_then(|v| v.as_str()),
            Some("sword")
        );
        let bonuses = snap.bonuses.expect("бонусы");
        // Контент: attr str + param str + param dex; инстанс: skill sword.
        assert_eq!(bonuses.len(), 4);
        assert!(bonuses.iter().any(|b| b.kind == "skill" && b.id == "sword"));
        assert_eq!(bonuses.iter().filter(|b| b.id == "str").count(), 2);
        assert!(snap.requires.is_some());
    }

    #[test]
    fn item_snapshot_without_bonuses_is_none() {
        let mut def = item_def("weapon", 0, None);
        def.bonuses = None;
        def.stats = json!({ "dmg": 1 });
        let snap = item_snapshot(&item_row(json!([]), None), &def);
        assert!(snap.bonuses.is_none());
        assert_eq!(snap.sockets, None);
    }

    #[test]
    fn recipe_payload_maps_all_fields() {
        let row = db::craft::RecipeRow {
            id: "r_1".into(),
            name: "Клинок".into(),
            profession: "blacksmith".into(),
            rank: 2,
            station: Some("forge".into()),
            tool_type: Some("hammer".into()),
            requires: json!({ "blacksmith": 2 }),
            inputs: json!([{ "id": "iron", "qty": 3 }]),
            time_s: 10,
            xp_profession: 5,
            output_type: "weapon".into(),
            output_qty: 1,
            station_label: Some("Кузница".into()),
        };
        let payload = recipe_payload(&row);
        assert_eq!(payload["id"], "r_1");
        assert_eq!(payload["tool"], json!({ "type": "hammer" }));
        assert_eq!(payload["inputs"][0]["qty"], 3);
        assert_eq!(payload["xp"]["profession"], 5);
        assert_eq!(payload["output"], json!({ "type": "weapon", "qty": 1 }));
        assert_eq!(payload["station_label"], "Кузница");

        let mut bare = row;
        bare.tool_type = None;
        bare.station = None;
        let payload = recipe_payload(&bare);
        assert_eq!(payload["tool"], Value::Null);
        assert_eq!(payload["station"], Value::Null);
    }

    #[test]
    fn entity_loot_defs_reads_extra_and_filters_non_strings() {
        let entity: Entity = serde_json::from_value(json!({
            "id": "mob_1",
            "kind": "monster",
            "name": "Волк",
            "loot": ["fur", 3, "meat"]
        }))
        .expect("entity");
        assert_eq!(entity_loot_defs(&entity), vec!["fur", "meat"]);

        let no_loot: Entity =
            serde_json::from_value(json!({ "id": "mob_2", "kind": "monster", "name": "Заяц" }))
                .expect("entity");
        assert!(entity_loot_defs(&no_loot).is_empty());
    }

    #[test]
    fn summary_of_maps_row_and_location() {
        let row = db::characters::CharacterRow {
            id: "c_1".into(),
            name: "Странник".into(),
            profile: "warrior".into(),
            gender: "male".into(),
            location_id: "l_village".into(),
            hp_cur: 10,
            hp_max: 10,
            mp_cur: 0,
            mp_max: 0,
            xp_pool: 0,
            total_xp: 0,
            state_version: 1,
            attrs: json!({ "str": 10 }),
        };
        let summary = summary_of(&row, "Деревня".into());
        assert_eq!(summary.id, "c_1");
        assert_eq!(summary.profile.as_deref(), Some("warrior"));
        assert_eq!(summary.location, "Деревня");
        assert!(!summary.online);
        assert!(summary.crime.is_empty());
    }
}
