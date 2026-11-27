//! Редьюсеры предметов, трупов и улучшения — вынесены из `apply_event` (RF4f).

use ld2_protocol::names;

use super::super::*;

impl AppStore {
    /// Предметы/трупы/улучшение: `evt/item/*`, `evt/corpse/*`, `evt/upgrade/result` (RF4f).
    pub(crate) fn reduce_items(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            m if m.starts_with(names::EVT_CORPSE_PREFIX) => self.reduce_corpses(m, payload),
            names::EVT_ITEM_ADDED
            | names::EVT_ITEM_REMOVED
            | names::EVT_ITEM_DROPPED
            | names::EVT_ITEM_EXPIRED => self.reduce_item_events(msg_type, payload),
            names::EVT_ITEM_STATE => self.reduce_item_state(msg_type, payload),
            names::EVT_UPGRADE_RESULT => self.reduce_upgrade(msg_type, payload),
            _ => vec![],
        }
    }

    /// Трупы: создание и разграбление.
    fn reduce_corpses(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_CORPSE_CREATED => {
                let Ok(corpse) = serde_json::from_value::<CorpseCreated>(payload) else {
                    return vec![];
                };
                // M23: лут/флаги трупа обновились — освежить окно
                // подятия, если оно открыто.
                self.pickup_dirty = true;
                let mut added = false;
                if let Some(snap) = self.snapshot.as_mut() {
                    match snap
                        .location
                        .entities
                        .iter()
                        .position(|e| e.id == corpse.entity.id)
                    {
                        // Обновление (M22: лут/флаги разделки) — замена.
                        Some(pos) => snap.location.entities[pos] = corpse.entity,
                        None => {
                            added = true;
                            snap.location.entities.push(corpse.entity);
                        }
                    }
                }
                if added {
                    self.push_log(LogKind::Loot, format!("Труп — лута: {}", corpse.loot_count));
                }
                vec![]
            }
            names::EVT_CORPSE_LOOTED => {
                let Ok(looted) = serde_json::from_value::<CorpseLooted>(payload) else {
                    return vec![];
                };
                if looted.was_player {
                    // ФР-11: лут с трупа игрока — статус «Мародёр».
                    self.push_log(
                        LogKind::Crime,
                        format!(
                            "{} собирает лут с трупа ИГРОКА — статус «Мародёр»",
                            looted.by_name
                        ),
                    );
                } else {
                    self.push_log(LogKind::Loot, format!("{} собирает лут", looted.by_name));
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Предметы: появление, убыль, выброс, истечение TTL.
    fn reduce_item_events(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_ITEM_ADDED => {
                let Ok(added) = serde_json::from_value::<ItemAdded>(payload) else {
                    return vec![];
                };
                // Лут приходит сервером уже добавленным — синхронизируем снапшот.
                if let Some(snap) = self.snapshot.as_mut() {
                    if !snap.char.inventory.iter().any(|i| i.uid == added.item.uid) {
                        snap.char.inventory.push(added.item.clone());
                    }
                }
                // Унификация (заказчик): «Вы получили {qty} {имя}[+N]»;
                // подбор (extra.picked) строку даёт despawn — без дубля.
                if added.item.extra.get("picked").and_then(|v| v.as_bool()) != Some(true) {
                    let qty = added.item.qty.unwrap_or(1).max(1);
                    // M39: отдельная строка только для камня из жилы
                    // (во время сбора); прочие источники raw_* — обычная.
                    let gathering = self.harvest.as_ref().is_some_and(|h| h.action == "gather");
                    if gathering && added.item.item_type.starts_with("raw_") {
                        // M39: найденный в жиле камень — отдельная строка
                        // (руда — обычная «Вы получили…»).
                        self.push_log(
                            LogKind::Loot,
                            format!("Найден самоцвет: {}", added.item.name),
                        );
                    } else {
                        let upg = if added.item.upgrade > 0 {
                            format!(" +{}", added.item.upgrade)
                        } else {
                            String::new()
                        };
                        self.push_log(
                            LogKind::Loot,
                            format!("Вы получили {qty} {}{upg}", added.item.name),
                        );
                    }
                }
                vec![]
            }
            names::EVT_ITEM_REMOVED => {
                let Ok(removed) = serde_json::from_value::<ItemRemoved>(payload) else {
                    return vec![];
                };
                // Название/qty/заточка — из инвентаря ДО удаления
                // (унификация журнала, заказчик 2026-11-21); надетые
                // тоже в инвентаре — один lookup покрывает всё.
                let removed_item = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.char.inventory.iter().find(|i| i.uid == removed.uid))
                    .map(|i| (i.name.clone(), i.qty.unwrap_or(1).max(1), i.upgrade))
                    .unwrap_or_else(|| (removed.uid.clone(), 1, 0));
                // ФР-06/ФР-07: убыль из инвентаря и из слотов экипировки.
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.inventory.retain(|i| i.uid != removed.uid);
                    snap.char
                        .equip
                        .retain(|_, slot| slot.as_deref() != Some(removed.uid.as_str()));
                }
                // M18/F54: активный камень выбыл — снимаем выбор и
                // гасим превью: оно было посчитано с этим камнем, повторный
                // старт без нового камня (`E_UPGRADE_PREVIEW_STALE`/`E_…`)
                // был бы неверным шагом.
                if self
                    .sharpen_stone
                    .as_ref()
                    .is_some_and(|st| st.uid == removed.uid)
                {
                    self.sharpen_stone = None;
                    if let Some(view) = self.upgrade.as_mut() {
                        view.preview = None;
                    }
                }
                // M20: при выбросе строку даёт `evt/item/dropped`.
                // Унификация (заказчик): «Вы {действие} {qty} {имя}[+N]» —
                // действие зависит от причины убыли.
                if !matches!(removed.reason, ItemRemovalReason::Dropped) {
                    let (rname, rqty, rupg) = removed_item;
                    let verb = match removed.reason {
                        ItemRemovalReason::Consumed => "использовали",
                        ItemRemovalReason::Traded => "обменяли",
                        _ => "потеряли",
                    };
                    let upg = if rupg > 0 {
                        format!(" +{rupg}")
                    } else {
                        String::new()
                    };
                    self.push_log(LogKind::Loot, format!("Вы {verb} {rqty} {rname}{upg}"));
                }
                vec![]
            }
            names::EVT_ITEM_DROPPED => {
                // M20: выброс предмета — журнал всем в локации; свой
                // char_id → «Вы выбросили…», иначе по полу «выбросил(а)».
                let Ok(dropped) = serde_json::from_value::<ItemDropped>(payload) else {
                    return vec![];
                };
                // Заказчик 2026-11-21: свои — «Вы выбросили…», чужие —
                // «{имя} выбросил(а) {qty} {предмет}».
                let mine = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.char.id == dropped.char_id);
                let qty = dropped.qty;
                let text = if mine {
                    format!("Вы выбросили {qty} {}", dropped.item_name)
                } else if dropped.gender == "female" {
                    format!("{} выбросила {qty} {}", dropped.by_name, dropped.item_name)
                } else {
                    format!("{} выбросил {qty} {}", dropped.by_name, dropped.item_name)
                };
                self.push_log(LogKind::Loot, text);
                vec![]
            }
            names::EVT_ITEM_EXPIRED => {
                // M20: предмет в локации исчез по TTL.
                let Ok(expired) = serde_json::from_value::<ItemExpired>(payload) else {
                    return vec![];
                };
                self.push_log(LogKind::Loot, format!("{} исчез", expired.name));
                vec![]
            }
            _ => vec![],
        }
    }

    /// Патч предмета (upgrade/sockets/qty).
    fn reduce_item_state(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_ITEM_STATE => {
                // Патч предмета (upgrade/sockets/qty) — ФР-09/ФР-10.
                #[derive(serde::Deserialize)]
                struct ItemPatch {
                    uid: String,
                    patch: serde_json::Map<String, serde_json::Value>,
                }
                let Ok(update) = serde_json::from_value::<ItemPatch>(payload) else {
                    return vec![];
                };
                let gathering = self.harvest.as_ref().is_some_and(|h| h.action == "gather");
                let mut gem_stack: Option<String> = None;
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(item) = snap.char.inventory.iter_mut().find(|i| i.uid == update.uid)
                    {
                        if let Some(full) = update.patch.get("item") {
                            if let Ok(replaced) = serde_json::from_value::<
                                ld2_protocol::types::ItemSnapshot,
                            >(full.clone())
                            {
                                *item = replaced;
                            }
                        } else if let Some(up) =
                            update.patch.get("upgrade").and_then(|v| v.as_i64())
                        {
                            item.upgrade = up as i32;
                        } else if let Some(sockets) = update.patch.get("sockets") {
                            if let Ok(sockets) = serde_json::from_value::<
                                Vec<ld2_protocol::types::Socket>,
                            >(sockets.clone())
                            {
                                // M24: прирост зарядов — такт тикера
                                // (или новая вставка) — фаза
                                // прогрессбара рун обновляется.
                                let old_charges: i32 = item
                                    .sockets
                                    .as_ref()
                                    .map(|old| {
                                        old.iter()
                                            .filter_map(|s| s.rune.as_ref().map(|r| r.charges))
                                            .sum()
                                    })
                                    .unwrap_or(0);
                                let new_charges: i32 = sockets
                                    .iter()
                                    .filter_map(|s| s.rune.as_ref().map(|r| r.charges))
                                    .sum();
                                item.sockets = Some(sockets);
                                if new_charges > old_charges {
                                    self.rune_recharge_phase_ms =
                                        Some(local_epoch_ms() + self.time_offset * 1000);
                                }
                            }
                        } else if let Some(qty) = update.patch.get("qty").and_then(|v| v.as_i64()) {
                            item.qty = Some(qty);
                            if gathering && item.item_type.starts_with("raw_") {
                                gem_stack = Some(item.name.clone());
                            }
                        }
                    }
                }
                if let Some(name) = gem_stack {
                    self.push_log(LogKind::Loot, format!("Найден самоцвет: {name}"));
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Результат улучшения — журнал и модалка.
    fn reduce_upgrade(&mut self, msg_type: &str, payload: serde_json::Value) -> Vec<Effect> {
        match msg_type {
            names::EVT_UPGRADE_RESULT => {
                // ФР-10: результат — событием; модалка закрывается.
                let Ok(result) =
                    serde_json::from_value::<ld2_protocol::evt::UpgradeResult>(payload.clone())
                else {
                    return vec![];
                };
                let name = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| {
                        s.char
                            .inventory
                            .iter()
                            .find(|i| i.uid == result.uid)
                            .map(|i| i.name.clone())
                    })
                    .unwrap_or_else(|| result.uid.clone());
                if result.ok {
                    let n = result.new_upgrade.unwrap_or_default();
                    self.push_log(LogKind::Loot, format!("Улучшение: «{name}» → +{n}"));
                } else if result.item_destroyed == Some(true) {
                    self.push_log(
                        LogKind::Crime,
                        format!("ПРОВАЛ улучшения — «{name}» РАЗРУШЕН"),
                    );
                } else {
                    self.push_log(
                        LogKind::Loot,
                        format!("Провал улучшения: «{name}» (без изменений)"),
                    );
                }
                // M18/F53: модалка не закрывается — сохраняем результат,
                // сбрасываем превью (следующее улучшение запросит новое).
                if let Some(view) = self.upgrade.as_mut() {
                    if view.item_uid == result.uid {
                        view.result = Some(payload.clone());
                        view.preview = None;
                    }
                }
                vec![Effect::Notify(NotifyKind::Upgrade)]
            }
            _ => vec![],
        }
    }
}
