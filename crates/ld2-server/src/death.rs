//! Общий поток смерти персонажа (ФР-06): теряется **только лут** — опыт
//! навыков и XP-пул сохраняются (skills.md §2.2); респаун в спавне.
//!
//! Используется и dev-сценарием `/scenario death` (M12.4), и тик-лупом,
//! когда персонажа убивает моб (M12.5). Возвращает события, адресованные
//! соединению погибшего (адресат — char_id).

use ld2_protocol::evt::ItemRemovalReason;
use ld2_protocol::evt::{CharDeath, CharStats, ItemRemoved, LocationChanged, LostItem, Respawn};
use serde_json::json;
use tracing::warn;

use crate::state::SharedState;

/// Смерть персонажа → `(char_id, json-конверт)` для каждого события
/// (порядок: ack-сценария шлёт вызывающий; здесь — только события).
pub async fn kill_character(state: &SharedState, char_id: &str) -> Vec<(String, String)> {
    let pool = &state.pool;

    // Весь лут теряется (ФР-06); надетые сумки (M28) остаются —
    // теряется только их содержимое. Имена — для lost_items.
    let lost_rows = match crate::db::items::delete_all(pool, char_id).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("смерть (инвентарь {char_id}): {e}");
            return vec![];
        }
    };
    let mut lost_items = Vec::with_capacity(lost_rows.len());
    for row in &lost_rows {
        let (name, rarity) = match crate::db::items::def(pool, &row.def_id).await {
            Ok(Some(def)) => (def.name, Some(def.rarity)),
            Ok(None) => (row.def_id.clone(), None),
            Err(e) => {
                warn!("смерть (определение {}): {e}", row.def_id);
                (row.def_id.clone(), None)
            }
        };
        lost_items.push(LostItem {
            uid: row.uid.clone(),
            name,
            rarity,
            extra: Default::default(),
        });
    }

    // Респаун: hp полное + позиция = спавн (иначе текущая) + бамп (D27).
    // M25: настройка сервера `[world] respawn_location_id` — точка
    // респавна после смерти (пусто — спавн-локация контента).
    let override_id = state
        .config
        .world
        .respawn_location_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let spawn = match override_id {
        Some(id) => id,
        None => crate::db::locations::spawn_id(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default(),
    };
    let current_row = crate::db::characters::get(pool, char_id)
        .await
        .ok()
        .flatten();
    let current = current_row
        .as_ref()
        .map(|r| r.location_id.clone())
        .unwrap_or_default();
    let (char_name, char_gender) = current_row
        .map(|r| (r.name, r.gender))
        .unwrap_or_else(|| (String::new(), "male".to_string()));
    let target = if spawn.is_empty() {
        current.clone()
    } else {
        spawn
    };
    let Some((hp_cur, hp_max, sv)) =
        (match crate::db::characters::respawn(pool, char_id, &target).await {
            Ok(row) => row,
            Err(e) => {
                warn!("смерть (респаун {char_id}): {e}");
                return vec![];
            }
        })
    else {
        warn!("смерть: персонаж {char_id} не найден");
        return vec![];
    };
    let state_version = sv as u64;
    let addr = char_id.to_string();

    // Локация спавна (снимок §6).
    let mut location = match crate::db::locations::get(pool, &target).await {
        Ok(Some(row)) => {
            // Оверлей живого мира (M20): ресурсы/трупы/наземные — из
            // памяти, контент-строка только как сид.
            let world = state.world.lock().expect("мир");
            world.build_location(row.snapshot())
        }
        _ => crate::session::stub_location(&target),
    };
    // Присутствие: погибший не должен видеть себя в «Игроках» своей же
    // локации (соседи увидят его по событиям ниже).
    location
        .entities
        .retain(|e| !(e.kind == ld2_protocol::types::EntityKind::Player && e.id == char_id));
    // Присутствие (M29, заказчик): гибель видят ВСЕ соседи — строка
    // «{имя} погиб/погибла» в despawn. Пара despawn+spawn идёт всегда:
    // при респауне в другой локации — расход/приход, в той же — уход и
    // мгновенное возвращение (клиент корректно обновляет «Игроков»).
    if !current.is_empty() {
        let died = crate::session::presence::died_line(&char_name, &char_gender);
        crate::session::presence::announce_despawn(state, &current, char_id, Some(died));
        crate::session::presence::announce_spawn(
            state,
            &target,
            crate::session::presence::player_entity(char_id, &char_name),
            None,
        );
    }

    let death = CharDeath {
        victim_char: true,
        lost_items,
        corpse: format!("cor_{char_id}"),
        respawn: Respawn {
            location_id: target,
            delay_s: 0,
            extra: Default::default(),
        },
        extra: Default::default(),
    };

    let mut events = vec![(
        addr.clone(),
        crate::session::event_text(
            "evt/char/death",
            Some(state_version),
            serde_json::to_value(&death).unwrap_or_default(),
        ),
    )];
    for item in &death.lost_items {
        events.push((
            addr.clone(),
            crate::session::event_text(
                "evt/item/removed",
                Some(state_version),
                serde_json::to_value(ItemRemoved {
                    uid: item.uid.clone(),
                    reason: ItemRemovalReason::LostOnDeath,
                })
                .unwrap_or_default(),
            ),
        ));
    }
    let mut patch = serde_json::Map::new();
    patch.insert("hp".into(), json!({ "cur": hp_cur, "max": hp_max }));
    events.push((
        addr.clone(),
        crate::session::event_text(
            "evt/char/stats",
            Some(state_version),
            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
        ),
    ));
    events.push((
        addr,
        crate::session::event_text(
            "evt/location/changed",
            Some(state_version),
            serde_json::to_value(LocationChanged { location }).unwrap_or_default(),
        ),
    ));
    events
}
