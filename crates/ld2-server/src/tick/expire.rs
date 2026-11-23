//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

use tracing::warn;

use crate::state::SharedState;

use super::plan::event_text;

/// M18/F58d (ADR-012 §6): снятие истёкших баффов рун раз в секунду —
/// M20: респавны по сроку (очередь `World`): моб возвращается из
/// шаблона сида со стартовым HP → `evt/entity/spawn`, его старый труп
/// уходит → `evt/entity/despawn` (kind=corpse); статический лут —
/// сброс `loot_taken` в БД (без события — клиент осматривает заново).
pub(super) async fn respawn_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let due = {
        let mut world = state.world.lock().expect("мир");
        world.due_respawns(now)
    };
    for entry in due {
        match entry.kind {
            crate::world::RespawnKind::Mob { corpse_id } => {
                let (entity, corpse_id) = {
                    let mut world = state.world.lock().expect("мир");
                    let entity = world.respawn_mob(&entry.entity_id);
                    let corpse = corpse_id
                        .as_deref()
                        .and_then(|id| world.remove_corpse(id))
                        .map(|c| c.id);
                    (entity, corpse)
                };
                let Some(entity) = entity else {
                    continue; // нет шаблона (не должен случиться)
                };
                let world = state.world.lock().expect("мир");
                // Сначала старый труп (вместе с лутом), затем моб.
                if let Some(corpse_id) = corpse_id {
                    let text = event_text(
                        "evt/entity/despawn",
                        &ld2_protocol::evt::EntityDespawn {
                            entity_id: corpse_id,
                            location_id: entry.location_id.clone(),
                            kind: Some(ld2_protocol::types::EntityKind::Corpse),
                            picked_by: None,
                            picked_gender: None,
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                }
                let text = event_text(
                    "evt/entity/spawn",
                    &ld2_protocol::evt::EntitySpawn {
                        entity,
                        location_id: entry.location_id.clone(),
                    },
                );
                for addr in world.chars_in_location(&entry.location_id) {
                    world.send_to_char(&addr, text.clone());
                }
            }
            crate::world::RespawnKind::Loot => {
                if let Err(e) =
                    crate::db::items::loot_taken_reset(&state.pool, &entry.entity_id).await
                {
                    warn!("респавн лута ({}): {e}", entry.entity_id);
                }
            }
            crate::world::RespawnKind::Resource => {
                let restored = {
                    let mut world = state.world.lock().expect("мир");
                    world.respawn_resource(&entry.entity_id)
                };
                let Some((node, was_hidden)) = restored else {
                    continue; // нет узла (не должен случиться)
                };
                let world = state.world.lock().expect("мир");
                if was_hidden {
                    // Трава исчезала → возвращается видимой.
                    let text = event_text(
                        "evt/entity/spawn",
                        &ld2_protocol::evt::EntitySpawn {
                            entity: node.entity(),
                            location_id: entry.location_id.clone(),
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                } else {
                    // Жилы/деревья остаются — только патч шкалы запаса.
                    let mut patch = serde_json::Map::new();
                    patch.insert(
                        "hp".into(),
                        serde_json::json!({"cur": node.current, "max": node.max}),
                    );
                    let text = event_text(
                        "evt/entity/move",
                        &ld2_protocol::evt::EntityMove {
                            entity_id: node.id.clone(),
                            patch,
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                }
            }
        }
    }
}

/// M20: выброшенные предметы истекают по `drop_ttl_s` → «{name} исчез»
/// всем в локации (`ttl <= 0` — выключено).
pub(super) async fn ground_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let ttl = state.config.world.drop_ttl_s;
    if ttl <= 0 {
        return;
    }
    let expired: Vec<(String, ld2_protocol::evt::ItemExpired)> = {
        let mut world = state.world.lock().expect("мир");
        world
            .due_ground(now, ttl)
            .into_iter()
            .map(|g| {
                (
                    g.location_id.clone(),
                    ld2_protocol::evt::ItemExpired {
                        uid: g.id,
                        name: g.name,
                    },
                )
            })
            .collect()
    };
    for (loc, payload) in expired {
        let text = event_text("evt/item/expired", &payload);
        let world = state.world.lock().expect("мир");
        for addr in world.chars_in_location(&loc) {
            world.send_to_char(&addr, text.clone());
        }
    }
}

/// M20: трупы старше `corpse_ttl_s` исчезают (вместе с лутом) —
/// `evt/entity/despawn` (kind=corpse → «{name} исчезает»).
pub(super) async fn corpse_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let ttl = state.config.world.corpse_ttl_s;
    if ttl <= 0 {
        return;
    }
    let expired: Vec<(String, String)> = {
        let mut world = state.world.lock().expect("мир");
        world
            .due_corpses(now, ttl)
            .into_iter()
            .map(|c| (c.location_id.clone(), c.id))
            .collect()
    };
    for (loc, corpse_id) in expired {
        let payload = ld2_protocol::evt::EntityDespawn {
            entity_id: corpse_id,
            location_id: loc.clone(),
            kind: Some(ld2_protocol::types::EntityKind::Corpse),
            picked_by: None,
            picked_gender: None,
        };
        let text = event_text("evt/entity/despawn", &payload);
        let world = state.world.lock().expect("мир");
        for addr in world.chars_in_location(&loc) {
            world.send_to_char(&addr, text.clone());
        }
    }
}

/// клиенту шлётся **полная замена** списка `evt/char/buffs` (паттерн
/// `evt/crime/status`); только онлайн-персонажи (как `regen_step`).
pub(super) async fn buff_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    for char_id in online {
        let expired = match crate::db::buffs::expire(&state.pool, &char_id, now).await {
            Ok(kinds) => kinds,
            Err(e) => {
                warn!("баффы (истечение {char_id}): {e}");
                continue;
            }
        };
        if expired.is_empty() {
            continue;
        }
        // Полная замена: оставшиеся активные баффы.
        let rest = match crate::db::buffs::active(&state.pool, &char_id, now).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("баффы (список {char_id}): {e}");
                continue;
            }
        };
        let buffs: Vec<ld2_protocol::types::BuffState> = rest
            .iter()
            .map(
                |(kind, title, stacks, expires_at, ..)| ld2_protocol::types::BuffState {
                    kind: kind.clone(),
                    title: title.clone(),
                    stacks: (*stacks).max(0) as u32,
                    expires_at: *expires_at,
                    extra: Default::default(),
                },
            )
            .collect();
        let text = crate::session::event_text(
            "evt/char/buffs",
            None,
            serde_json::to_value(ld2_protocol::evt::CharBuffs { buffs }).unwrap_or_default(),
        );
        if text.is_empty() {
            continue;
        }
        let world = state.world.lock().expect("мир");
        world.send_to_char(&char_id, text);
    }
}

/// Истечение статусов преступления раз в секунду (ФР-11, §10): снятие
/// просроченных → `evt/crime/removed` адресату (подстраховка клиентского
/// fallback по `expires_at`).
pub(super) async fn crime_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    for char_id in online {
        let expired = match crate::db::crime::expire(&state.pool, &char_id, now).await {
            Ok(kinds) => kinds,
            Err(e) => {
                warn!("crime (истечение {char_id}): {e}");
                continue;
            }
        };
        for kind in expired {
            let text = crate::session::event_text(
                "evt/crime/removed",
                None,
                serde_json::json!({ "kind": kind }),
            );
            if text.is_empty() {
                continue;
            }
            let world = state.world.lock().expect("мир");
            world.send_to_char(&char_id, text);
        }
    }
}
