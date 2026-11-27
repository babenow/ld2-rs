//! RF1v: админ-часть — предметы, god mode, reload и диалог.

use super::*;

const MAX_ADMIN_QTY: i64 = 999;

impl Session {
    /// `cmd/admin.items` (§7): каталог всех определений — левая панель
    /// окна выдачи (вариант A: одним запросом, клиент кэширует и
    /// фильтрует локально). Тяжёлые поля (stats/requires) не отдаём.
    pub(in crate::session) async fn admin_items(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let Ok(_cmd) = Self::payload_of::<AdminItems>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let defs = match db::items::defs_all(&self.state.pool).await {
            Ok(defs) => defs,
            Err(e) => {
                warn!("admin items: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let items: Vec<serde_json::Value> = defs
            .iter()
            .map(|d| {
                serde_json::json!({
                    "id": d.id,
                    "name": d.name,
                    "type": d.item_type,
                    "rarity": d.rarity,
                    "qty": d.qty,
                    "upgrade": d.upgrade,
                    "gold_value": d.gold_value,
                })
            })
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "items": items }),
        )]
    }

    /// `cmd/admin.god_mode` (§7): toggle неуязвимости — состояние в
    /// мире (tick::apply_mob_hit смотрит оттуда), гейт admin.
    pub(in crate::session) async fn admin_god_mode(
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
        let Ok(cmd) = Self::payload_of::<AdminGodMode>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_god(&char_id, cmd.enabled);
        }
        tracing::info!("admin god mode: {char_id} → enabled={}", cmd.enabled);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "enabled": cmd.enabled }),
        )]
    }

    /// `cmd/admin.content.reload` (M27, фаза 1): перечитать каталог
    /// контента на работающем сервере — сид в БД (если версия изменилась),
    /// замена диалогов/магазинов/версии в памяти и сброс кэша карты.
    /// Только `role=admin` (§9 `E_PERMISSION`), аудит в лог.
    pub(in crate::session) async fn admin_content_reload(
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
        if Self::payload_of::<AdminContentReload>(env, seq).is_err() {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        }
        match crate::reload::reload_state(&self.state).await {
            Ok(outcome) => {
                tracing::info!(
                    "admin content reload: версия {} (БД: {}, локаций {}, предметов {})",
                    short_version(&outcome.version),
                    if outcome.applied {
                        "применён"
                    } else {
                        "без изменений"
                    },
                    outcome.locations,
                    outcome.items
                );
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({
                        "version": outcome.version,
                        "applied": outcome.applied,
                        "locations": outcome.locations,
                        "items": outcome.items,
                    }),
                )]
            }
            Err(e) => {
                warn!("admin content reload: {e}");
                vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "контент не применён (старое состояние сохранено, см. логи)",
                )]
            }
        }
    }

    /// M24: submit админ-диалога — `give:<target>#<def_id>` /
    /// `remove:<target>#<ref>`. Роль ПОВТОРНО (диалог мог открыть
    /// уже не-админ), qty >= 1; верхняя граница 999 — кроме ВАЛЮТЫ
    /// (`type=money`, без потолка; `E_BAD_QTY`), цель
    /// валидируется повторно (могла исчезнуть).
    pub(in crate::session) async fn admin_dialog_submit(
        &mut self,
        rest: &str,
        values: &serde_json::Map<String, serde_json::Value>,
        seq: u32,
    ) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let qty = values
            .get("count")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        // Нижняя граница — всегда; верхняя (999) проверяется в ветках
        // give/remove: у ВАЛЮТЫ (type=money) потолка нет (указание
        // заказчика) — граница стака INTEGER в БД.
        if qty < 1 {
            return vec![Self::err(seq, ErrorCode::BadQuantity, "количество: от 1")];
        }
        if let Some(args) = rest.strip_prefix("give:") {
            let Some((target, def_id)) = args.split_once('#') else {
                return vec![Self::err(seq, ErrorCode::Internal, "битый id диалога")];
            };
            if !self.admin_target_valid(target).await {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель исчезла")];
            }
            if qty > MAX_ADMIN_QTY {
                // Валюта (type=money) — без потолка 999 (указание
                // заказчика); предел — INTEGER-стак в БД.
                let ty = db::items::def(&self.state.pool, def_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.item_type);
                if ty.as_deref() != Some(crate::db::items::MONEY_TYPE) || qty > i64::from(i32::MAX)
                {
                    return vec![Self::err(
                        seq,
                        ErrorCode::BadQuantity,
                        "количество: 1..=999 (валюта — до 2147483647)",
                    )];
                }
            }
            if let Some(id) = target.strip_prefix("corpse:") {
                return self.admin_give_corpse(id, def_id, qty, seq).await;
            }
            if let Some(id) = target.strip_prefix("monster:") {
                return self.admin_give_monster(id, def_id, qty, seq).await;
            }
            if let Some(id) = target.strip_prefix("container:") {
                return self.admin_give_container(id, def_id, qty, seq).await;
            }
            if let Some(id) = target.strip_prefix("shop:") {
                return self.admin_give_shop(id, def_id, qty, seq).await;
            }
            let Some(char_id) = self.admin_target_char(target).await else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "персонаж не найден")];
            };
            return self.admin_give_to(&char_id, def_id, qty, seq).await;
        }
        if let Some(args) = rest.strip_prefix("remove:") {
            let Some((target, item_ref)) = args.split_once('#') else {
                return vec![Self::err(seq, ErrorCode::Internal, "битый id диалога")];
            };
            if !self.admin_target_valid(target).await {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель исчезла")];
            }
            if qty > MAX_ADMIN_QTY {
                // Валюта (type=money) — без потолка 999 (указание
                // заказчика); удаление и так зажимается остатком.
                let ty = self
                    .admin_item_def(target, item_ref)
                    .await
                    .map(|d| d.item_type);
                if ty.as_deref() != Some(crate::db::items::MONEY_TYPE) || qty > i64::from(i32::MAX)
                {
                    return vec![Self::err(
                        seq,
                        ErrorCode::BadQuantity,
                        "количество: 1..=999 (валюта — до 2147483647)",
                    )];
                }
            }
            // Слоты трупа и админ-лут монстра — по формату ref.
            if let Some(rest) = item_ref.strip_prefix("loot:") {
                let Some((corpse_id, idx)) = rest.rsplit_once(':') else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Ok(idx) = idx.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                return self
                    .admin_remove_corpse_slot(corpse_id, false, idx, qty, seq)
                    .await;
            }
            if let Some(rest) = item_ref.strip_prefix("inner:") {
                let Some((corpse_id, idx)) = rest.rsplit_once(':') else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Ok(idx) = idx.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                return self
                    .admin_remove_corpse_slot(corpse_id, true, idx, qty, seq)
                    .await;
            }
            if let Some(rest) = item_ref.strip_prefix("bonus:") {
                let Ok(idx) = rest.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Some(mob_id) = target.strip_prefix("monster:") else {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Internal,
                        "bonus- ref без монстра",
                    )];
                };
                return self.admin_remove_monster(mob_id, idx, qty, seq).await;
            }
            // Сундук (F24d): базовый слот (tombstone) или надстройка.
            if let Some(rest) = item_ref.strip_prefix("cont:") {
                let Some((entity_id, idx)) = rest.rsplit_once(':') else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Ok(idx) = idx.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                // Базовый слот — одна позиция (tombstone), qty не нужен.
                return self
                    .admin_remove_container(entity_id, idx, true, 1, seq)
                    .await;
            }
            if let Some(rest) = item_ref.strip_prefix("contx:") {
                let Some((entity_id, idx)) = rest.rsplit_once(':') else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Ok(idx) = idx.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                return self
                    .admin_remove_container(entity_id, idx, false, qty, seq)
                    .await;
            }
            // Магазин (F24d): контентная позиция или надстройка.
            if let Some(rest) = item_ref.strip_prefix("xshop:") {
                let Ok(idx) = rest.parse::<usize>() else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Some(npc_id) = target.strip_prefix("shop:") else {
                    return vec![Self::err(seq, ErrorCode::Internal, "xshop без магазина")];
                };
                return self.admin_remove_shop_extra(npc_id, idx, qty, seq).await;
            }
            if let Some(rest) = item_ref.strip_prefix("shop:") {
                let Some((tab, idx)) = rest.rsplit_once(':') else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let (Ok(tab), Ok(idx)) = (tab.parse::<usize>(), idx.parse::<usize>()) else {
                    return vec![Self::err(seq, ErrorCode::Internal, "битый ref")];
                };
                let Some(npc_id) = target.strip_prefix("shop:") else {
                    return vec![Self::err(seq, ErrorCode::Internal, "shop-ref без магазина")];
                };
                return self.admin_remove_shop_content(npc_id, tab, idx, seq).await;
            }
            // Инвентарь (self/char) — uid.
            let Some(char_id) = self.admin_target_char(target).await else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "персонаж не найден")];
            };
            return self.admin_remove_from(&char_id, item_ref, qty, seq).await;
        }
        vec![Self::err(
            seq,
            ErrorCode::Internal,
            "неизвестный админ-диалог",
        )]
    }
}
