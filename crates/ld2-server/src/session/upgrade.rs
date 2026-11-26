//! RF1l: заточка (M18) — выбор камня, превью шанса и запуск усиления
//! предмета с расходом камня и бампом `upgrade`.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{UpgradePreview, UpgradeStart};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{ItemAdded, ItemRemovalReason, ItemRemoved, ItemState, UpgradeResult};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::item_snapshot;
use super::{out_event, Out, Session, UpgradePreviewState};
use crate::db;

impl Session {
    /// M18/F54: активный камень усиления — предмет (`upgrade_stone`)
    /// из инвентаря текущего персонажа; `None` — не выбран или потерян
    /// (выброшен/продан после выбора).
    pub(super) async fn active_stone(
        &self,
        stone_uid: &str,
        char_id: &str,
    ) -> Option<(db::items::ItemRow, db::items::ItemDefRow)> {
        let pool = &self.state.pool;
        let item = db::items::find(pool, stone_uid, char_id)
            .await
            .ok()
            .flatten()?;
        if item.qty <= 0 {
            return None;
        }
        let def = db::items::def(pool, &item.def_id).await.ok().flatten()?;
        if def.item_type != "upgrade_stone" {
            return None;
        }
        Some((item, def))
    }

    /// `cmd/upgrade.preview` (§7, ФР-10): превью «сейчас → после» —
    /// `preview_id`, `delta.upgrade`, `cost.gold`, `chance` (если
    /// `shows_chance`). `E_UPGRADE_MAX` — уровень уже предельный.
    pub(super) async fn upgrade_preview(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradePreview>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("upgrade.preview (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("upgrade.preview (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };

        let cfg = &self.state.config.gameplay.upgrade;
        let level = def.upgrade + item.upgrade;
        // Потолок уровня — раньше проверки камня: предмет вообще не
        // улучшается, камень тут ни при чём.
        if level >= cfg.max_level as i32 {
            return vec![Self::err(seq, ErrorCode::UpgradeMax, "предельный уровень")];
        }
        // M18/F54: улучшение требует выбранный камень усиления.
        let Some(stone_uid) = self.sharpen_stone.clone() else {
            return vec![Self::err(
                seq,
                ErrorCode::Ingredients,
                "нужен камень усиления",
            )];
        };
        let Some((_, stone_def)) = self.active_stone(&stone_uid, &char_id).await else {
            return vec![Self::err(
                seq,
                ErrorCode::Ingredients,
                "камень усиления не найден",
            )];
        };
        let stone_quality = Self::stone_quality(&stone_def);

        let next = level + 1;
        let cost = crate::systems::upgrade_cost(i64::from(level), cfg);
        let base = crate::systems::upgrade_chance(i64::from(level), cfg);
        let chance = self.sharpen_chance(base, stone_quality, level);
        let preview_id = format!("pv_{:x}", crate::time::now_ms());
        self.preview_state = Some(UpgradePreviewState {
            preview_id: preview_id.clone(),
            item_uid: item.uid.clone(),
            level,
            stone_quality,
            stone_uid,
        });

        let mut payload = serde_json::json!({
            "preview_id": preview_id,
            "delta": { "upgrade": format!("+{next}") },
            "cost": { "gold": cost },
        });
        if cfg.shows_chance {
            payload["chance"] = serde_json::json!(chance);
        }
        vec![Self::ack_payload(seq, &payload)]
    }

    /// `cmd/upgrade.start` (§7): золото списывается всегда, бросок — по
    /// шансу из конфига; провал — по `features."upgrade.rules"`
    /// (keep/level_down/break_on_fail). Итог — `evt/upgrade/result`
    /// (+ `evt/item/state`/`evt/item/removed`). `E_UPGRADE_PREVIEW_STALE` —
    /// превью не то/устарело.
    pub(super) async fn upgrade_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<UpgradeStart>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        // Превью: одно на соединение, uid + уровень должны совпасть.
        let preview = self.preview_state.clone();
        self.preview_state = None; // одноразово (§7: stale после старта)
        let Some(preview) = preview else {
            return vec![Self::err(seq, ErrorCode::UpgradePreviewStale, "нет превью")];
        };
        if preview.preview_id != cmd.preview_id || preview.item_uid != cmd.item_uid {
            return vec![Self::err(
                seq,
                ErrorCode::UpgradePreviewStale,
                "превью устарело",
            )];
        }

        let Some(item) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("upgrade.start (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("upgrade.start (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        let cfg = self.state.config.gameplay.upgrade.clone();
        let level = def.upgrade + item.upgrade;
        if level >= cfg.max_level as i32 {
            return vec![Self::err(seq, ErrorCode::UpgradeMax, "предельный уровень")];
        }
        if level != preview.level {
            // Уровень изменился после превью — бросать по свежим данным нельзя.
            return vec![Self::err(
                seq,
                ErrorCode::UpgradePreviewStale,
                "предмет изменился",
            )];
        }

        // M18/F54: камень от превью обязан существовать и совпадать
        // с активным (игрок мог выкинуть/сменить его между превью и стартом).
        let Some((stone_item, _)) = self.active_stone(&preview.stone_uid, &char_id).await else {
            return vec![Self::err(
                seq,
                ErrorCode::Ingredients,
                "камень усиления не найден",
            )];
        };
        if self.sharpen_stone.as_deref() != Some(preview.stone_uid.as_str()) {
            return vec![Self::err(
                seq,
                ErrorCode::Ingredients,
                "выбран другой камень усиления",
            )];
        }

        // Золото списывается всегда (до броска); F29a: монеты-стаки.
        let cost = crate::systems::upgrade_cost(i64::from(level), &cfg);
        let money_events = match db::items::consume_money(&pool, &char_id, cost).await {
            Ok(Some(changes)) => Self::money_events(changes),
            Ok(None) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает золота: {cost}"),
                )];
            }
            Err(e) => {
                warn!("upgrade.start (монеты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };

        // M18/F54: 1 шт. камня на улучшение — списываем до броска.
        let stone_events = if stone_item.qty > 1 {
            // Стак пережил расход — камень остаётся активным, игрок
            // может заточить следующий предмет тем же стаком.
            let new_qty = i64::from(stone_item.qty) - 1;
            if let Err(e) = db::items::set_qty(&pool, &stone_item.uid, new_qty).await {
                warn!("upgrade.start (камень qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            vec![out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: stone_item.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )]
        } else {
            if let Err(e) = db::items::delete(&pool, &stone_item.uid).await {
                warn!("upgrade.start (камень удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            // Последняя штука — выбор снимается.
            self.sharpen_stone = None;
            vec![out_event(
                "evt/item/removed",
                None,
                serde_json::to_value(ItemRemoved {
                    uid: stone_item.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            )]
        };

        // M18/F54: шанс — тот же, что показано в превью (база + бонус
        // камня × рейты сервера).
        let base = crate::systems::upgrade_chance(i64::from(level), &cfg);
        let chance = self.sharpen_chance(base, preview.stone_quality, level);
        // R7: CSPRNG — wall-clock-хэш позволял угадывать «удачные» мс
        // и обходить провалы (камень/золото/поломка предмета).
        let success = crate::world::roll_chance(chance);
        let rule = crate::systems::UpgradeFailRule::parse(
            self.state
                .config
                .gameplay
                .features
                .get("upgrade.rules")
                .and_then(|v| match v {
                    crate::config::FeatureValue::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("keep"),
        );
        let cost_json = serde_json::json!({ "gold": cost });

        let mut outs = Vec::new();
        // F29a: оплата монетами — события списания до результата.
        outs.extend(money_events);
        // M18/F54: списанный камень усиления.
        outs.extend(stone_events);
        if success {
            let new_level = level + 1;
            // ФИКС (указание заказчика): улучшение — НОВЫЙ предмет.
            // Стак (qty>1) не улучшается целиком: старый −1 шт.,
            // улучшенная штука — отдельная строка (uid новый).
            // Порядок событий как в оригинале: итог первым, затем
            // патчи (иначе wait(evt/upgrade/result) съедает патчи).
            let (result_uid, tail) = if item.qty > 1 {
                let mut new_row = match db::items::insert(&pool, &char_id, &item.def_id, 1).await {
                    Ok(row) => row,
                    Err(e) => {
                        warn!("upgrade.start (новая строка): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                if item.sockets.as_array().is_some_and(|a| !a.is_empty()) {
                    if let Err(e) =
                        db::items::set_sockets(&pool, &new_row.uid, item.sockets.clone()).await
                    {
                        warn!("upgrade.start (сокеты новой строки): {e}");
                    }
                }
                if let Some(bonuses) = item.bonuses.clone() {
                    if let Err(e) = db::items::set_bonuses(&pool, &new_row.uid, bonuses).await {
                        warn!("upgrade.start (бонусы новой строки): {e}");
                    }
                }
                if let Err(e) = db::items::set_upgrade(&pool, &new_row.uid, item.upgrade + 1).await
                {
                    warn!("upgrade.start (запись): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                new_row.upgrade = item.upgrade + 1;
                let new_qty = i64::from(item.qty) - 1;
                if let Err(e) = db::items::set_qty(&pool, &item.uid, new_qty).await {
                    warn!("upgrade.start (убыль стака): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                // События (после итога): новый предмет + убыль стака.
                let mut events = vec![out_event(
                    "evt/item/added",
                    None,
                    serde_json::to_value(ItemAdded {
                        item: item_snapshot(&new_row, &def),
                    })
                    .unwrap_or_default(),
                )];
                let mut patch = serde_json::Map::new();
                patch.insert("qty".into(), serde_json::json!(new_qty));
                events.push(out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                ));
                (new_row.uid, events)
            } else {
                if let Err(e) = db::items::set_upgrade(&pool, &item.uid, item.upgrade + 1).await {
                    warn!("upgrade.start (запись): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                let mut patch = serde_json::Map::new();
                patch.insert("upgrade".into(), serde_json::json!(new_level));
                let event = out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                );
                (item.uid.clone(), vec![event])
            };
            outs.push(out_event(
                "evt/upgrade/result",
                None,
                serde_json::to_value(UpgradeResult {
                    uid: result_uid,
                    ok: true,
                    new_upgrade: Some(new_level as u32),
                    item_destroyed: Some(false),
                    cost: serde_json::from_value(cost_json.clone()).unwrap_or_default(),
                    duration_ms: Some((cfg.duration_s * 1000.0).round() as i64),
                })
                .unwrap_or_default(),
            ));
            outs.extend(tail);
        } else {
            let (new_upgrade, destroyed) = match rule {
                crate::systems::UpgradeFailRule::Keep => (level, false),
                crate::systems::UpgradeFailRule::LevelDown => ((level - 1).max(0), false),
                crate::systems::UpgradeFailRule::Break => (level, true),
            };
            if destroyed {
                if item.qty > 1 {
                    // ФИКС: стак не удаляется целиком — гибнет одна штука.
                    let new_qty = i64::from(item.qty) - 1;
                    if let Err(e) = db::items::set_qty(&pool, &item.uid, new_qty).await {
                        warn!("upgrade.start (разрушение штуки): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(new_qty));
                    outs.push(out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState {
                            uid: item.uid.clone(),
                            patch,
                        })
                        .unwrap_or_default(),
                    ));
                } else {
                    if let Err(e) = db::items::delete(&pool, &item.uid).await {
                        warn!("upgrade.start (разрушение): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    outs.push(out_event(
                        "evt/item/removed",
                        None,
                        serde_json::to_value(ItemRemoved {
                            uid: item.uid.clone(),
                            reason: ItemRemovalReason::Consumed,
                        })
                        .unwrap_or_default(),
                    ));
                }
            } else if new_upgrade != level && item.qty == 1 {
                if let Err(e) =
                    db::items::set_upgrade(&pool, &item.uid, (new_upgrade - def.upgrade).max(0))
                        .await
                {
                    warn!("upgrade.start (понижение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                let mut patch = serde_json::Map::new();
                patch.insert("upgrade".into(), serde_json::json!(new_upgrade));
                outs.push(out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                ));
            }
            let result = UpgradeResult {
                uid: item.uid.clone(),
                ok: false,
                // ФИКС: стак (qty>1) не разрушается целиком и не
                // меняет уровень — итог без понижения/уничтожения.
                new_upgrade: if item.qty > 1 {
                    Some(level as u32)
                } else if destroyed {
                    None
                } else {
                    Some(new_upgrade as u32)
                },
                item_destroyed: Some(destroyed && item.qty <= 1),
                cost: serde_json::from_value(cost_json).unwrap_or_default(),
                duration_ms: Some((cfg.duration_s * 1000.0).round() as i64),
            };
            outs.insert(
                0,
                out_event(
                    "evt/upgrade/result",
                    None,
                    serde_json::to_value(result).unwrap_or_default(),
                ),
            );
        }

        // M32: +N меняет урон/статы оружия → кэш боя.
        self.refresh_char_hits_as(&char_id).await;
        // ack → события (§2.2).
        let mut result = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        result.extend(outs);
        result
    }
}
