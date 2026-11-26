//! RF1w: dev-сценарии (M13) — прыжок по локациям, набор предметов, выдача
//! предмета/золота и добавление розыска.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::envelope::Ack;
use ld2_protocol::evt::{CharXp, ItemAdded, ItemState, LocationChanged};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::{content_bonuses, item_snapshot, location_snapshot};
use super::{out_ack, out_event, Out, Session};
use crate::db;

impl Session {
    /// `/scenario death` (ФР-06): общий поток смерти (death.rs) — теряется
    /// **только лут**, опыт/пул сохраняются (skills.md §2.2); респаун в спавне.
    pub(super) async fn scenario(&mut self, scenario: &str, seq: u32) -> Vec<Out> {
        // `/scenario give <def_id>` — dev-выдача предмета (тесты M12.5+).
        if let Some(def_id) = scenario.strip_prefix("give ") {
            return self.scenario_give(def_id.trim(), seq).await;
        }
        // `/scenario dialog page|execute` — M20: открыть тестовый
        // игровой диалог (заготовка UI: многостраничный со страницами
        // и действиями / execute с полями ввода).
        if let Some(rest) = scenario.strip_prefix("dialog ") {
            use ld2_protocol::evt::{DialogAction, DialogInput, DialogOpen, DialogPage};
            let open = match rest.trim() {
                "page" => DialogOpen {
                    id: "dev_page".into(),
                    kind: "page".into(),
                    title: "Тестовый разговор".into(),
                    pages: vec![
                        DialogPage {
                            id: "p1".into(),
                            text: "Первая страница. «Дальше» — переход на вторую \
                                   (заготовка go:<page_id>), «Пока» — закрыть окно."
                                .into(),
                            actions: vec![
                                DialogAction {
                                    id: "go:p2".into(),
                                    label: "Дальше".into(),
                                },
                                DialogAction {
                                    id: "close".into(),
                                    label: "Пока".into(),
                                },
                            ],
                        },
                        DialogPage {
                            id: "p2".into(),
                            text: "Вторая страница — многостраничность работает. \
                                   Кнопка «Назад» возвращает на первую."
                                .into(),
                            actions: vec![DialogAction {
                                id: "close".into(),
                                label: "Завершить".into(),
                            }],
                        },
                    ],
                    inputs: vec![],
                },
                "execute" => DialogOpen {
                    id: "dev_execute".into(),
                    kind: "execute".into(),
                    title: "Наладка (execute)".into(),
                    pages: vec![],
                    inputs: vec![
                        DialogInput {
                            id: "who".into(),
                            label: "Кого настроить".into(),
                            kind: "text".into(),
                            default: String::new(),
                        },
                        DialogInput {
                            id: "count".into(),
                            label: "Количество".into(),
                            kind: "number".into(),
                            default: "1".into(),
                        },
                    ],
                },
                _ => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Internal,
                        "формат: dialog page|execute",
                    )];
                }
            };
            return vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    "evt/dialog/open",
                    None,
                    serde_json::to_value(open).unwrap_or_default(),
                ),
            ];
        }
        // `/scenario gold <n>` — dev-начисление золота (тесты M12.5c).
        if let Some(amount) = scenario.strip_prefix("gold ") {
            return self.scenario_gold(amount.trim(), seq).await;
        }
        // `/scenario crime <kind>` — dev-нарушение (тесты M12.5d).
        if let Some(kind) = scenario.strip_prefix("crime ") {
            return self.scenario_crime(kind.trim(), seq).await;
        }
        // `/scenario kit` — тестовый набор рюкзака (M16/F29b):
        // предметы разных типов/редкостей, улучшения, экипировка,
        // монеты.
        if scenario == "kit" {
            return self.scenario_kit(seq).await;
        }
        // `/scenario skillxp <skill_id> [xp]` — M18/F44: заполнить шкалу
        // навыка (по умолчанию ровно до 100 %) — смоук обучения у
        // тренера без боя (гейт 100 %, ADR-009).
        if let Some(rest) = scenario.strip_prefix("skillxp ") {
            let mut parts = rest.split_whitespace();
            let Some(skill_id) = parts.next().map(str::to_string) else {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "формат: skillxp <skill_id> [xp]",
                )];
            };
            let char_id = match self.require_char_id(seq) {
                Ok(id) => id,
                Err(out) => return vec![out],
            };
            let pool = &self.state.pool;
            let (xp_now, rank) = match db::skills::learned_one(pool, &char_id, &skill_id).await {
                Ok(Some(pair)) => pair,
                Ok(None) => (0, 0),
                Err(e) => {
                    warn!("scenario skillxp (прогресс): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let xp = match parts.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(xp) => xp.max(0),
                None => crate::systems::xp_to_next(rank.max(0) as u32, &self.state.config.skills),
            };
            let pool_left = match db::skills::xp_pool(pool, &char_id).await {
                Ok(p) => p,
                Err(e) => {
                    warn!("scenario skillxp (пул): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if let Err(e) =
                db::skills::invest_tx(pool, &char_id, &skill_id, xp, rank, pool_left).await
            {
                warn!("scenario skillxp: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let need = crate::systems::xp_to_next(rank.max(0) as u32, &self.state.config.skills);
            return vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    "evt/char/xp",
                    None,
                    serde_json::to_value(CharXp {
                        skill_id,
                        amount: xp - xp_now,
                        skill_xp: xp,
                        rank: Some(rank.max(0) as u32),
                        xp_to_next: Some(need),
                        pool_left: Some(pool_left),
                    })
                    .unwrap_or_default(),
                ),
            ];
        }
        // `/scenario bonus <uid> <kind> <id> <value> [pct]` — M18/F47:
        // добавить бонус инстансу (ADR-010) — смоук карточки и статов
        // без правки контента.
        if let Some(rest) = scenario.strip_prefix("bonus ") {
            let mut parts = rest.split_whitespace();
            let (Some(uid), Some(kind), Some(id), Some(value)) = (
                parts.next().map(str::to_string),
                parts.next().map(str::to_string),
                parts.next().map(str::to_string),
                parts.next().and_then(|v| v.parse::<i64>().ok()),
            ) else {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "формат: bonus <uid> <kind> <id> <value> [pct]",
                )];
            };
            if !matches!(kind.as_str(), "attr" | "param" | "skill") {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "kind должен быть attr | param | skill",
                )];
            }
            let pct = parts.next().is_some_and(|v| v == "pct");
            if pct && kind != "param" {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "pct допустим только для kind=param",
                )];
            }
            let char_id = match self.require_char_id(seq) {
                Ok(id) => id,
                Err(out) => return vec![out],
            };
            let pool = &self.state.pool;
            let Some(row) = (match db::items::find(pool, &uid, &char_id).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("scenario bonus (предмет): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }) else {
                return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
            };
            let mut list: Vec<serde_json::Value> = row
                .bonuses
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            list.push(serde_json::json!({
                "kind": kind, "id": id, "value": value, "pct": pct
            }));
            let bonuses = serde_json::Value::Array(list);
            if let Err(e) = db::items::set_bonuses(pool, &uid, bonuses.clone()).await {
                warn!("scenario bonus (запись): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            // Патч несёт эффективный список (контент + инстанс) — как
            // item_snapshot, иначе карточка потеряет контент-бонусы.
            let def = match db::items::def(pool, &row.def_id).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    return vec![Self::err(seq, ErrorCode::ItemGone, "нет определения")];
                }
                Err(e) => {
                    warn!("scenario bonus (определение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let mut effective: Vec<serde_json::Value> = content_bonuses(&def)
                .iter()
                .cloned()
                .map(|bonus| serde_json::to_value(bonus).unwrap_or_default())
                .collect();
            effective.extend(bonuses.as_array().cloned().unwrap_or_default());
            let mut patch = serde_json::Map::new();
            patch.insert("bonuses".into(), serde_json::Value::Array(effective));
            return vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    "evt/item/state",
                    None,
                    serde_json::to_value(ItemState {
                        uid: row.uid,
                        patch,
                    })
                    .unwrap_or_default(),
                ),
            ];
        }
        // `/scenario jump <location_id>` (M25): телепорт в любую
        // засеенную локацию — для обхода ад-карты (не связана с миром
        // выходами). Персист + мир + события — паритет `location.move`.
        if let Some(loc_id) = scenario.strip_prefix("jump ") {
            return self.scenario_jump(loc_id.trim(), seq).await;
        }
        if scenario != "death" {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("неизвестный сценарий: {scenario}"),
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let events = crate::death::kill_character(&self.state, &char_id).await;

        // Свежее состояние после респауна: ack со sv, сессия и мир синхронны.
        let mut outs = vec![out_ack(Ack::ok(seq, None))];
        if let Ok(Some(row)) = db::characters::get(&self.state.pool, &char_id).await {
            let state_version = row.state_version as u64;
            outs[0] = out_ack(Ack::ok(seq, Some(state_version)));
            let (location_id, _) = self.resolve_location(&row.location_id, &char_id).await;
            self.location_id = Some(location_id.clone());
            if let Ok(state) = self.character_state(&row).await {
                self.char = Some(state);
            }
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, &location_id);
            world.end_combat(&char_id);
        }
        for (addr, text) in events {
            if !text.is_empty() && addr == char_id {
                outs.push(Out::Text(text));
            }
        }
        outs
    }

    /// `/scenario jump <location_id>` (M25): телепорт в любую
    /// засеенную локацию — для обхода ад-карты (не связана с миром
    /// выходами). Персист + мир + события — паритет `location.move`.
    pub(super) async fn scenario_jump(&mut self, loc_id: &str, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::locations::get(&self.state.pool, loc_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                format!("локации {loc_id} нет в сиде"),
            )];
        }
        let state_version = match db::locations::move_char(&self.state.pool, &char_id, loc_id).await
        {
            Ok(v) => v as u64,
            Err(e) => {
                warn!("jump (персонаж): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let previous = self.location_id.clone().unwrap_or_default();
        self.location_id = Some(loc_id.to_string());
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, loc_id);
            world.end_combat(&char_id);
        }
        // Присутствие: телепорт = уход из старой и появление в новой.
        if previous != loc_id && !previous.is_empty() {
            let name = self
                .char
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_default();
            super::presence::announce_despawn(&self.state, &previous, &char_id, None);
            super::presence::announce_spawn(
                &self.state,
                loc_id,
                super::presence::player_entity(&char_id, &name),
                None,
            );
        }
        let location = match db::locations::get(&self.state.pool, loc_id).await {
            Ok(Some(row)) => self.location_with_world(row.snapshot()),
            Ok(None) => {
                warn!("jump: целевая локация {loc_id} отсутствует — заглушка");
                location_snapshot(loc_id)
            }
            Err(e) => {
                warn!("jump (чтение {loc_id}): {e}");
                location_snapshot(loc_id)
            }
        };
        let ack = match Ack::ok_with_payload(
            seq,
            Some(state_version),
            &serde_json::json!({ "location": location.clone() }),
        ) {
            Ok(ack) => out_ack(ack),
            Err(e) => Self::err(seq, ErrorCode::Internal, format!("encode: {e}")),
        };
        let changed = out_event(
            "evt/location/changed",
            Some(state_version),
            serde_json::to_value(LocationChanged { location }).unwrap_or_default(),
        );
        vec![ack, changed]
    }

    /// `/scenario give <def_id>` (dev): выдать предмет персонажу —
    /// сокеты посоха создаются сразу (ADR-002, иначе UI не увидит слоты).
    /// `/scenario kit` (M16/F29b): тестовый набор в рюкзак — предметы
    /// разных типов и редкостей с улучшениями +N, полная экипировка
    /// (слоты из конфига) и стак монет. Наполнение окна рюкзака для
    /// смоука; повторный вызов добавляет стаки (слоты переносятся).
    pub(super) async fn scenario_kit(&mut self, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];

        // (def_id, qty, улучшение +N, слот): экипировка — уникальные
        // инстансы, расходники/материалы — в существующие стаки.
        // M17/F30b: экипировка по новым слотам (12 из 15 — ring2 и
        // артефакты остаются пустыми для смоука).
        let grants: [(&str, i64, i32, Option<&str>); 18] = [
            ("sword_knight", 1, 3, Some("main_hand")),
            ("shield_wood", 1, 1, Some("off_hand")),
            ("helmet_leather", 1, 2, Some("head")),
            ("shoulders_leather", 1, 1, Some("shoulders")),
            ("bracers_leather", 1, 1, Some("bracers")),
            ("gloves_cloth", 1, 2, Some("gloves")),
            ("armor_bronze", 1, 4, Some("body")),
            ("greaves_iron", 1, 3, Some("legs")),
            ("boots_traveler", 1, 5, Some("feet")),
            ("cloak_traveler", 1, 2, Some("cloak")),
            ("ring_silver", 1, 1, Some("ring1")),
            ("amulet_talisman", 1, 1, Some("amulet")),
            ("staff_apprentice", 1, 1, None),
            ("potion_heal", 3, 0, None),
            ("rune_fire", 2, 0, None),
            ("scroll_frost", 1, 0, None),
            ("herb", 5, 0, None),
            ("tool_quill", 1, 0, None),
        ];
        for (def_id, qty, upgrade, slot) in grants {
            let Some(def) = db::items::def(pool, def_id).await.ok().flatten() else {
                warn!("scenario kit: нет определения {def_id} — пропуск");
                continue;
            };
            let mut row = if let Some(slot) = slot {
                // Слот занят — прежний предмет снимается (паритет
                // cmd/item.equip).
                if let Ok(Some(prev)) = db::items::find_equipped_in_slot(pool, &char_id, slot).await
                {
                    let _ = db::items::set_equipped(pool, &prev.uid, None).await;
                }
                let Ok(mut row) = db::items::insert(pool, &char_id, def_id, qty).await else {
                    warn!("scenario kit: insert {def_id} — пропуск");
                    continue;
                };
                if upgrade > 0 {
                    row.upgrade = upgrade;
                    let _ = db::items::set_upgrade(pool, &row.uid, upgrade).await;
                }
                let _ = db::items::set_equipped(pool, &row.uid, Some(slot)).await;
                row
            } else {
                match db::items::grant_stacking(pool, &char_id, def_id, qty).await {
                    Ok((row, _)) => row,
                    Err(e) => {
                        warn!("scenario kit (стак {def_id}): {e}");
                        continue;
                    }
                }
            };
            // Посох в рюкзаке тоже должен иметь пустые сокеты (окно
            // инкрустации открывается по предмету, без экипировки).
            if def.sockets > 0 && row.sockets.as_array().is_none_or(|a| a.is_empty()) {
                row.sockets = Self::empty_sockets(def.sockets);
                let _ = db::items::set_sockets(pool, &row.uid, row.sockets.clone()).await;
            }
            let item = item_snapshot(&row, &def);
            outs.push(out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            ));
        }

        // Монеты — стак (F29a): патч qty, если стак уже есть.
        match db::items::grant_stacking_by_type(pool, &char_id, db::items::MONEY_TYPE, 500).await {
            Ok(Some((row, stacked))) => {
                let ev = if stacked {
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(row.qty));
                    out_event(
                        "evt/item/state",
                        None,
                        serde_json::to_value(ItemState {
                            uid: row.uid,
                            patch,
                        })
                        .unwrap_or_default(),
                    )
                } else {
                    let def = db::items::def(pool, &row.def_id).await.ok().flatten();
                    match def {
                        Some(def) => out_event(
                            "evt/item/added",
                            None,
                            serde_json::to_value(ItemAdded {
                                item: item_snapshot(&row, &def),
                            })
                            .unwrap_or_default(),
                        ),
                        None => return outs,
                    }
                };
                outs.push(ev);
            }
            Ok(None) => warn!("scenario kit: валюта не найдена в контенте"),
            Err(e) => warn!("scenario kit (монеты): {e}"),
        }
        outs
    }

    pub(super) async fn scenario_give(&mut self, def_id: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let Some(def) = (match db::items::def(pool, def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("scenario give (определение {def_id}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::ItemGone,
                "нет такого определения",
            )];
        };
        if !db::items::has_space(
            pool,
            &char_id,
            i64::from(self.state.config.gameplay.inventory.slots),
        )
        .await
        .unwrap_or(false)
        {
            return vec![Self::err(
                seq,
                ErrorCode::InventoryFull,
                "инвентарь заполнен",
            )];
        }
        let mut row = match db::items::insert(pool, &char_id, def_id, i64::from(def.qty)).await {
            Ok(row) => row,
            Err(e) => {
                warn!("scenario give (insert): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if def.sockets > 0 {
            row.sockets = Self::empty_sockets(def.sockets);
            if let Err(e) = db::items::set_sockets(pool, &row.uid, row.sockets.clone()).await {
                warn!("scenario give (сокеты): {e}");
            }
        }
        let item = item_snapshot(&row, &def);
        vec![
            Self::ack_payload(seq, &serde_json::json!({})),
            out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            ),
        ]
    }

    /// `/scenario gold <n>` (dev): начислить золото (тесты M12.5c).
    pub(super) async fn scenario_gold(&mut self, amount: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Ok(n) = amount.parse::<i64>() else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректная сумма")];
        };
        // F29a: золото — стак монет (первое начисление создаёт стак).
        match db::items::grant_stacking_by_type(
            &self.state.pool,
            &char_id,
            db::items::MONEY_TYPE,
            n,
        )
        .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "валюта не найдена в контенте",
                )];
            }
            Err(e) => {
                warn!("scenario gold: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `/scenario crime <kind>` (dev): нарушение через общий поток §10
    /// (blend + статус + журнал), как при реальном триггере.
    pub(super) async fn scenario_crime(&mut self, kind: &str, seq: u32) -> Vec<Out> {
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some((_, text)) = self.apply_crime(&char_id, kind).await {
            outs.push(Out::Text(text));
        }
        outs
    }
}
