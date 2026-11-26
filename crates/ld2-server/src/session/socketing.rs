//! RF1k: камни и сокеты (M18) — вставка/извлечение камня с проверкой
//! совместимости приёмника и качеством, а также шанс заточки.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{SocketExtract, SocketInsert};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{ItemAdded, ItemRemovalReason, ItemRemoved, ItemState};
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use super::rules::{item_snapshot, rune_slots, socket_accepts};
use super::{out_event, refresh_char_runes, Out, Session};
use crate::db;

impl Session {
    /// `cmd/socket.insert` (§7, ADR-002): руна → свободный сокет посоха.
    /// ack `{staff}`; руна расходуется (`evt/item/removed`/qty), сокеты —
    /// патчем `evt/item/state`.
    pub(super) async fn socket_insert(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketInsert>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let Some(mut staff) = (match db::items::find(pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.insert (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.insert (определение посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // M17/F30a: сокеты принимает любой предмет с `sockets > 0`
        // (посохи и мечи равноправны); M25 — и каст с любого
        // предмета с сокетами (раньше только посох).
        if staff_def.sockets == 0 {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "у предмета нет сокетов",
            )];
        }
        let Some(rune) = (match db::items::find(pool, &cmd.rune_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.insert (поиск руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "руны нет")];
        };
        let Some(rune_def) = (match db::items::def(pool, &rune.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.insert (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        // M17/F30c: в сокет идут руны и самоцветы (броня).
        if !matches!(rune_def.item_type.as_str(), "rune" | "gem") {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "в сокет идут руны и самоцветы",
            )];
        }
        // M18/F58 (ADR-012): принимает слот — его `accepts`, а не тип
        // предмета: отсюда самоцветы в оружие (слоты со `accepts: gem`)
        // и отказ «руна в гемный слот». Правило сверяется после поиска
        // слота — см. ниже.
        // Ленивая инициализация: пустой массив → слоты по определению.
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if sockets.is_empty() {
            sockets = Self::empty_sockets_list(staff_def.sockets);
        }
        // Фаза проверок — immutable (занят/accepts/дубликат); запись
        // ниже переопределяет `slot` как mutable (M18/F58f).
        let Some(slot) = sockets.get(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        if slot.get("rune").is_some_and(|r| !r.is_null()) {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет уже занят")];
        }
        // M18/F58 (ADR-012): слот принимает только то, что указано в
        // `accepts`; поля в сырой записи может не быть — тогда считаем
        // по индексу и `rune_slots` предмета (тот же хелпер, что и в
        // снапшоте — клиент и сервер видят слот одинаково).
        let accepts = socket_accepts(slot, cmd.socket_index, rune_slots(&staff_def));
        if accepts != rune_def.item_type {
            let takes = match accepts.as_str() {
                "gem" => "самоцветы",
                _ => "руны",
            };
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                format!("слот принимает только {takes}"),
            )];
        }
        // M18/F58f: дубликаты рун не стакаются (указание заказчика) —
        // одна и та же руна не может стоять в двух слотах предмета;
        // гемы не ограничиваются. После accepts: «не тот слот» важнее
        // «дубликата».
        if rune_def.item_type == "rune"
            && sockets
                .iter()
                .any(|s| s.get("def_id").and_then(|v| v.as_str()) == Some(rune_def.id.as_str()))
        {
            return vec![Self::err(
                seq,
                ErrorCode::DuplicateRune,
                "эта руна уже вставлена в предмет",
            )];
        }
        // Индекс проверен выше — запись безопасна.
        let slot = &mut sockets[cmd.socket_index as usize];
        let charges = rune_def
            .stats
            .get("charges")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        let max = rune_def
            .stats
            .get("max")
            .and_then(|v| v.as_i64())
            .unwrap_or(charges);
        let spell = rune_def
            .stats
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // M17/F30a: damage руны пишется в сокет — прибавляется к урону
        // оружия (stats::derive) и попадает в снапшот.
        let damage = rune_def.stats.get("damage").and_then(|v| v.as_i64());
        // M18/F58f: настраиваемый знак руны — поле `glyph` её параметров
        // (латинские буквы, можно несколько — глифы Dwarven Runes);
        // без поля клиент фолбэчит хешем имени.
        let glyph = rune_def
            .stats
            .get("glyph")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut rune_obj = serde_json::json!({
            "name": rune_def.name,
            "spell": spell,
            "charges": charges,
            "max": max,
            "damage": damage,
        });
        if let Some(g) = glyph {
            rune_obj["glyph"] = serde_json::json!(g);
        }
        // M24: класс руны (stats.class) — в сокет, чтобы клиент гасил
        // кнопку каста не своему классу; нет поля — любой (§11).
        if let Some(class) = rune_def.stats.get("class").and_then(|v| v.as_str()) {
            rune_obj["class"] = serde_json::json!(class);
        }
        // M18/F58f: раритет вставленного — символ самоцвета в UI
        // зависит от него (предмет и слот; §8 — сервер-истина).
        rune_obj["rarity"] = serde_json::json!(rune_def.rarity);
        // M18/F58f: подпись занятого слота — что даёт вставка (урон/
        // шанс/бафф/эффект/трата; самоцвет — атрибут) — для окна
        // информации (§8: строки формирует сервер).
        let detail = crate::systems::slot_detail_lines(&rune_def.item_type, &rune_def.stats);
        if !detail.is_empty() {
            rune_obj["detail"] = serde_json::json!(detail);
        }
        *slot = serde_json::json!({
            "index": cmd.socket_index,
            "def_id": rune_def.id,
            "rune": rune_obj
        });
        // M24 фикс (указание заказчика): инкрустация — в ОДИН предмет.
        // Стак (qty>1) не заполняется целиком: целевая штука — новая
        // строка (паритет фикса заточки a996b53), старый стак убывает
        // на 1; сокеты пишутся уже в новую строку. Надетая вещь стаком
        // (админ-кейс) — без сплита, как раньше.
        let mut split: Option<(String, i64)> = None;
        if staff.equipped_slot.is_none() && staff.qty > 1 {
            let mut new_row = match db::items::insert(pool, &char_id, &staff.def_id, 1).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("socket.insert (новая строка): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if staff.upgrade > 0 {
                if let Err(e) = db::items::set_upgrade(pool, &new_row.uid, staff.upgrade).await {
                    warn!("socket.insert (уровень новой строки): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
                new_row.upgrade = staff.upgrade;
            }
            if let Some(bonuses) = staff.bonuses.clone() {
                if let Err(e) = db::items::set_bonuses(pool, &new_row.uid, bonuses).await {
                    warn!("socket.insert (бонусы новой строки): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            let new_qty = i64::from(staff.qty) - 1;
            if let Err(e) = db::items::set_qty(pool, &staff.uid, new_qty).await {
                warn!("socket.insert (убыль стака): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            split = Some((staff.uid.clone(), new_qty));
            staff = new_row;
        }
        let sockets_value = Value::Array(sockets);
        if let Err(e) = db::items::set_sockets(pool, &staff.uid, sockets_value).await {
            warn!("socket.insert (запись): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M18/F58d: кэш рун (charges/параметры) — пересборка.
        refresh_char_runes(&self.state, &char_id).await;

        // Руна расходуется (стак −1 либо удаление).
        let mut outs = Vec::new();
        if rune.qty > 1 {
            let new_qty = i64::from(rune.qty) - 1;
            if let Err(e) = db::items::set_qty(pool, &rune.uid, new_qty).await {
                warn!("socket.insert (qty руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            outs.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: rune.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            if let Err(e) = db::items::delete(pool, &rune.uid).await {
                warn!("socket.insert (удаление руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            outs.push(out_event(
                "evt/item/removed",
                None,
                serde_json::to_value(ItemRemoved {
                    uid: rune.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ));
        }

        // ack `{staff, char?}` (§7): посох с новыми сокетами; M18/F32:
        // цель надета → бонусы (самоцвет/урон руны) уже в силу — несём
        // свежий `char` (паритет с `item.equip`).
        let staff_row = match db::items::find(pool, &staff.uid, &char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => staff,
            Err(e) => {
                warn!("socket.insert (перечитывание посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // M24 фикс: события стака — новая строка (уже с сокетами) и
        // убыль старого стака; идут после событий руны, до ack клиента.
        if let Some((old_uid, new_qty)) = &split {
            outs.push(out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded {
                    item: item_snapshot(&staff_row, &staff_def),
                })
                .unwrap_or_default(),
            ));
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            outs.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: old_uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        }
        let mut payload = serde_json::Map::new();
        payload.insert(
            "staff".into(),
            serde_json::to_value(item_snapshot(&staff_row, &staff_def)).unwrap_or(Value::Null),
        );
        if staff_row.equipped_slot.is_some() {
            let char_state = match self.reload_char(seq, &char_id).await {
                Ok(state) => state,
                Err(out) => return vec![out],
            };
            payload.insert(
                "char".into(),
                serde_json::to_value(char_state).unwrap_or(Value::Null),
            );
        }
        // M32: самоцветы меняют атрибуты → пересчёт боевого кэша.
        self.refresh_char_hits_as(&char_id).await;
        let mut result = vec![Self::ack_payload(seq, &Value::Object(payload))];
        result.extend(outs);
        result
    }

    /// `cmd/socket.extract` (§7): извлечь руну → в инвентарь (заряды
    /// сбрасываются к максимуму — отклонение M12.5b: per-item charges нет).
    /// ack `{staff, rune}`.
    pub(super) async fn socket_extract(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<SocketExtract>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let Some(staff) = (match db::items::find(pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("socket.extract (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.extract (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if sockets.is_empty() {
            sockets = Self::empty_sockets_list(staff_def.sockets);
        }
        let Some(slot) = sockets.get_mut(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        let has_rune = slot.get("rune").is_some_and(|r| !r.is_null());
        let rune_def_id = slot
            .get("def_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        if !has_rune {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        }
        let Some(rune_def_id) = rune_def_id else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        };
        // M18 (указание заказчика): извлечённый самоцвет по
        // возможности идёт в существующий стак — слот не занимает,
        // лимит проверяем с учётом стака (take_loot_fits).
        match Self::take_loot_fits(
            pool,
            &char_id,
            &rune_def_id,
            self.state.config.gameplay.inventory.slots,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::InventoryFull,
                    "инвентарь заполнен",
                )];
            }
            Err(e) => {
                warn!("socket.extract (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        *slot = serde_json::json!({ "index": cmd.socket_index, "rune": null });
        if let Err(e) = db::items::set_sockets(pool, &staff.uid, Value::Array(sockets)).await {
            warn!("socket.extract (запись): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M18/F58d: кэш рун — пересборка ПОСЛЕ записи (слот очищен).
        refresh_char_runes(&self.state, &char_id).await;
        let Some(rune_def) = (match db::items::def(pool, &rune_def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("socket.extract (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        // M18 (указание заказчика): извлечённый **самоцвет** по
        // возможности складывается в существующий стак (простая
        // запись — состояние не теряется); руны — прежнее поведение.
        let rune_row = if rune_def.item_type == "gem" {
            match db::items::grant_stacking(pool, &char_id, &rune_def_id, 1).await {
                Ok((row, _stacked)) => row,
                Err(e) => {
                    warn!("socket.extract (стак самоцвета): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        } else {
            match db::items::insert(pool, &char_id, &rune_def_id, 1).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("socket.extract (возврат руны): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        };
        let staff_row = match db::items::find(pool, &staff.uid, &char_id).await {
            Ok(Some(row)) => row,
            _ => staff,
        };
        // M18/F32: цель надета → извлечение сразу меняет бонусы —
        // ack дополнен `char` (паритет с `socket.insert`).
        let mut payload = serde_json::Map::new();
        payload.insert(
            "staff".into(),
            serde_json::to_value(item_snapshot(&staff_row, &staff_def)).unwrap_or(Value::Null),
        );
        payload.insert(
            "rune".into(),
            serde_json::to_value(item_snapshot(&rune_row, &rune_def)).unwrap_or(Value::Null),
        );
        if staff_row.equipped_slot.is_some() {
            let char_state = match self.reload_char(seq, &char_id).await {
                Ok(state) => state,
                Err(out) => return vec![out],
            };
            payload.insert(
                "char".into(),
                serde_json::to_value(char_state).unwrap_or(Value::Null),
            );
        }
        // M32: извлечение самоцвета меняет атрибуты → кэш боя.
        self.refresh_char_hits_as(&char_id).await;
        vec![Self::ack_payload(seq, &Value::Object(payload))]
    }

    /// Пустые слоты посоха (форма хранения JSONB, ADR-002).
    pub(super) fn empty_sockets(count: i32) -> Value {
        Value::Array(Self::empty_sockets_list(count))
    }

    pub(super) fn empty_sockets_list(count: i32) -> Vec<Value> {
        (0..count.max(0))
            .map(|i| serde_json::json!({ "index": i, "rune": null }))
            .collect()
    }

    /// M18/F54: качество камня (`stats.quality`, процентные пункты;
    /// ключа нет → 0 — нейтральный «средний» камень).
    pub(super) fn stone_quality(def: &db::items::ItemDefRow) -> i32 {
        def.stats
            .get("quality")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32
    }

    /// M18/F54: итоговый шанс заточки — база конфига + бонус камня
    /// (затухание 1/(1+level)) × рейты сервера.
    pub(super) fn sharpen_chance(&self, base: f64, stone_quality: i32, level: i32) -> f64 {
        let rates = self.state.config.gameplay.rates.chance;
        crate::systems::upgrade_chance_with_stone(
            base,
            i64::from(stone_quality),
            i64::from(level),
            rates,
        )
    }
}
