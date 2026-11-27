//! RF1h: предметы (ФР-04/07, M16–M18) — перемещение в инвентаре,
//! использование (включая статический лут), экипировка/снятие по слотам
//! конфига (D25), выброс, подбор лута с трупа и подбор «по стакам».
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::{ItemDrop, ItemEquip, ItemMove, ItemUnequip, ItemUse};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{CharStats, ItemAdded, ItemRemovalReason, ItemRemoved, ItemState};
use ld2_protocol::types::{Entity, Requires};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::rules::{attr_genitive, entity_loot_defs, item_snapshot};
use super::{out_event, Out, Session};
use crate::db;

impl Session {
    /// `cmd/item.move` (M16/F29g): поменять местами два предмета
    /// рюкзака (перетаскивание) → ack `{}`; порядок — `items.sort_order`,
    /// клиент меняет сетку локально при отправке.
    pub(super) async fn item_move(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemMove>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // M28/B2: перемещение по контейнерам (рюкзак/сумки/сумка-uid).
        if let (Some(to_container), Some(to_index)) = (cmd.to_container.as_deref(), cmd.to_index) {
            let inv = &self.state.config.gameplay.inventory;
            return match db::items::place(
                &self.state.pool,
                &char_id,
                &cmd.item_uid,
                to_container,
                to_index,
                i64::from(inv.slots),
                i32::try_from(inv.bag_slots).unwrap_or(0),
            )
            .await
            {
                Ok(()) => vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({
                        "item_uid": cmd.item_uid,
                        "container": to_container,
                        "slot_index": to_index,
                    }),
                )],
                Err(e) => {
                    let (code, msg) = match e {
                        db::items::PlaceError::Gone => (ErrorCode::ItemGone, "предмета нет"),
                        db::items::PlaceError::Equipped => {
                            (ErrorCode::InventoryFull, "предмет надет — сначала снимите")
                        }
                        db::items::PlaceError::NoSuchBag => {
                            (ErrorCode::InventoryFull, "надетой сумки нет")
                        }
                        db::items::PlaceError::NotABag => {
                            (ErrorCode::InventoryFull, "в слот сумки кладут не сумку")
                        }
                        db::items::PlaceError::BagNotEmpty => {
                            (ErrorCode::InventoryFull, "сумка не пуста")
                        }
                        db::items::PlaceError::BagInBag => {
                            (ErrorCode::InventoryFull, "сумку в сумку класть нельзя")
                        }
                        db::items::PlaceError::OutOfRange => {
                            (ErrorCode::InventoryFull, "вне ёмкости контейнера")
                        }
                        db::items::PlaceError::BagBusy => {
                            (ErrorCode::InventoryFull, "слот сумки занят")
                        }
                        db::items::PlaceError::WrongType => (
                            ErrorCode::BagType,
                            "эта сумка не принимает такой тип предметов",
                        ),
                        db::items::PlaceError::WrongSlot => {
                            (ErrorCode::BagSlot, "сумка не для этого слота")
                        }
                        db::items::PlaceError::Internal => {
                            warn!("item.move: внутренняя ошибка контейнеров");
                            (ErrorCode::Internal, "внутренняя ошибка")
                        }
                    };
                    vec![Self::err(seq, code, msg)]
                }
            };
        }
        match db::items::swap_order(&self.state.pool, &char_id, &cmd.item_uid, &cmd.target_uid)
            .await
        {
            Ok(true) => vec![Self::ack_payload(seq, &serde_json::json!({}))],
            Ok(false) => vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")],
            Err(e) => {
                warn!("item.move: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// `cmd/item.use` (§7): взятие статического лута (`loot:*`) или расход
    /// зелья (heal из stats §6 → HP + убыль стака).
    pub(super) async fn item_use(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.item_uid.starts_with("loot:") || cmd.item_uid.starts_with("inner:") {
            return self.take_loot(seq, &char_id, &cmd.item_uid).await;
        }
        // M20/F20c: предмет на земле (id `gN`) — подбор любым игроком
        // в текущей локации (uid инвентаря `i_…` — коллизии нет).
        let ground = {
            let world = self.state.world.lock().expect("мир");
            world.ground_item(&cmd.item_uid).cloned()
        };
        if let Some(ground) = ground {
            return self.ground_pickup(seq, &char_id, ground).await;
        }
        let pool = &self.state.pool;
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.use (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(def) = (match db::items::def(pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.use (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // M18/F54: камень усиления — не расходник: `item.use` выбирает
        // активный камень, клиент открывает список предметов для заточки;
        // 1 шт. списывается сервером при `upgrade.start`.
        if def.item_type == "upgrade_stone" {
            if item.qty <= 0 {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    "камень усиления не найден",
                )];
            }
            self.sharpen_stone = Some(item.uid.clone());
            // Типизированный payload (ld2-protocol) — golden-тест фиксирует
            // форму ack; клиент читает `stone_uid`/`quality` (M18/F54).
            let selected = ld2_protocol::StoneSelected {
                stone_uid: item.uid,
                quality: i64::from(Self::stone_quality(&def)),
            };
            return vec![Self::ack_payload(
                seq,
                &serde_json::to_value(selected).expect("StoneSelected сериализуется"),
            )];
        }
        // M17/F30g: кулдаун предмета (cooldown_s, дефолт 1 с).
        if let Some(err) = self.item_cd_gate(seq, &char_id, &def).await {
            return vec![err];
        }
        // Зелье: heal из stats (открытый объект §6).
        if def.item_type == "potion" {
            if let Some(heal) = def.stats.get("heal").and_then(|v| v.as_i64()) {
                // R14: сначала расходуем зелье (ошибка списания — лечения
                // нет, повторное лечение исключено), затем атомарное
                // относительное лечение LEAST(hp_max, hp_cur + heal) —
                // гонка с тик-уроном не теряет ни урон, ни лечение.
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                if item.qty > 1 {
                    let new_qty = i64::from(item.qty) - 1;
                    if let Err(e) = db::items::set_qty(pool, &item.uid, new_qty).await {
                        warn!("item.use (qty): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    let mut patch = serde_json::Map::new();
                    patch.insert("qty".into(), serde_json::json!(new_qty));
                    outs.push(out_event(
                        names::EVT_ITEM_STATE,
                        None,
                        serde_json::to_value(ItemState {
                            uid: item.uid.clone(),
                            patch,
                        })
                        .unwrap_or_default(),
                    ));
                } else {
                    if let Err(e) = db::items::delete(pool, &item.uid).await {
                        warn!("item.use (удаление): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    outs.push(out_event(
                        names::EVT_ITEM_REMOVED,
                        None,
                        serde_json::to_value(ItemRemoved {
                            uid: item.uid.clone(),
                            reason: ItemRemovalReason::Consumed,
                        })
                        .unwrap_or_default(),
                    ));
                }
                let Some((hp_cur, hp_max)) = db::characters::heal_hp(pool, &char_id, heal)
                    .await
                    .ok()
                    .flatten()
                else {
                    warn!("item.use (hp): персонаж не найден");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                };
                let mut patch = serde_json::Map::new();
                patch.insert(
                    "hp".into(),
                    serde_json::json!({ "cur": hp_cur, "max": hp_max }),
                );
                outs.push(out_event(
                    names::EVT_CHAR_STATS,
                    None,
                    serde_json::to_value(CharStats { patch }).unwrap_or_default(),
                ));
                // M17/F30g: кулдаун использования → таймер кнопки.
                outs.push(self.item_cd_started(&char_id, &def));
                return outs;
            }
        }
        vec![Self::err(
            seq,
            ErrorCode::ItemGone,
            "предмет нельзя применить",
        )]
    }

    /// Взятие лута сущности: `loot:<сущность>:<idx>` — однократно
    /// (`loot_taken`, D27), лимит слотов — из конфига (D25).
    /// Снятие шкур (M22): взаимодействие с предметом во время
    /// активной разделки **прерывает** её — кожа испорчена
    /// (`ruined`, повтор невозможен, «Шкура испорчена» в журнале),
    /// освежевание просто отменяется (можно повторить). Событие
    /// чистит прогресс-бар клиента; сама команда предмета при этом
    /// выполняется как обычно.
    pub(super) async fn maybe_interrupt_harvest(&mut self, seq: u32) -> Vec<Out> {
        if self.require_auth(seq).is_some() {
            return Vec::new();
        }
        let Ok(char_id) = self.require_char_id(seq) else {
            return Vec::new(); // ошибку даст сам обработчик команды
        };
        let job = {
            let mut world = self.state.world.lock().expect("мир");
            world.cancel_harvest(&char_id)
        };
        let Some(job) = job else {
            return Vec::new(); // разделки не было
        };
        if job.action == "skin" {
            let mut world = self.state.world.lock().expect("мир");
            world.ruin_skin(&job.corpse_id);
        }
        // «Шкура испорчена» — только для снятия шкур; освежевание
        // просто отменяется (повтор разрешён).
        let ruined = job.action == "skin";
        let result = ld2_protocol::evt::HarvestResult {
            entity_id: job.corpse_id,
            name: job.name,
            action: job.action,
            ok: false,
            ruined,
        };
        let text = event_payload_text(names::EVT_HARVEST_RESULT, &result);
        text.map(|t| vec![Out::Text(t)]).unwrap_or_default()
    }

    pub(super) async fn take_loot(&mut self, seq: u32, char_id: &str, loot_uid: &str) -> Vec<Out> {
        // Снятие шкур: `inner:` — лут освежевания («Внутри»),
        // `loot:` — общий список (дроп/шкуры).
        let inner = loot_uid.starts_with("inner:");
        let Some(rest) = loot_uid.strip_prefix(if inner { "inner:" } else { "loot:" }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let Some((entity_id, idx)) = rest.rsplit_once(':') else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let Ok(idx) = idx.parse::<usize>() else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "некорректный uid лута")];
        };
        let pool = self.state.pool.clone();

        // Динамический труп (M12.5): слот в памяти; лимит — до изъятия.
        let dynamic = {
            let world = self.state.world.lock().expect(" мир");
            world.dynamic_corpse(entity_id).and_then(|c| {
                let slots = if inner { &c.inner_loot } else { &c.loot };
                slots.get(idx).cloned()
            })
        };
        if let Some(slot) = dynamic {
            let Some(def_id) = slot else {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")];
            };
            let Some(def) = (match db::items::def(&pool, &def_id).await {
                Ok(def) => def,
                Err(e) => {
                    warn!("take_loot (определение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }) else {
                return vec![Self::err(
                    seq,
                    ErrorCode::Internal,
                    "нет определения предмета",
                )];
            };
            // M18/F58f (новая фича): рюкзак вмещает **стаки** — простой
            // стак уже есть → место не нужно; иначе проверяем слот
            // ДО изъятия лута (иначе при полном лут бы пропал).
            match Self::take_loot_fits(
                &pool,
                char_id,
                &def_id,
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
                    warn!("take_loot (слоты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            // Изъятие из памяти: None — кто-то успел раньше (гонок нет —
            // обработка последовательна, но проверка честная).
            let took = {
                let mut world = self.state.world.lock().expect("мир");
                world.take_dynamic_slot(entity_id, idx, inner)
            };
            if took.is_none() {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")];
            }
            // M18/F58f: всё складывается в простой стак (монеты —
            // как раньше, F29a); слот занят только новой записью
            // (место проверено выше). Событие — патч qty либо added.
            let (row, stacked) = match db::items::grant_stacking(
                &pool,
                char_id,
                &def_id,
                i64::from(def.qty),
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!("take_loot (стак): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
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
                out_event(
                    names::EVT_ITEM_ADDED,
                    None,
                    serde_json::to_value(ItemAdded {
                        item: item_snapshot(&row, &def),
                    })
                    .unwrap_or_default(),
                )
            };
            return vec![Self::ack_payload(seq, &serde_json::json!({})), ev];
        }

        // Статический лут из сида (D26): однократно на персонажа.
        let current = self.current_location(char_id).await;
        let Some(loc_row) = (match db::locations::get(&pool, &current).await {
            Ok(row) => row,
            Err(e) => {
                warn!("take_loot (локация): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "персонаж не в мире")];
        };
        let entities: Vec<Entity> =
            serde_json::from_value(loc_row.entities.clone()).unwrap_or_default();
        let Some(entity) = entities.into_iter().find(|e| e.id == entity_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "сущность не найдена")];
        };
        let defs = entity_loot_defs(&entity);
        let base_len = defs.len();
        // M24: базовый слот (может быть удалён админом — tombstone)
        // либо надстройка сундука (админ-инъекция после базовых defs).
        let mut extra_qty: Option<i64> = None;
        let def_id = if let Some(def_id) = defs.get(idx).cloned() {
            let removed = {
                let world = self.state.world.lock().expect("мир");
                world.container_base_removed(&entity.id, idx)
            };
            if removed {
                return vec![Self::err(seq, ErrorCode::ItemGone, "предмет удалён")];
            }
            def_id
        } else if idx >= base_len {
            let slot = {
                let world = self.state.world.lock().expect("мир");
                world.container_extra_slot(&entity.id, idx - base_len)
            };
            match slot {
                Some((def_id, qty)) => {
                    extra_qty = Some(qty);
                    def_id
                }
                None => {
                    return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого лута")];
                }
            }
        } else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "нет такого лута")];
        };
        let Some(def) = (match db::items::def(&pool, &def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("take_loot (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // M18/F58f: лимит по стакам (простой стак — место не нужно).
        match Self::take_loot_fits(
            &pool,
            char_id,
            &def_id,
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
                warn!("take_loot (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        // M24: надстройка сундука несёт своё количество (стак
        // админа); базовый слот — количество из определения.
        let grant_qty = extra_qty.unwrap_or_else(|| i64::from(def.qty));
        match db::items::take_loot_tx(&pool, loot_uid, char_id, &def_id, grant_qty).await {
            Ok(None) => vec![Self::err(seq, ErrorCode::ItemGone, "уже собрано")],
            Ok(Some((row, stacked))) => {
                let item = item_snapshot(&row, &def);
                // M18/F58f: стак → патч qty (как у монет), новая запись → added.
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
                    out_event(
                        names::EVT_ITEM_ADDED,
                        None,
                        serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
                    )
                };
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({})), ev];
                // M20: у сущности указан respawn → лут вернётся через
                // min..max (очередь мира, сброс `loot_taken` на тике).
                {
                    let mut world = self.state.world.lock().expect("мир");
                    // M24: надстройка сундука — слот занят (in-memory;
                    // респавн сундука её не трогает).
                    if idx >= base_len {
                        world.container_extra_clear(entity_id, idx - base_len);
                    }
                    world.schedule_loot_respawn(entity_id);
                }
                // ФР-11: лут с трупа **игрока** → статус «Мародёр» (§10).
                if entity.was_player.unwrap_or(false) {
                    if let Some((_, text)) = self.apply_crime(char_id, "marauder").await {
                        outs.push(Out::Text(text));
                    }
                }
                outs
            }
            Err(e) => {
                warn!("take_loot (транзакция): {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// M18/F58f: поместится ли подбираемое в рюкзак — **по стакам**:
    /// «простой» однотипный стак есть → место не нужно (складываем);
    /// иначе — свободный слот. `false` — инвентарь заполнен (вызывающий
    /// возвращает `InventoryFull`), `Err` — ошибка БД.
    pub(super) async fn take_loot_fits(
        pool: &sqlx::PgPool,
        char_id: &str,
        def_id: &str,
        slots: u32,
    ) -> Result<bool, sqlx::Error> {
        if db::items::stackable_exists(pool, char_id, def_id).await? {
            return Ok(true);
        }
        // M28: ёмкость = база + надетые сумки; надетое место не занимает.
        db::items::has_space(pool, char_id, i64::from(slots)).await
    }

    /// `cmd/item.equip` (§7): слот — из конфига (D25), требования — из
    /// определения (§6 `requires`); занятый слот снимает прежний предмет.
    pub(super) async fn item_equip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemEquip>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if !self
            .state
            .config
            .gameplay
            .inventory
            .equip_slots
            .contains(&cmd.slot)
        {
            return vec![Self::err(seq, ErrorCode::ItemGone, "слот недоступен")];
        }
        let pool = self.state.pool.clone();
        let Some(item) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.equip (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        if item.equipped_slot.is_some() {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмет уже надет")];
        }
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.equip (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // Требования предмета (§6) → E_REQUIREMENT_ATTR.
        if let Some(requires) = def
            .requires
            .as_ref()
            .and_then(|v| serde_json::from_value::<Requires>(v.clone()).ok())
        {
            let attrs = self
                .char
                .as_ref()
                .map(|c| c.attrs.clone())
                .unwrap_or_default();
            for (attr, need) in &requires.attr {
                let have = attrs.get(attr).copied().unwrap_or(0);
                if have < *need {
                    return vec![Self::err(
                        seq,
                        ErrorCode::RequirementAttr,
                        format!("не хватает {}: {have} < {need}", attr_genitive(attr)),
                    )];
                }
            }
        }
        // M17/F30b: слот↔предмет (парные ring/artifact — по
        // псевдослоту); без слота предмет не экипируется.
        match def.slot.as_deref() {
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::ItemGone,
                    "предмет не экипируется",
                )];
            }
            Some(req) => {
                let ok = req == cmd.slot
                    || (req == "ring" && (cmd.slot == "ring1" || cmd.slot == "ring2"))
                    || (req == "artifact" && (cmd.slot == "artifact1" || cmd.slot == "artifact2"));
                if !ok {
                    return vec![Self::err(
                        seq,
                        ErrorCode::ItemGone,
                        format!("«{}» не надевается в этот слот", def.name),
                    )];
                }
            }
        }
        // M17/F30b: правила классов для оружия/щитов/посоха.
        if matches!(def.item_type.as_str(), "weapon" | "staff" | "shield") {
            let profile = self.char.as_ref().and_then(|c| c.profile.clone());
            let is_bow = def.skill.as_deref() == Some("bow");
            let gate = match profile.as_deref() {
                Some("mage") if def.item_type != "staff" => Some("маг носит только посох"),
                Some("archer") if !is_bow => Some("лучник носит только луки"),
                Some("warrior") if def.item_type == "staff" => Some("воин не носит посох"),
                Some("warrior") if is_bow => Some("воин не носит луки"),
                _ => None,
            };
            if let Some(msg) = gate {
                return vec![Self::err(seq, ErrorCode::ItemGone, msg)];
            }
        }
        // M17/F30b: левая рука закрыта, пока в правой двуручник.
        if cmd.slot == "off_hand" {
            if let Ok(Some(main)) =
                db::items::find_equipped_in_slot(&pool, &char_id, "main_hand").await
            {
                if let Ok(Some(main_def)) = db::items::def(&pool, &main.def_id).await {
                    if main_def.hands >= 2 {
                        return vec![Self::err(
                            seq,
                            ErrorCode::ItemGone,
                            "левая рука занята двуручным оружием",
                        )];
                    }
                }
            }
        }
        // Переэкипка: прежний предмет слота снимается (без события —
        // клиент получает полный `char` в ack, §7).
        match db::items::find_equipped_in_slot(&pool, &char_id, &cmd.slot).await {
            Ok(Some(prev)) => {
                if let Err(e) = db::items::set_equipped(&pool, &prev.uid, None).await {
                    warn!("item.equip (снятие прежнего): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!("item.equip (слот): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        if let Err(e) = db::items::set_equipped(&pool, &item.uid, Some(&cmd.slot)).await {
            warn!("item.equip (надевание): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M17/F30b: двуручник в правой руке блокирует левую — щит
        // автоматически снимается (как в WoW при смене на двуручное).
        if cmd.slot == "main_hand" && def.hands >= 2 {
            match db::items::find_equipped_in_slot(&pool, &char_id, "off_hand").await {
                Ok(Some(prev)) => {
                    if let Err(e) = db::items::set_equipped(&pool, &prev.uid, None).await {
                        warn!("item.equip (снятие левой руки): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("item.equip (левая рука): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }
        let char_state = match self.reload_char(seq, &char_id).await {
            Ok(state) => state,
            Err(out) => return vec![out],
        };
        let updated = match db::items::find(&pool, &item.uid, &char_id).await {
            Ok(Some(row)) => item_snapshot(&row, &def),
            _ => {
                return vec![Self::err(seq, ErrorCode::Internal, "предмет исчез")];
            }
        };
        // M17/F30f: оружие изменилось — пересчёт «чем бьёт».
        self.refresh_char_hits_as(&char_id).await;
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "char": char_state, "item": updated }),
        )]
    }

    /// `cmd/item.unequip` (§7): снять предмет слота → ack `{char, item}`.
    pub(super) async fn item_unequip(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemUnequip>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();
        let Some(item) = (match db::items::find_equipped_in_slot(&pool, &char_id, &cmd.slot).await {
            Ok(item) => item,
            Err(e) => {
                warn!("item.unequip (слот): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "в этом слоте пусто")];
        };
        if let Err(e) = db::items::set_equipped(&pool, &item.uid, None).await {
            warn!("item.unequip: {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let char_state = match self.reload_char(seq, &char_id).await {
            Ok(state) => state,
            Err(out) => return vec![out],
        };
        let Some(def) = (match db::items::def(&pool, &item.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("item.unequip (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения предмета",
            )];
        };
        // M17/F30f: оружие снято — пересчёт «чем бьёт».
        self.refresh_char_hits_as(&char_id).await;
        let updated = item_snapshot(&item, &def);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "char": char_state, "item": updated }),
        )]
    }

    pub(super) async fn item_drop(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ItemDrop>(env, seq) else {
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
                warn!("item.drop (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        // M28: непустую сумку выбрасывать нельзя (содержимое осиротеет).
        let is_bag = db::items::def(pool, &item.def_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|d| d.bag_slots.is_some());
        if is_bag {
            match db::items::bag_has_contents(pool, &item.uid).await {
                Ok(true) => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::InventoryFull,
                        "сумка не пуста — сначала выложите содержимое",
                    )];
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("item.drop (сумка): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }
        let qty = cmd.qty.unwrap_or(i64::from(item.qty));
        if qty <= 0 {
            return vec![Self::err(
                seq,
                ErrorCode::ItemGone,
                "некорректное количество",
            )];
        }
        // M20: персонаж в локации → предмет лежит на земле до
        // `drop_ttl_s` (подбираемый всеми), а не удаляется; не в мире —
        // прежнее поведение. Объявление — всем в локации («{by}
        // выбросил(а)…» / «Вы выбросили…» различает клиент по char_id).
        let ground_loc = self
            .state
            .world
            .lock()
            .expect("мир")
            .char_location(&char_id);
        if let Some(location_id) = &ground_loc {
            let def = match db::items::def(pool, &item.def_id).await {
                Ok(Some(def)) => def,
                Ok(None) => {
                    return vec![Self::err(seq, ErrorCode::Internal, "нет определения")];
                }
                Err(e) => {
                    warn!("item.drop (определение): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let ttl = self.state.config.world.drop_ttl_s;
            let expires_at = crate::time::now_epoch() + ttl.max(0);
            let (by_name, gender) = self
                .char
                .as_ref()
                .map(|c| (c.name.clone(), c.gender.clone()))
                .unwrap_or_else(|| ("Игрок".into(), "male".into()));
            // R-dnd: снапшот предмета для отображения на земле «как
            // в рюкзаке» (цвет редкости/+N/×qty/слоты) + raw-состояние
            // для восстановления при подборе.
            let mut snap = item_snapshot(&item, &def);
            snap.qty = Some(qty);
            let display = serde_json::to_value(&snap).unwrap_or_default();
            let dropped = {
                let mut world = self.state.world.lock().expect("мир");
                world.drop_ground(
                    location_id,
                    &def.name,
                    &item,
                    qty,
                    display,
                    &char_id,
                    expires_at,
                )
            };
            // R-dnd: предмет на земле виден СРАЗУ всем в локации
            // (evt/entity/spawn — паритет despawn при подборе; раньше
            // появлялся только после resync).
            if let Some(entity) = self.ground_entity_for(location_id, &dropped) {
                let spawn_text = event_payload_text(
                    names::EVT_ENTITY_SPAWN,
                    &ld2_protocol::evt::EntitySpawn {
                        entity,
                        location_id: location_id.clone(),
                        text: None,
                    },
                );
                let world = self.state.world.lock().expect("мир");
                for addr in world.chars_in_location(location_id) {
                    if let Some(text) = &spawn_text {
                        world.send_to_char(&addr, text.clone());
                    }
                }
            }
            let payload = ld2_protocol::evt::ItemDropped {
                char_id: char_id.clone(),
                by_name,
                gender,
                qty,
                item_name: def.name,
            };
            let dropped_text =
                crate::session::event_payload_text(names::EVT_ITEM_DROPPED, &payload);
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(location_id) {
                if let Some(text) = &dropped_text {
                    world.send_to_char(&addr, text.clone());
                }
            }
            let _ = dropped; // id наземного предмета — для будущего подбора
        }
        if qty >= i64::from(item.qty) {
            if let Err(e) = db::items::delete(pool, &item.uid).await {
                warn!("item.drop: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    names::EVT_ITEM_REMOVED,
                    None,
                    serde_json::to_value(ItemRemoved {
                        uid: item.uid.clone(),
                        reason: ItemRemovalReason::Dropped,
                    })
                    .unwrap_or_default(),
                ),
            ]
        } else {
            let new_qty = i64::from(item.qty) - qty;
            if let Err(e) = db::items::set_qty(pool, &item.uid, new_qty).await {
                warn!("item.drop (qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            vec![
                Self::ack_payload(seq, &serde_json::json!({})),
                out_event(
                    names::EVT_ITEM_STATE,
                    None,
                    serde_json::to_value(ItemState {
                        uid: item.uid.clone(),
                        patch,
                    })
                    .unwrap_or_default(),
                ),
            ]
        }
    }
}
