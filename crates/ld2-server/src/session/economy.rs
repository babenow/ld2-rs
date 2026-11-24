//! RF1m: экономика (M14/M16) — витрина магазина, покупка с учётом скидки
//! и события изменения золота/инвентаря.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{ShopBuy, ShopList};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{ItemAdded, ItemRemovalReason, ItemRemoved, ItemState};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::item_snapshot;
use super::{out_event, Out, Session};
use crate::db;

impl Session {
    /// F29a: события списания стаков монет — патч `qty` уцелевшего
    /// стака либо `evt/item/removed` опустевшего (Consumed).
    pub(super) fn money_events(changes: Vec<(String, Option<i64>)>) -> Vec<Out> {
        changes
            .into_iter()
            .map(|(uid, new_qty)| match new_qty {
                Some(qty) => {
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(qty));
                    out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState { uid, patch }).unwrap_or_default(),
                    )
                }
                None => out_event(
                    "evt/item/removed",
                    None,
                    serde_json::to_value(ItemRemoved {
                        uid,
                        reason: ItemRemovalReason::Consumed,
                    })
                    .unwrap_or_default(),
                ),
            })
            .collect()
    }

    /// Скидка персонажа в bp: аккаунт (премиум — SQL,
    /// `accounts.shop_discount_bp`) + навыки из
    /// `[gameplay.shop].skill_discounts` (множитель за ранг); потолок
    /// применяет `shops::buy_price_gold`.
    pub(super) async fn shop_discount_bp(&self, char_id: &str, seq: u32) -> Result<i64, Out> {
        let pool = &self.state.pool;
        let account_id = self.account_id.expect("require_auth выше");
        let account_bp = match db::accounts::shop_discount_bp(pool, account_id).await {
            Ok(bp) => bp,
            Err(e) => {
                warn!("shop (скидка аккаунта): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let mut ranks = std::collections::HashMap::new();
        match db::skills::learned(pool, char_id).await {
            Ok(rows) => {
                for skill in rows {
                    ranks.insert(skill.skill_id, i64::from(skill.rank));
                }
            }
            Err(e) => {
                warn!("shop (навыки): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        let skills_bp = crate::shops::skill_discount_bp(
            &self.state.config.gameplay.shop.skill_discounts,
            &ranks,
        );
        Ok(account_bp + skills_bp)
    }

    /// `cmd/shop.list` (M18/F39): товары NPC по валютным вкладкам с
    /// ценами (база `gold_value` × наценка сервера × наценка торговца ×
    /// скидка bp с потолком → конвертация `rate`).
    /// ack `{shop: {tabs: [{currency, currency_name, items[]}]}}`.
    pub(super) async fn shop_list(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ShopList>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let entity = match self.require_npc(seq, &cmd.entity_id).await {
            Ok(entity) => entity,
            Err(out) => return vec![out],
        };
        let Some(shop) = self.state.content().shops.get(&entity.id).cloned() else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет магазина")];
        };
        let discount_bp = match self.shop_discount_bp(&char_id, seq).await {
            Ok(bp) => bp,
            Err(out) => return vec![out],
        };
        let cfg = &self.state.config.gameplay.shop;
        let pool = &self.state.pool;

        // M24: надстройки/удаления магазина — снимок под блокировкой
        // до async-запросов (in-memory оверлей админ-панели).
        let (extra_slots, removed) = {
            let world = self.state.world.lock().expect("мир");
            let mut removed = std::collections::HashSet::new();
            for (tab, currency) in shop.currencies.iter().enumerate() {
                for idx in 0..currency.items.len() {
                    if world.shop_content_removed(&entity.id, tab, idx) {
                        removed.insert((tab, idx));
                    }
                }
            }
            (world.shop_extra(&entity.id), removed)
        };
        let mut tabs = Vec::new();
        for (tab_idx, currency) in shop.currencies.iter().enumerate() {
            let cur_def = match db::items::def(pool, &currency.currency).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    warn!("shop.list (валюта {}): нет определения", currency.currency);
                    continue;
                }
                Err(e) => {
                    warn!("shop.list (валюта): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let mut items = Vec::new();
            // M24: контент без удалённых админом + надстройка (таб0).
            let mut entries: Vec<String> = currency
                .items
                .iter()
                .enumerate()
                .filter(|(idx, _)| !removed.contains(&(tab_idx, *idx)))
                .map(|(_, id)| id.clone())
                .collect();
            if tab_idx == 0 {
                entries.extend(extra_slots.iter().flatten().cloned());
            }
            for item_id in entries {
                let Some(def) = (match db::items::def(pool, &item_id).await {
                    Ok(Some(def)) => Some(def),
                    Ok(None) => None,
                    Err(e) => {
                        warn!("shop.list (товар {item_id}): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }) else {
                    warn!("shop.list (товар {item_id}): нет определения");
                    continue;
                };
                let gold = crate::shops::buy_price_gold(
                    i64::from(def.gold_value),
                    cfg.server_markup,
                    shop.markup,
                    discount_bp,
                    cfg.max_discount_bp,
                );
                let unit = i64::from(def.qty).max(1);
                // M18/F40: полный снапшот на виртуальной строке —
                // карточка информации по клику на товар.
                let virtual_row = db::items::ItemRow {
                    uid: format!("shop:{}", def.id),
                    def_id: def.id.clone(),
                    qty: unit.clamp(1, i64::from(i32::MAX)) as i32,
                    upgrade: 0,
                    equipped_slot: None,
                    sockets: serde_json::json!([]),
                    container: "backpack".into(),
                    slot_index: None,
                    // M18/F47: карточка товара показывает бонусы (ADR-010).
                    bonuses: def.bonuses.clone(),
                };
                let item_value =
                    serde_json::to_value(item_snapshot(&virtual_row, &def)).unwrap_or_default();
                items.push(serde_json::json!({
                    "def_id": def.id,
                    "name": def.name,
                    "rarity": def.rarity,
                    "price": crate::shops::price_in_currency(gold, currency.rate),
                    "qty": unit,
                    "item": item_value,
                }));
            }
            tabs.push(serde_json::json!({
                "currency": currency.currency,
                "currency_name": cur_def.name,
                "items": items,
            }));
        }
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "shop": { "tabs": tabs } }),
        )]
    }

    /// `cmd/shop.buy` (M18/F39): покупка одной партии — списание
    /// валюты (события стаков, паттерн F29a) + выдача товара
    /// (стак → `evt/item/state`, новый стак → `evt/item/added`).
    /// ack `{}` — изменения уходят событиями.
    pub(super) async fn shop_buy(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ShopBuy>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let entity = match self.require_npc(seq, &cmd.entity_id).await {
            Ok(entity) => entity,
            Err(out) => return vec![out],
        };
        let Some(shop) = self.state.content().shops.get(&entity.id).cloned() else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет магазина")];
        };
        let Some(tab) = shop.currencies.iter().find(|c| c.currency == cmd.currency) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такой валюты")];
        };
        if !tab.items.contains(&cmd.def_id) {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такого товара")];
        }
        let pool = &self.state.pool;
        let currency_def = match db::items::def(pool, &cmd.currency).await {
            Ok(Some(def)) => def,
            _ => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "нет определения валюты",
                )];
            }
        };
        let item_def = match db::items::def(pool, &cmd.def_id).await {
            Ok(Some(def)) => def,
            _ => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "нет определения товара",
                )];
            }
        };
        let discount_bp = match self.shop_discount_bp(&char_id, seq).await {
            Ok(bp) => bp,
            Err(out) => return vec![out],
        };
        let cfg = &self.state.config.gameplay.shop;
        let gold = crate::shops::buy_price_gold(
            i64::from(item_def.gold_value),
            cfg.server_markup,
            shop.markup,
            discount_bp,
            cfg.max_discount_bp,
        );
        let price = crate::shops::price_in_currency(gold, tab.rate);

        // Лимит слотов (паритет socket.extract: полный рюкзак — отказ).
        if db::items::count(pool, &char_id).await.unwrap_or(i64::MAX)
            >= i64::from(self.state.config.gameplay.inventory.slots)
        {
            return vec![Self::err(
                seq,
                ErrorCode::InventoryFull,
                "инвентарь заполнен",
            )];
        }

        // R6: списание валюты и выдача товара — одной транзакцией;
        // ошибка выдачи откатывает списание (дюп/потеря невозможны).
        let (money_events, row, stacked) =
            {
                let mut tx = match pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        warn!("shop.buy (tx): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let changes =
                    match db::items::consume_by_def_tx(&mut tx, &char_id, &cmd.currency, price)
                        .await
                    {
                        Ok(Some(changes)) => changes,
                        Ok(None) => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::NotEnoughMoney,
                                format!("не хватает {}: {price}", currency_def.name),
                            )];
                        }
                        Err(e) => {
                            warn!("shop.buy (валюта): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                // Выдача товара (партия = qty стака из контента).
                let unit_qty = i64::from(item_def.qty).max(1);
                let (row, stacked) =
                    match db::items::grant_stacking_tx(&mut tx, &char_id, &cmd.def_id, unit_qty)
                        .await
                    {
                        Ok(pair) => pair,
                        Err(e) => {
                            warn!("shop.buy (выдача): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                if let Err(e) = tx.commit().await {
                    warn!("shop.buy (commit): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                (Self::money_events(changes), row, stacked)
            };

        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        outs.extend(money_events);
        if stacked {
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(row.qty));
            outs.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            let item = item_snapshot(&row, &item_def);
            outs.push(out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            ));
        }
        outs
    }
}
