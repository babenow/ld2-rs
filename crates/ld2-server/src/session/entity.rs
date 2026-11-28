//! RF1f: сущности мира (ФР-03) — осмотр статической сущности
//! (`entity.info`), подбор наземного предмета (M20/F20c), спавн-патч
//! наземного предмета и broadcast трупа (M24).
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.
use super::rules::{entity_loot_defs, item_snapshot};
use super::{event_payload_text, event_text, out_event, Out, Session};
use crate::consts::GENDER_MALE;
use crate::db;
use ld2_protocol::cmd::EntityInfo;
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{EntityDespawn, ItemAdded, ItemState};
use ld2_protocol::names;
use ld2_protocol::types::Entity;
use ld2_protocol::ErrorCode;
use tracing::warn;

impl Session {
    /// `cmd/entity.info` (§7): осмотр статической сущности текущей локации
    /// вместе с её лутом (однократное взятие, M12.4); динамические трупы —
    /// M12.5.
    pub(super) async fn entity_info(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<EntityInfo>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Динамический труп (M12.5): живёт в мире, не в строке локации.
        let dyn_loot = {
            let world = self.state.world.lock().expect("мир");
            world.dynamic_corpse(&cmd.entity_id).map(|c| {
                (
                    c.id.clone(),
                    c.name.clone(),
                    c.loot.clone(),
                    c.inner_loot.clone(),
                    c.entity().extra,
                )
            })
        };
        if let Some((corpse_id, corpse_name, slots, inner_slots, extra)) = dyn_loot {
            let mut loot = Vec::new();
            let mut remaining = 0usize;
            for (i, def_id) in slots.iter().enumerate() {
                let Some(def_id) = def_id else {
                    continue; // слот уже взят
                };
                remaining += 1;
                let loot_uid = format!("loot:{corpse_id}:{i}");
                let def = match db::items::def(&self.state.pool, def_id).await {
                    Ok(Some(def)) => def,
                    Ok(None) => {
                        warn!("лут трупа {corpse_id}: нет определения {def_id}");
                        continue;
                    }
                    Err(e) => {
                        warn!("entity.info (определение {def_id}): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let row = db::items::ItemRow {
                    uid: loot_uid,
                    def_id: def_id.clone(),
                    qty: def.qty,
                    upgrade: 0,
                    personal: false,
                    equipped_slot: None,
                    sockets: serde_json::json!([]),
                    container: "backpack".into(),
                    slot_index: None,
                    // M18/F47: лут несёт контент-бонусы (ADR-010).
                    bonuses: def.bonuses.clone(),
                };
                loot.push(item_snapshot(&row, &def));
            }
            // Снятие шкур: «Внутри» — отдельный блок (uid inner:<corpse>:<idx>).
            let mut inner = Vec::new();
            let mut inner_remaining = 0usize;
            for (i, def_id) in inner_slots.iter().enumerate() {
                let Some(def_id) = def_id else {
                    continue;
                };
                inner_remaining += 1;
                let uid = format!("inner:{corpse_id}:{i}");
                let def = match db::items::def(&self.state.pool, def_id).await {
                    Ok(Some(def)) => def,
                    Ok(None) => {
                        warn!("лут трупа {corpse_id}: нет определения {def_id}");
                        continue;
                    }
                    Err(e) => {
                        warn!("entity.info (определение {def_id}): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let row = db::items::ItemRow {
                    uid,
                    def_id: def_id.clone(),
                    qty: def.qty,
                    upgrade: 0,
                    personal: false,
                    equipped_slot: None,
                    sockets: serde_json::json!([]),
                    container: "backpack".into(),
                    slot_index: None,
                    bonuses: def.bonuses.clone(),
                };
                inner.push(item_snapshot(&row, &def));
            }
            let entity = Entity {
                id: corpse_id,
                kind: ld2_protocol::types::EntityKind::Corpse,
                name: corpse_name,
                hp: None,
                tags: None,
                was_player: None,
                aggro: None,
                boss: None,
                next_attack_at: None,
                attack_interval: None,
                attacking: None,
                extra,
            };
            let looted = (slots.is_empty() || remaining == 0)
                && (inner_slots.is_empty() || inner_remaining == 0);
            return vec![Self::ack_payload(
                seq,
                &serde_json::json!({
                    "entity": entity,
                    "loot": loot,
                    "inner": inner,
                    "was_player": false,
                    "looted": looted,
                }),
            )];
        }
        // Статическая сущность из сида (D26) вместе с её лутом (однократно).
        let current = self.location_id.clone().unwrap_or_default();
        let entities = match db::locations::get(&self.state.pool, &current).await {
            Ok(Some(row)) => {
                serde_json::from_value::<Vec<Entity>>(row.entities.clone()).unwrap_or_default()
            }
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!("entity.info (чтение {current}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(entity) = entities.into_iter().find(|e| e.id == cmd.entity_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "сущность не найдена")];
        };
        let defs = entity_loot_defs(&entity);
        // M24: снимки надстроек/удалений сундука — под блокировкой до
        // async-запросов (in-memory оверлей админ-панели).
        let removed: std::collections::HashSet<usize> = {
            let world = self.state.world.lock().expect("мир");
            (0..defs.len())
                .filter(|i| world.container_base_removed(&entity.id, *i))
                .collect()
        };
        let extras: Vec<Option<(String, i64)>> = {
            let world = self.state.world.lock().expect("мир");
            world.container_extra(&entity.id)
        };
        let was_player = entity.was_player.unwrap_or(false);
        let mut loot = Vec::new();
        let mut remaining = 0usize;
        for (i, def_id) in defs.iter().enumerate() {
            // M24: слот удалён админом — не показывается (глобально).
            if removed.contains(&i) {
                continue;
            }
            let loot_uid = format!("loot:{}:{}", entity.id, i);
            match db::items::loot_taken_exists(&self.state.pool, &loot_uid, &char_id).await {
                Ok(true) => continue,
                Ok(false) => remaining += 1,
                Err(e) => {
                    warn!("entity.info (лут {loot_uid}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            let def = match db::items::def(&self.state.pool, def_id).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    warn!("лут ссылается на неизвестное определение {def_id}");
                    continue;
                }
                Err(e) => {
                    warn!("entity.info (определение {def_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let row = db::items::ItemRow {
                uid: loot_uid,
                def_id: def_id.clone(),
                qty: def.qty,
                upgrade: 0,
                personal: false,
                equipped_slot: None,
                sockets: serde_json::json!([]),
                container: "backpack".into(),
                slot_index: None,
                // M18/F47: лут несёт контент-бонусы (ADR-010).
                bonuses: def.bonuses.clone(),
            };
            loot.push(item_snapshot(&row, &def));
        }
        // M24: надстройки сундука — uid продолжает индексацию базовых defs.
        let base_len = defs.len();
        let mut extras_remaining = 0usize;
        for (n, slot) in extras.iter().enumerate() {
            let Some((def_id, qty)) = slot else {
                continue; // слот взят/удалён
            };
            extras_remaining += 1;
            let loot_uid = format!("loot:{}:{}", entity.id, base_len + n);
            let def = match db::items::def(&self.state.pool, def_id).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    warn!("надстройка сундука ссылается на неизвестное {def_id}");
                    continue;
                }
                Err(e) => {
                    warn!("entity.info (надстройка {def_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let row = db::items::ItemRow {
                uid: loot_uid,
                def_id: def_id.clone(),
                // M24: количество стака админа, не из определения.
                qty: i32::try_from(*qty).unwrap_or(i32::MAX),
                upgrade: 0,
                personal: false,
                equipped_slot: None,
                sockets: serde_json::json!([]),
                container: "backpack".into(),
                slot_index: None,
                bonuses: def.bonuses.clone(),
            };
            loot.push(item_snapshot(&row, &def));
        }
        let looted =
            remaining == 0 && extras_remaining == 0 && (!defs.is_empty() || !extras.is_empty());
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "entity": entity,
                "loot": loot,
                "was_player": was_player,
                "looted": looted,
            }),
        )]
    }

    /// R-dnd: Entity наземного предмета для evt/entity/spawn
    /// (паритет полей World::build_location — extra.ground/def_id/qty).
    pub(super) fn ground_entity_for(
        &self,
        _location_id: &str,
        ground_id: &str,
    ) -> Option<ld2_protocol::types::Entity> {
        let world = self.state.world.lock().expect("мир");
        // R-dnd: единый вид с build_location (extra.upgrade + item —
        // снапшот предмета для отображения «как в рюкзаке»).
        world
            .ground_item(ground_id)
            .map(crate::world::GroundItem::entity)
    }

    /// `cmd/item.drop` (§7): полный выброс → `evt/item/removed`
    /// (reason=dropped), частичный → `evt/item/state` с новым qty.
    /// M20/F20c: подобрать наземный предмет — место в инвентаре
    /// проверяется до изъятия (паритет `take_loot`), изъятие из мира
    /// атомарно (гонка двух подбирающих → `E_ITEM_GONE`).
    pub(super) async fn ground_pickup(
        &mut self,
        seq: u32,
        char_id: &str,
        ground: crate::world::GroundItem,
    ) -> Vec<Out> {
        let current = self.current_location(char_id).await;
        if ground.location_id != current {
            return vec![Self::err(seq, ErrorCode::TargetGone, "предмета нет")];
        }
        let pool = &self.state.pool;
        let Some(def) = (match db::items::def(pool, &ground.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("подбор (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения")];
        };
        let slots = self.state.config.gameplay.inventory.slots;
        // R-dnd: предмет с состоянием (заточка/камни/бонусы) занимает
        // отдельную строку — место проверяем строго (в «простой» стак
        // не складывается, иначе камни теряются — заказчик).
        let stateful = ground.stateful();
        let fits = if stateful {
            match db::items::has_space(pool, char_id, i64::from(slots)).await {
                Ok(fits) => fits,
                Err(e) => {
                    warn!("подбор (слоты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        } else {
            match Self::take_loot_fits(pool, char_id, &ground.def_id, slots).await {
                Ok(fits) => fits,
                Err(e) => {
                    warn!("подбор (слоты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        };
        if !fits {
            return vec![Self::err(
                seq,
                ErrorCode::InventoryFull,
                "инвентарь заполнен",
            )];
        }
        let taken = {
            let mut world = self.state.world.lock().expect("мир");
            world.take_ground(&ground.id)
        };
        let Some(taken) = taken else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "уже подобрано")];
        };
        let (row, stacked) = if stateful {
            // Восстановление состояния — отдельная запись (M18: в
            // «простой» стак не складывается).
            match db::items::insert_stateful(
                pool,
                char_id,
                &taken.def_id,
                taken.qty,
                i32::try_from(taken.upgrade).unwrap_or(i32::MAX),
                &taken.sockets,
                taken.bonuses.clone(),
            )
            .await
            {
                Ok(r) => (r, false),
                Err(e) => {
                    warn!("подбор (восстановление): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        } else {
            match db::items::grant_stacking(pool, char_id, &taken.def_id, taken.qty).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("подбор (выдача): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        };
        // Снять с земли — всем в локации (объект исчезает).
        // R-dnd: подбор — в despawn имя/пол подобрал(а) для журнала
        // («Вы подняли…» / «{имя} подобрал(а)…»).
        let (picked_name, picked_gender) = self
            .char
            .as_ref()
            .map(|c| (c.name.clone(), c.gender.clone()))
            .unwrap_or_else(|| ("Игрок".into(), GENDER_MALE.into()));
        let despawn_text = event_payload_text(
            names::EVT_ENTITY_DESPAWN,
            &EntityDespawn {
                entity_id: taken.id.clone(),
                location_id: taken.location_id.clone(),
                kind: Some(ld2_protocol::types::EntityKind::Object),
                picked_by: Some(picked_name),
                picked_gender: Some(picked_gender),
                text: None,
            },
        );
        {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&taken.location_id) {
                if let Some(text) = &despawn_text {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }
        // Выдача себе — стак → патч qty, иначе added (паритет take_loot).
        let mut item = item_snapshot(&row, &def);
        let ev = if stacked {
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(row.qty));
            out_event(
                names::EVT_ITEM_STATE,
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        } else {
            // Унификация журнала (заказчик): подбор — строку даёт
            // despawn («Вы подняли…»); added с маркером picked стор
            // не дублирует.
            item.extra.insert("picked".into(), serde_json::json!(true));
            out_event(
                names::EVT_ITEM_ADDED,
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            )
        };
        vec![Self::ack_payload(seq, &serde_json::json!({})), ev]
    }

    /// M24: обновление трупа всем в локации (себе — Out, остальным —
    /// в коннекты): лут инъекции/удаления виден без переоткрытия.
    pub(super) fn broadcast_corpse(&self, corpse_id: &str) -> Vec<Out> {
        let current = self.location_id.clone().unwrap_or_default();
        let text = {
            let world = self.state.world.lock().expect("мир");
            let Some(corpse) = world.corpse(corpse_id) else {
                return Vec::new();
            };
            let loot_count = corpse.loot.iter().filter(|l| l.is_some()).count() as u32;
            event_text(
                names::EVT_CORPSE_CREATED,
                None,
                serde_json::to_value(ld2_protocol::evt::CorpseCreated {
                    entity: corpse.entity(),
                    loot_count,
                })
                .unwrap_or_default(),
            )
        };
        let own = self.selected_char_id.clone();
        let world = self.state.world.lock().expect("мир");
        let outs = vec![Out::Text(text.clone())];
        for addr in world.chars_in_location(&current) {
            if own.as_deref() != Some(addr.as_str()) {
                world.send_to_char(&addr, text.clone());
            }
        }
        outs
    }
}
