//! RF1v: админ-часть — цели, определения и очистка.

use ld2_protocol::names;

use super::*;

impl Session {
    /// M24: цель существует и валидна для текущего контекста:
    /// `self` (выбран персонаж), `char:<имя>` (есть в БД, в т.ч.
    /// оффлайн), `corpse:<id>`/`monster:<id>` (в текущей локации).
    /// Сундук/магазин — F24d.
    pub(in crate::session) async fn admin_target_valid(&self, target: &str) -> bool {
        if target == "self" {
            return self.selected_char_id.is_some();
        }
        if let Some(name) = target.strip_prefix("char:") {
            return matches!(
                db::characters::id_by_name(&self.state.pool, name).await,
                Ok(Some(_))
            );
        }
        let current = self.location_id.clone().unwrap_or_default();
        // Динамические цели — память мира (возврат внутри scope:
        // блокировка не держится через await).
        if target.starts_with("corpse:") || target.starts_with("monster:") {
            let world = self.state.world.lock().expect("мир");
            return if let Some(id) = target.strip_prefix("corpse:") {
                world.corpse(id).is_some_and(|c| c.location_id == current)
            } else if let Some(id) = target.strip_prefix("monster:") {
                world.mob(id).is_some_and(|m| m.location_id == current)
            } else {
                false
            };
        }
        // Статические цели (F24d): сундук/магазин — сущности текущей
        // локации из БД + магазины контента.
        let (container, id) = if let Some(id) = target.strip_prefix("container:") {
            (true, id)
        } else if let Some(id) = target.strip_prefix("shop:") {
            (false, id)
        } else {
            return false;
        };
        let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
            return false;
        };
        let entities: Vec<Entity> = serde_json::from_value(loc_row.entities).unwrap_or_default();
        let kind = if container {
            ld2_protocol::types::EntityKind::Container
        } else {
            ld2_protocol::types::EntityKind::Npc
        };
        if !entities.iter().any(|e| e.id == id && e.kind == kind) {
            return false;
        }
        if !container {
            // Магазин существует и имеет валютные вкладки.
            return self
                .state
                .content()
                .shops
                .get(id)
                .is_some_and(|shop| !shop.currencies.is_empty());
        }
        true
    }

    /// M24: char_id цели-игрока (`self` → свой персонаж).
    pub(in crate::session) async fn admin_target_char(&self, target: &str) -> Option<String> {
        if target == "self" {
            return self.selected_char_id.clone();
        }
        let name = target.strip_prefix("char:")?;
        db::characters::id_by_name(&self.state.pool, name)
            .await
            .ok()
            .flatten()
    }

    /// M24: имя предмета для заголовка диалога удаления (по цели и ref):
    /// uid инвентаря / `loot:`-слот / `inner:`-слот / `bonus:`-лут монстра.
    pub(in crate::session) async fn admin_item_def(
        &self,
        target: &str,
        item_ref: &str,
    ) -> Option<db::items::ItemDefRow> {
        let def_id = if let Some(rest) = item_ref.strip_prefix("loot:") {
            let (corpse_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.corpse(corpse_id)?.loot.get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("inner:") {
            let (corpse_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.corpse(corpse_id)?.inner_loot.get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("bonus:") {
            let idx: usize = rest.parse().ok()?;
            let mob_id = target.strip_prefix("monster:")?;
            let world = self.state.world.lock().expect("мир");
            world.mob_bonus(mob_id).get(idx)?.clone()
        } else if let Some(rest) = item_ref.strip_prefix("cont:") {
            let (entity_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let current = self.location_id.clone().unwrap_or_default();
            let loc_row = db::locations::get(&self.state.pool, &current)
                .await
                .ok()??;
            let entities: Vec<Entity> = serde_json::from_value(loc_row.entities).ok()?;
            let entity = entities.into_iter().find(|e| e.id == entity_id)?;
            entity_loot_defs(&entity).get(idx)?.clone()
        } else if let Some(rest) = item_ref.strip_prefix("contx:") {
            let (entity_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.container_extra_slot(entity_id, idx)?.0
        } else if let Some(rest) = item_ref.strip_prefix("xshop:") {
            let idx: usize = rest.parse().ok()?;
            let npc_id = target.strip_prefix("shop:")?;
            let world = self.state.world.lock().expect("мир");
            world.shop_extra(npc_id).get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("shop:") {
            let (tab, idx) = rest.rsplit_once(':')?;
            let (tab, idx): (usize, usize) = (tab.parse().ok()?, idx.parse().ok()?);
            let npc_id = target.strip_prefix("shop:")?;
            self.state
                .content()
                .shops
                .get(npc_id)?
                .currencies
                .get(tab)?
                .items
                .get(idx)?
                .clone()
        } else {
            let char_id = self.admin_target_char(target).await?;
            let row = db::items::find(&self.state.pool, item_ref, &char_id)
                .await
                .ok()??;
            row.def_id
        };
        db::items::def(&self.state.pool, &def_id)
            .await
            .ok()
            .flatten()
    }

    /// M24: имя предмета по цели/ref — заголовок диалога удаления.
    pub(in crate::session) async fn admin_item_name(
        &self,
        target: &str,
        item_ref: &str,
    ) -> Option<String> {
        self.admin_item_def(target, item_ref).await.map(|d| d.name)
    }

    /// `cmd/admin.target.info` (§7): содержимое цели для правой
    /// колонки админ-окна — инвентарь (`self`/`char:`), слоты лута
    /// трупа (`loot:`/`inner:`), админ-лут монстра (`bonus:`).
    pub(in crate::session) async fn admin_target_info(
        &mut self,
        env: &Envelope,
        seq: u32,
    ) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<AdminTargetInfo>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не найдена")];
        }
        // Инвентарь (self/char) — одним SQL с джойном определений.
        if let Some(char_id) = self.admin_target_char(&cmd.target).await {
            let rows = match db::items::admin_inventory(&self.state.pool, &char_id).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("admin target.info (инвентарь {char_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "ref": r.uid,
                        "id": r.def_id,
                        "name": r.name,
                        "type": r.item_type,
                        "rarity": r.rarity,
                        "qty": r.qty,
                    })
                })
                .collect();
            return vec![Self::ack_payload(
                seq,
                &serde_json::json!({ "items": items }),
            )];
        }
        // Сундук/магазин (F24d) — статика локации/контента; труп/
        // монстр — память мира; блокировки не держатся через await.
        let slots: Vec<(String, String, Option<i64>)> = if cmd.target.starts_with("container:") {
            let id = &cmd.target["container:".len()..];
            let current = self.location_id.clone().unwrap_or_default();
            let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "сундука нет")];
            };
            let entities: Vec<Entity> =
                serde_json::from_value(loc_row.entities).unwrap_or_default();
            let Some(entity) = entities.into_iter().find(|e| e.id == id) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "сундука нет")];
            };
            let defs = entity_loot_defs(&entity);
            let (removed, extra) = {
                let world = self.state.world.lock().expect("мир");
                let removed: std::collections::HashSet<usize> = (0..defs.len())
                    .filter(|i| world.container_base_removed(id, *i))
                    .collect();
                (removed, world.container_extra(id))
            };
            let mut out: Vec<(String, String, Option<i64>)> = defs
                .iter()
                .enumerate()
                .filter(|(i, _)| !removed.contains(i))
                .map(|(i, d)| (format!("cont:{id}:{i}"), d.clone(), None))
                .collect();
            out.extend(extra.iter().enumerate().filter_map(|(n, slot)| {
                slot.as_ref()
                    .map(|(d, q)| (format!("contx:{id}:{n}"), d.clone(), Some(*q)))
            }));
            out
        } else if cmd.target.starts_with("shop:") {
            let npc = &cmd.target["shop:".len()..];
            let content = self.state.content();
            let Some(shop) = content.shops.get(npc) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
            };
            let (extra, removed) = {
                let world = self.state.world.lock().expect("мир");
                let mut removed = std::collections::HashSet::new();
                for (tab, currency) in shop.currencies.iter().enumerate() {
                    for idx in 0..currency.items.len() {
                        if world.shop_content_removed(npc, tab, idx) {
                            removed.insert((tab, idx));
                        }
                    }
                }
                (world.shop_extra(npc), removed)
            };
            let mut out: Vec<(String, String, Option<i64>)> = Vec::new();
            for (tab, currency) in shop.currencies.iter().enumerate() {
                for (idx, def_id) in currency.items.iter().enumerate() {
                    if removed.contains(&(tab, idx)) {
                        continue;
                    }
                    out.push((format!("shop:{tab}:{idx}"), def_id.clone(), None));
                }
            }
            out.extend(extra.iter().enumerate().filter_map(|(n, slot)| {
                slot.as_ref()
                    .map(|d| (format!("xshop:{n}"), d.clone(), None))
            }));
            out
        } else {
            let world = self.state.world.lock().expect("мир");
            if let Some(id) = cmd.target.strip_prefix("corpse:") {
                let Some(corpse) = world.corpse(id) else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
                };
                corpse
                    .loot
                    .iter()
                    .enumerate()
                    .filter_map(|(i, slot)| {
                        slot.as_ref()
                            .map(|d| (format!("loot:{id}:{i}"), d.clone(), None))
                    })
                    .chain(
                        corpse
                            .inner_loot
                            .iter()
                            .enumerate()
                            .filter_map(|(i, slot)| {
                                slot.as_ref()
                                    .map(|d| (format!("inner:{id}:{i}"), d.clone(), None))
                            }),
                    )
                    .collect()
            } else if let Some(id) = cmd.target.strip_prefix("monster:") {
                world
                    .mob_bonus(id)
                    .into_iter()
                    .enumerate()
                    .map(|(i, d)| (format!("bonus:{i}"), d, None))
                    .collect()
            } else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "неизвестная цель")];
            }
        };
        let mut items: Vec<serde_json::Value> = Vec::new();
        for (item_ref, def_id, custom_qty) in slots {
            let Some(def) = db::items::def(&self.state.pool, &def_id)
                .await
                .ok()
                .flatten()
            else {
                continue; // слот ссылается на несуществующее определение
            };
            items.push(serde_json::json!({
                "ref": item_ref,
                "id": def.id,
                "name": def.name,
                "type": def.item_type,
                "rarity": def.rarity,
                // M24: у стака надстройки — своё количество.
                "qty": custom_qty.unwrap_or_else(|| i64::from(def.qty)),
            }));
        }
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "items": items }),
        )]
    }

    /// `cmd/admin.clear` (§7): удалить ВСЕ предметы цели (кнопка
    /// «Удалить все» в админ-окне): инвентарь — все записи, труп —
    /// все слоты (broadcast), сундук — база tombstone-ами + надстройка,
    /// магазин — контент + надстройка, монстр — весь лут (восстановится
    /// респавном). Аудит каждой операции.
    pub(in crate::session) async fn admin_clear(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<AdminClear>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не найдена")];
        }

        // ── инвентарь (self/char) ──
        if let Some(char_id) = self.admin_target_char(&cmd.target).await {
            let own = match self.require_char_id(seq) {
                Ok(id) => id,
                Err(out) => return vec![out],
            };
            let rows = match db::items::delete_all(&self.state.pool, &char_id).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("admin clear (инвентарь {char_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            tracing::info!(
                "admin clear inventory: {char_id} ({} предметов)",
                rows.len()
            );
            if own == char_id {
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                for row in rows {
                    outs.push(out_event(
                        names::EVT_ITEM_REMOVED,
                        None,
                        serde_json::to_value(ItemRemoved {
                            uid: row.uid,
                            reason: ItemRemovalReason::Consumed,
                        })
                        .unwrap_or_default(),
                    ));
                }
                return outs;
            }
            // Чужой персонаж: события — в его коннект (оффлайн просто
            // увидит пустой инвентарь при входе).
            {
                let world = self.state.world.lock().expect("мир");
                for row in rows {
                    world.send_to_char(
                        &char_id,
                        event_text(
                            names::EVT_ITEM_REMOVED,
                            None,
                            serde_json::to_value(ItemRemoved {
                                uid: row.uid,
                                reason: ItemRemovalReason::Consumed,
                            })
                            .unwrap_or_default(),
                        ),
                    );
                }
            }
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── труп: все слоты + broadcast ──
        if let Some(id) = cmd.target.strip_prefix("corpse:") {
            let cleared = {
                let mut world = self.state.world.lock().expect("мир");
                world.corpse_clear(id)
            };
            if cleared.is_none() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            }
            tracing::info!("admin clear corpse: {id}");
            let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
            outs.extend(self.broadcast_corpse(id));
            return outs;
        }

        // ── сундук: база → tombstone (глобально) + надстройка ──
        if let Some(id) = cmd.target.strip_prefix("container:") {
            let current = self.location_id.clone().unwrap_or_default();
            let defs_len = {
                let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "локации нет")];
                };
                let entities: Vec<Entity> =
                    serde_json::from_value(loc_row.entities).unwrap_or_default();
                entities
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| entity_loot_defs(e).len())
                    .unwrap_or(0)
            };
            {
                let mut world = self.state.world.lock().expect("мир");
                for i in 0..defs_len {
                    world.container_base_remove(id, i);
                }
                world.container_clear(id);
            }
            tracing::info!("admin clear container: {id} (база {defs_len})");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── магазин: контент → tombstone + надстройка ──
        if let Some(npc) = cmd.target.strip_prefix("shop:") {
            let content = self.state.content();
            let Some(shop) = content.shops.get(npc) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
            };
            let tabs: Vec<(usize, usize)> = shop
                .currencies
                .iter()
                .enumerate()
                .map(|(tab, currency)| (tab, currency.items.len()))
                .collect();
            {
                let mut world = self.state.world.lock().expect("мир");
                for (tab, len) in tabs {
                    for idx in 0..len {
                        world.shop_content_remove(npc, tab, idx);
                    }
                }
                world.shop_clear(npc);
            }
            tracing::info!("admin clear shop: {npc}");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── монстр: весь живой лут (восстановится респавном) ──
        if let Some(id) = cmd.target.strip_prefix("monster:") {
            let ok = {
                let mut world = self.state.world.lock().expect("мир");
                world.mob_drops_clear(id)
            };
            if !ok {
                return vec![Self::err(seq, ErrorCode::TargetGone, "монстра нет")];
            }
            tracing::info!("admin clear monster: {id}");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        vec![Self::err(seq, ErrorCode::TargetGone, "неизвестная цель")]
    }
}
