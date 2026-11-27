//! Диспетчеризация кликов интерфейса. `on_click` разложен на группы
//! по экранам/доменам, чтобы каждая функция была не длиннее 200 строк.
//! Вынесено из `root.rs` (RF3h).

use super::*;

impl Root {
    pub(crate) fn on_click(&mut self, click: Click, cx: &mut Context<'_, Self>) {
        if self.click_nav(click.clone(), cx) {
            return;
        }
        if self.click_settings(click.clone(), cx) {
            return;
        }
        if self.click_combat(click.clone(), cx) {
            return;
        }
        if self.click_loot(click.clone(), cx) {
            return;
        }
        if self.click_social(click.clone(), cx) {
            return;
        }
        let _ = self.click_admin(click, cx);
    }

    /// Обработчик кликов: nav.
    fn click_nav(&mut self, click: Click, cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::Login => self.send_login(cx),
            Click::OpenRegister => {
                self.register_open = true;
                self.register_submitted = false;
                self.store.register_error = None;
            }
            Click::Logout => self.open_exit(ExitKind::Logout, cx),
            Click::Lang(code) => {
                self.lang = Lang::from_code(code);
                self.config.lang = self.lang.code().to_string();
                // Журнал переводит каналы по языку (заказчик 2026-11-20).
                self.store.lang = self.lang;
                self.save_config();
            }
            Click::ToggleSettings => {
                self.settings_open = !self.settings_open;
                if !self.settings_open {
                    self.color_picker_open = None;
                }
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host.clone(), cx));
                }
                let domain = self.config.domain.clone().unwrap_or_default();
                if let Some(input) = self.domain_input.clone() {
                    input.update(cx, |i, cx| i.set_text(domain.clone(), cx));
                }
            }
            Click::ToggleCharacter => {
                if self.char_popup.open {
                    self.char_popup.open = false;
                } else {
                    // M16/F29d: базовый размер + первая вкладка.
                    self.open_popup(PopupKind::Character);
                }
                cx.notify();
            }
            // M16/F29b: окно рюкзака (открывается вместе с персонажем).
            Click::ToggleInventory => {
                if self.backpack_popup.open {
                    self.backpack_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Backpack);
                }
                cx.notify();
            }
            // M18/F36: окно квестов (текущие/выполненные).
            Click::ToggleQuests => {
                if self.quests_popup.open {
                    self.quests_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Quests);
                }
                cx.notify();
            }
            Click::ToggleProfessions => {
                if self.professions_popup.open {
                    self.professions_popup.open = false;
                } else {
                    self.open_popup(PopupKind::Professions);
                }
                self.prof_reset_armed = None;
                cx.notify();
            }
            Click::MoveExit(exit_id) => {
                let cmd = LocationMove { exit_id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Move)
                }
            }
            Click::OpenReleases => cx.open_url(ld2_core::update_check::RELEASES_PAGE),
            // ── M10.4: игровой экран ──
            Click::SwitchChar => self.open_exit(ExitKind::SwitchChar, cx),
            _ => return false,
        }
        true
    }

    /// Обработчик кликов: settings.
    fn click_settings(&mut self, click: Click, cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::Theme(theme) => {
                self.config.theme = theme.to_string();
                // M11.5: тема Ely переключается вместе с config (ФР-19).
                Theme::set_mode_now(
                    if theme == "light" {
                        Mode::Light
                    } else {
                        Mode::Dark
                    },
                    cx,
                );
                self.save_config();
            }
            Click::ChatLimit(delta) => {
                let limit = (self.config.chat_limit as i64 + delta).clamp(100, 10_000) as usize;
                self.config.chat_limit = limit;
                self.store.chat_limit = limit;
                self.save_config();
            }
            Click::SetNotify(notify) => {
                self.config.notify = notify;
                self.save_config();
            }
            Click::SetMuteFilter(on) => {
                self.config.chat_mute_filter = on;
                self.save_config();
            }
            Click::SetBackpackShowAllBags(on) => {
                self.config.backpack_show_all_bags = on;
                self.save_config();
            }
            Click::EditHost => {
                // Предзаполнение текущим адресом (как при открытии
                // настроек) и раскрытие ввода.
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host, cx));
                }
                let domain = self.config.domain.clone().unwrap_or_default();
                if let Some(input) = self.domain_input.clone() {
                    input.update(cx, |i, cx| i.set_text(domain, cx));
                }
                self.host_editing = true;
            }
            Click::TogglePicker(key) => {
                self.toggle_color_picker(key);
            }
            Click::LogDamageBold(bold) => {
                self.config.log_damage_bold = bold;
                self.save_config();
            }
            Click::ApplyHost => {
                let host = self.ti(&self.host_input).read(cx).text().to_string();
                let host = host.trim().to_string();
                if !host_allowed(&host, RELEASE) {
                    self.store.notice = Some(t(self.lang, Key::SettingsWsDenied).to_string());
                    return true;
                }
                // M19 (§15): домен discovery — пусто = прямой режим;
                // пробелы внутри — некорректный ввод (отказ до сохранения).
                let domain_raw = self.ti(&self.domain_input).read(cx).text().to_string();
                let domain_raw = domain_raw.trim().to_string();
                if !domain_raw.is_empty() && domain_raw.contains(char::is_whitespace) {
                    self.store.notice = Some(t(self.lang, Key::DiscBadDoc).to_string());
                    return true;
                }
                let domain = (!domain_raw.is_empty()).then(|| domain_raw.clone());
                let changed = host != self.config.host || domain != self.config.domain;
                if changed {
                    self.config.host = host;
                    self.config.domain = domain;
                    self.save_config();
                    self.reconnect();
                }
                self.host_editing = false;
                self.settings_open = false;
            }
            _ => return false,
        }
        true
    }

    /// Обработчик кликов: combat.
    fn click_combat(&mut self, click: Click, _cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::Attack(id) => {
                let cmd = ld2_protocol::cmd::CombatAttack { target_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Attack);
                    // M14/F4: любое боевое действие → глобальный кулдаун.
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Attack);
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            Click::Examine(id) => {
                let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Examine)
                }
            }
            // M35: ПКМ-осмотр игрока — всплывающая карточка.
            Click::InspectPlayer(id) => self.toggle_inspect(&id, _cx),
            // M23: ЛКМ по трупу — окно подятия (лут слева, рюкзак
            // справа); данные — отдельный `entity.info` → `pickup`.
            Click::PowerStrike => {
                let cmd = ld2_protocol::cmd::SkillUse {
                    skill_id: "power_strike".into(),
                    target_id: self.store.target.clone(),
                };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Cast);
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            _ => return false,
        }
        true
    }

    /// Обработчик кликов: loot.
    fn click_loot(&mut self, click: Click, cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::LootOpen(id) => {
                self.pickup_id = Some(id);
                self.pickup_sel = None;
                // Лут прошлого трупа не должен мелькнуть под новым.
                self.store.pickup = None;
                // Без флага pump не отправит запрос, а рендер с пустыми
                // данными закроет окно в первом же кадре (баг M23).
                self.pickup_request = true;
                self.open_popup(PopupKind::LootPickup);
                self.pump_pickup(cx);
            }
            // M18/F35: диалог NPC — реплики/квесты вместо осмотра.
            Click::UseItem(uid) => {
                // M14/F4: расходник боя (свиток/зелье) запускает
                // глобальный кулдаун; лут и прочие предметы — нет.
                // M18/F54: камень усиления — отдельный ack (Act::Sharpen).
                let stone = self.store.snapshot.as_ref().and_then(|s| {
                    s.char
                        .inventory
                        .iter()
                        .find(|i| i.uid == uid && i.item_type == "upgrade_stone")
                        .map(|i| i.uid.clone())
                });
                let combat_item = self.store.snapshot.as_ref().is_some_and(|s| {
                    s.char.inventory.iter().any(|i| {
                        i.uid == uid && matches!(i.item_type.as_str(), "scroll" | "potion")
                    })
                });
                // M23: взятие из окна подятия (`loot:`/`inner:`) —
                // перезапрашиваем его лут, когда транспорт освободится.
                let is_loot = uid.starts_with("loot:") || uid.starts_with("inner:");
                let cmd = ld2_protocol::cmd::ItemUse {
                    item_uid: uid,
                    target_id: None,
                };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(if stone.is_some() {
                        Act::Sharpen
                    } else {
                        Act::Drop
                    });
                    if is_loot && self.pickup_popup.open {
                        self.pickup_request = true;
                    }
                    if combat_item {
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    if stone.is_some() {
                        // M18/F54: геометрия карточки; гейт рендера —
                        // sharpen_stone, которое придёт в ack.
                        self.open_popup(PopupKind::Upgrade);
                    }
                    self.store.entity_detail = None;
                    self.store.clear_loot_confirm();
                }
            }
            Click::ConfirmDeath => self.store.confirm_death(),
            Click::LootTakeMarker(uid, name) => self.store.set_loot_confirm(uid, name),
            // M23: «Взять всё» — партия в очередь; труп игрока —
            // одно подтверждение статуса на всю партию (ФР-11).
            Click::LootTakeAll => {
                let Some(detail) = self.store.pickup.clone() else {
                    return true;
                };
                let uids = pickup::pickup_uids(&detail);
                if uids.is_empty() {
                    return true;
                }
                if detail["was_player"] == true {
                    self.pickup_confirm_all = true;
                    let name = detail["entity"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    self.store.set_loot_confirm(String::new(), name);
                } else {
                    self.pickup_queue = uids;
                    self.pump_take_queue(cx);
                }
            }
            Click::LootTake => {
                if self.pickup_confirm_all {
                    // Подтверждение партии на трупе игрока (ФР-11).
                    self.pickup_confirm_all = false;
                    self.store.clear_loot_confirm();
                    self.pickup_queue = self
                        .store
                        .pickup
                        .as_ref()
                        .map(pickup::pickup_uids)
                        .unwrap_or_default();
                    self.pump_take_queue(cx);
                } else if let Some((uid, _)) = self.store.loot_confirm.clone() {
                    if !uid.is_empty() {
                        self.on_click(Click::UseItem(uid), cx);
                    }
                }
            }
            Click::LootCancel => {
                self.pickup_confirm_all = false;
                self.store.clear_loot_confirm();
            }
            Click::CloseDetail => self.store.entity_detail = None,
            _ => return false,
        }
        true
    }

    /// Обработчик кликов: social.
    fn click_social(&mut self, click: Click, _cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::NpcTalk(id) => {
                tracing::debug!("вых. cmd/npc.talk {id}");
                let cmd = ld2_protocol::cmd::NpcTalk { entity_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::NpcTalk);
                } else {
                    tracing::debug!("cmd/npc.talk не ушла: нет связи");
                }
            }
            Click::PartyInvite(player_id) => {
                let cmd = ld2_protocol::cmd::PartyInvite { player_id };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_sheet_sent(Act::Party)
                }
            }
            Click::TradeOpen(player_id) => {
                let cmd = ld2_protocol::cmd::TradeOpen { player_id };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_sheet_sent(Act::Trade)
                }
            }
            Click::RuneCast {
                staff_uid,
                socket_index,
            } => {
                let cmd = ld2_protocol::cmd::MagicRuneCast {
                    staff_uid,
                    socket_index,
                    target_id: self.store.target.clone(),
                };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Cast);
                    self.store.begin_global_cooldown(self.now_ms());
                }
            }
            Click::Gather(id) => {
                let cmd = ld2_protocol::cmd::ResourceGather { entity_id: id };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Gather)
                }
            }
            Click::Harvest(id, action) => {
                // Имя животного — из трупа снапшота (без «Труп:»).
                let name = self
                    .store
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.location.entities.iter().find(|e| e.id == id))
                    .map(|e| e.name.strip_prefix("Труп: ").unwrap_or(&e.name).to_string())
                    .unwrap_or_default();
                let cmd = ld2_protocol::cmd::CorpseHarvest {
                    entity_id: id.clone(),
                    action: action.clone(),
                };
                if self.send_or_notice(&cmd, SendSlot::World) {
                    self.store.on_world_sent(Act::Harvest);
                    // Прогресс-бар — с момента отправки (5 с);
                    // отказ сервера чистит Act::Harvest (ошибка).
                    // entity_id (M24): итог другого трупа бар не гасит.
                    let label = format!(
                        "{} · {}",
                        if action == "skin" {
                            t(self.lang, Key::SkinBtn)
                        } else {
                            t(self.lang, Key::GutBtn)
                        },
                        name
                    );
                    self.store.harvest = Some(ld2_core::store::HarvestProgress {
                        started_ms: self.now_ms(),
                        action,
                        name,
                        entity_id: id,
                    });
                    // M24: единый прогресс-бар (сервер-джоб5 с).
                    self.start_progress(ProgressSource::Harvest, label, 5_000);
                }
            }
            // M24: админ-панель — роль гейтит сервер; кнопка в шапке
            // рисуется только для admin.
            _ => return false,
        }
        true
    }

    /// Обработчик кликов: admin.
    fn click_admin(&mut self, click: Click, cx: &mut Context<'_, Self>) -> bool {
        match click {
            Click::AdminOpen => {
                self.open_popup(PopupKind::AdminPanel);
                self.admin_catalog_request = true;
                self.pump_admin_catalog(cx);
            }
            Click::AdminGive(def_id) => {
                let cmd = ld2_protocol::cmd::AdminGiveAsk {
                    target: self.admin_target.clone(),
                    def_id,
                };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminAsk)
                }
            }
            Click::AdminRemove(item_ref) => {
                let cmd = ld2_protocol::cmd::AdminRemoveAsk {
                    target: self.admin_target.clone(),
                    item_ref,
                };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminAsk)
                }
            }
            Click::AdminTarget(target) => {
                self.admin_target = target;
                self.store.admin_target_items = None;
                self.admin_target_request = true;
                self.pump_admin_target(cx);
            }
            // M24: очистка цели — свои предметы уходят событиями
            // снапшота, чужие (не-self) перечитываем после ack.
            Click::AdminClear(target) => {
                let cmd = ld2_protocol::cmd::AdminClear { target };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminClear);
                    if self.admin_target != "self" {
                        self.admin_target_request = true;
                    }
                }
            }
            Click::AdminGod(enabled) => {
                let cmd = ld2_protocol::cmd::AdminGodMode { enabled };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_world_sent(Act::AdminGod)
                }
            }
            _ => return false,
        }
        true
    }
}
