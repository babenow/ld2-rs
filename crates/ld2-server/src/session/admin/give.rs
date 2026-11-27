//! RF1v: админ-часть — выдача и удаление.

use ld2_protocol::names;

use super::*;

impl Session {
    /// `cmd/admin.give.ask` (§7): валидация def/цели → открытие
    /// execute-диалога количества; id `admin.give:<target>#<def_id>`
    /// кодирует действие — submit stateless (роль там повторно).
    pub(in crate::session) async fn admin_give_ask(
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
        let Ok(cmd) = Self::payload_of::<AdminGiveAsk>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "цель не найдена или не поддерживается",
            )];
        }
        let Some(def) = (match db::items::def(&self.state.pool, &cmd.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("admin give.ask (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        };
        let open = ld2_protocol::evt::DialogOpen {
            id: format!("admin.give:{}#{}", cmd.target, cmd.def_id),
            kind: "execute".into(),
            title: format!("Выдать «{}»", def.name),
            pages: Vec::new(),
            inputs: vec![ld2_protocol::evt::DialogInput {
                id: "count".into(),
                label: "Количество".into(),
                kind: "number".into(),
                default: "1".into(),
            }],
        };
        vec![
            Self::ack_payload(seq, &serde_json::json!({})),
            out_event(
                names::EVT_DIALOG_OPEN,
                None,
                serde_json::to_value(open).unwrap_or_default(),
            ),
        ]
    }

    /// `cmd/admin.remove.ask` (§7): execute-диалог количества на
    /// удаление; ref — uid / `loot:`/`inner:`-слот / `bonus:`.
    pub(in crate::session) async fn admin_remove_ask(
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
        let Ok(cmd) = Self::payload_of::<AdminRemoveAsk>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "цель не найдена или не поддерживается",
            )];
        }
        let name = self
            .admin_item_name(&cmd.target, &cmd.item_ref)
            .await
            .unwrap_or_else(|| cmd.item_ref.clone());
        let open = ld2_protocol::evt::DialogOpen {
            id: format!("admin.remove:{}#{}", cmd.target, cmd.item_ref),
            kind: "execute".into(),
            title: format!("Удалить «{name}»"),
            pages: Vec::new(),
            inputs: vec![ld2_protocol::evt::DialogInput {
                id: "count".into(),
                label: "Количество".into(),
                kind: "number".into(),
                default: "1".into(),
            }],
        };
        vec![
            Self::ack_payload(seq, &serde_json::json!({})),
            out_event(
                names::EVT_DIALOG_OPEN,
                None,
                serde_json::to_value(open).unwrap_or_default(),
            ),
        ]
    }

    /// M24: выдать `qty` в инвентарь `target_char` — стак/слот по
    /// общим правилам (`take_loot_fits` + `grant_stacking`), сокеты
    /// новому экземпляру (паритет `scenario_give`); себе — события
    /// Out, другому (онлайн) — в его коннект (оффлайн увидит при
    /// входе — инвентарь из БД); аудит.
    pub(in crate::session) async fn admin_give_to(
        &mut self,
        target_char: &str,
        def_id: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        let own = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(def) = (match db::items::def(pool, def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("admin give (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        };
        let slots = self.state.config.gameplay.inventory.slots;
        match Self::take_loot_fits(pool, target_char, def_id, slots).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::InventoryFull,
                    "инвентарь заполнен",
                )];
            }
            Err(e) => {
                warn!("admin give (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let (row, stacked) = match db::items::grant_stacking(pool, target_char, def_id, qty).await {
            Ok(v) => v,
            Err(e) => {
                warn!("admin give (grant): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if !stacked && def.sockets > 0 {
            if let Err(e) =
                db::items::set_sockets(pool, &row.uid, Self::empty_sockets(def.sockets)).await
            {
                warn!("admin give (сокеты): {e}");
            }
        }
        tracing::info!("admin give: {def_id} ×{qty} → {target_char} (стак={stacked})");
        let text = if stacked {
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(row.qty));
            event_text(
                names::EVT_ITEM_STATE,
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        } else {
            event_text(
                names::EVT_ITEM_ADDED,
                None,
                serde_json::to_value(ItemAdded {
                    item: item_snapshot(&row, &def),
                })
                .unwrap_or_default(),
            )
        };
        if target_char == own {
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                Out::Text(text),
            ]
        } else {
            {
                let world = self.state.world.lock().expect("мир");
                world.send_to_char(target_char, text);
            }
            vec![Self::ack_payload(seq, &serde_json::json!({}))]
        }
    }

    /// M24: удалить `qty` из инвентаря `target_char` — весь стак
    /// (≥ qty) или частично; маршрутизация событий как в выдаче.
    pub(in crate::session) async fn admin_remove_from(
        &mut self,
        target_char: &str,
        item_ref: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        let own = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, item_ref, target_char).await {
            Ok(item) => item,
            Err(e) => {
                warn!("admin remove (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмет не найден")];
        };
        tracing::info!(
            "admin remove: {item_ref} ×{qty} из {target_char} (было {})",
            item.qty
        );
        let text = if qty >= i64::from(item.qty) {
            if let Err(e) = db::items::delete(pool, item_ref).await {
                warn!("admin remove (delete): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            event_text(
                names::EVT_ITEM_REMOVED,
                None,
                serde_json::to_value(ItemRemoved {
                    uid: item.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            )
        } else {
            let new_qty = i64::from(item.qty) - qty;
            if let Err(e) = db::items::set_qty(pool, item_ref, new_qty).await {
                warn!("admin remove (set_qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            event_text(
                names::EVT_ITEM_STATE,
                None,
                serde_json::to_value(ItemState {
                    uid: item.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        };
        if target_char == own {
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                Out::Text(text),
            ]
        } else {
            {
                let world = self.state.world.lock().expect("мир");
                world.send_to_char(target_char, text);
            }
            vec![Self::ack_payload(seq, &serde_json::json!({}))]
        }
    }

    /// M24: выдача `qty` слотов в лут трупа (общий список) + broadcast.
    pub(in crate::session) async fn admin_give_corpse(
        &mut self,
        corpse_id: &str,
        def_id: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if db::items::def(&self.state.pool, def_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        }
        let current = self.location_id.clone().unwrap_or_default();
        {
            let world = self.state.world.lock().expect("мир");
            match world.corpse(corpse_id) {
                Some(corpse) if corpse.location_id == current => {}
                _ => return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет в локации")],
            }
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            for _ in 0..qty {
                if world.corpse_loot_add(corpse_id, def_id).is_none() {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
                }
            }
        }
        tracing::info!("admin give corpse: {def_id} ×{qty} → {corpse_id}");
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        outs.extend(self.broadcast_corpse(corpse_id));
        outs
    }

    /// M24: удаление слотов трупа (общий лут/«Внутри») + broadcast.
    pub(in crate::session) async fn admin_remove_corpse_slot(
        &mut self,
        corpse_id: &str,
        inner: bool,
        idx: usize,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        let current = self.location_id.clone().unwrap_or_default();
        {
            let world = self.state.world.lock().expect("мир");
            match world.corpse(corpse_id) {
                Some(corpse) if corpse.location_id == current => {}
                _ => return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет в локации")],
            }
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            if world
                .corpse_slot_remove_at(corpse_id, inner, idx, qty as usize)
                .is_none()
            {
                return vec![Self::err(seq, ErrorCode::ItemGone, "слотов нет")];
            }
        }
        tracing::info!(
            "admin remove corpse{}: {corpse_id} ×{qty} с {idx}",
            if inner { " inner" } else { "" }
        );
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        outs.extend(self.broadcast_corpse(corpse_id));
        outs
    }

    /// M24: админ-лут монстра (`qty` копий) — попадёт в труп при
    /// смерти, при респавне забудется (in-memory, one-shot).
    pub(in crate::session) async fn admin_give_monster(
        &mut self,
        mob_id: &str,
        def_id: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if db::items::def(&self.state.pool, def_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        }
        let current = self.location_id.clone().unwrap_or_default();
        {
            let mut world = self.state.world.lock().expect("мир");
            match world.mob(mob_id) {
                Some(mob) if mob.location_id == current => {}
                _ => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::TargetGone,
                        "монстра нет в локации",
                    )]
                }
            }
            for _ in 0..qty {
                if !world.mob_bonus_add(mob_id, def_id) {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "монстра нет")];
                }
            }
        }
        tracing::info!("admin give monster: {def_id} ×{qty} → {mob_id}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: убрать `qty` позиций админ-лута монстра с `idx`.
    pub(in crate::session) async fn admin_remove_monster(
        &mut self,
        mob_id: &str,
        idx: usize,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        let current = self.location_id.clone().unwrap_or_default();
        {
            let mut world = self.state.world.lock().expect("мир");
            match world.mob(mob_id) {
                Some(mob) if mob.location_id == current => {}
                _ => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::TargetGone,
                        "монстра нет в локации",
                    )]
                }
            }
            if !world.mob_bonus_remove_at(mob_id, idx, qty as usize) {
                return vec![Self::err(seq, ErrorCode::ItemGone, "лута монстра нет")];
            }
        }
        tracing::info!("admin remove monster: {mob_id} ×{qty} с {idx}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: выдача в сундук — надстройка in-memory (слоты после
    /// базовых defs; респавн сундука её не трогает).
    pub(in crate::session) async fn admin_give_container(
        &mut self,
        entity_id: &str,
        def_id: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if db::items::def(&self.state.pool, def_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        }
        if !self
            .admin_target_valid(&format!("container:{entity_id}"))
            .await
        {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "сундука нет в локации",
            )];
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            world.container_extra_add(entity_id, def_id, qty);
        }
        tracing::info!("admin give container: {def_id} ×{qty} → {entity_id}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: удаление из сундука — базовый слот (глобальный tombstone,
    /// одна позиция) либо `qty` единиц из стака надстройки.
    pub(in crate::session) async fn admin_remove_container(
        &mut self,
        entity_id: &str,
        idx: usize,
        base: bool,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if !self
            .admin_target_valid(&format!("container:{entity_id}"))
            .await
        {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "сундука нет в локации",
            )];
        }
        if base {
            // Границы — по базовым defs сущности (БД, до блокировок).
            let current = self.location_id.clone().unwrap_or_default();
            let defs_len = {
                let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "локации нет")];
                };
                let entities: Vec<Entity> =
                    serde_json::from_value(loc_row.entities).unwrap_or_default();
                entities
                    .iter()
                    .find(|e| e.id == entity_id)
                    .map(|e| entity_loot_defs(e).len())
                    .unwrap_or(0)
            };
            if idx >= defs_len {
                return vec![Self::err(seq, ErrorCode::ItemGone, "слота нет")];
            }
            let already = {
                let world = self.state.world.lock().expect("мир");
                world.container_base_removed(entity_id, idx)
            };
            if already {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже удалено")];
            }
            let mut world = self.state.world.lock().expect("мир");
            world.container_base_remove(entity_id, idx);
        } else {
            let mut world = self.state.world.lock().expect("мир");
            if !world.container_extra_reduce(entity_id, idx, qty) {
                return vec![Self::err(seq, ErrorCode::ItemGone, "слотов нет")];
            }
        }
        tracing::info!("admin remove container: {entity_id} base={base} idx={idx} ×{qty}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: выдача в магазин — надстройка in-memory (таб0 в shop.list).
    pub(in crate::session) async fn admin_give_shop(
        &mut self,
        npc_id: &str,
        def_id: &str,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if db::items::def(&self.state.pool, def_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого предмета")];
        }
        if !self.admin_target_valid(&format!("shop:{npc_id}")).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            world.shop_extra_add(npc_id, def_id);
        }
        tracing::info!("admin give shop: {def_id} ×{qty} → {npc_id}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: удаление контентного товара магазина (tombstone, одна
    /// позиция; покупатели увидят изменение в следующем shop.list).
    pub(in crate::session) async fn admin_remove_shop_content(
        &mut self,
        npc_id: &str,
        tab: usize,
        idx: usize,
        seq: u32,
    ) -> Vec<Out> {
        if !self.admin_target_valid(&format!("shop:{npc_id}")).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
        }
        let exists = self
            .state
            .content()
            .shops
            .get(npc_id)
            .and_then(|shop| shop.currencies.get(tab))
            .and_then(|currency| currency.items.get(idx))
            .is_some();
        if !exists {
            return vec![Self::err(seq, ErrorCode::ItemGone, "товара нет")];
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            if world.shop_content_removed(npc_id, tab, idx) {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже удалено")];
            }
            world.shop_content_remove(npc_id, tab, idx);
        }
        tracing::info!("admin remove shop content: {npc_id} tab={tab} idx={idx}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// M24: удаление из надстройки магазина (`qty` позиций).
    pub(in crate::session) async fn admin_remove_shop_extra(
        &mut self,
        npc_id: &str,
        idx: usize,
        qty: i64,
        seq: u32,
    ) -> Vec<Out> {
        if !self.admin_target_valid(&format!("shop:{npc_id}")).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
        }
        {
            let mut world = self.state.world.lock().expect("мир");
            if !world.shop_extra_take_at(npc_id, idx, qty as usize) {
                return vec![Self::err(seq, ErrorCode::ItemGone, "позиций нет")];
            }
        }
        tracing::info!("admin remove shop extra: {npc_id} idx={idx} ×{qty}");
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }
}
