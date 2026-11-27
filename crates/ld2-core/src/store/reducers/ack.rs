//! Редьюсер ack-ответов (`apply_ack`): корреляция команд, ошибки формы
//! и успешные payload-ы (§7). Вынесен из `store.rs` и разложен по доменам
//! (RF4c); тела веток перенесены без изменений.

use super::super::*;

impl AppStore {
    pub(crate) fn apply_ack(&mut self, ack: ld2_protocol::Ack) -> Vec<Effect> {
        // E_AUTH_EXPIRED важнее локальной корреляции (§4.5).
        if ack.error_code() == Some(ErrorCode::AuthExpired) {
            let msg = ack
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "Сессия истекла".into());
            return self.reset_to_login(Some(msg));
        }

        let Some(act) = self.pending.take() else {
            // M18/F41: ack без корреляции (гонка команд) молча теряется —
            // в debug-логе видно, чей ack пришёл не вовремя.
            tracing::debug!(
                "ack {} (seq={}) без pending — пропущен",
                ack.msg_type,
                ack.seq
            );
            return vec![];
        };

        if !ack.is_ok() {
            self.ack_error(act, &ack);
            return vec![];
        }

        match act {
            Act::Login | Act::Register | Act::Select | Act::Delete | Act::Create => {
                self.ack_session(act, &ack)
            }
            Act::Move | Act::MapWorld | Act::Invest => self.ack_world(act, &ack),
            Act::Equip
            | Act::Unequip
            | Act::Socket
            | Act::Learn
            | Act::CraftRankUp
            | Act::ProfessionReset => self.ack_char(act, &ack),
            Act::Sharpen | Act::Preview => self.ack_sharpen(act, &ack),
            Act::Party | Act::GuildBrowse | Act::Guild | Act::CrimeLog | Act::Craft => {
                self.ack_social(act, &ack)
            }
            _ => {}
        }
        self.ack_post(act, ack);
        vec![]
    }

    /// Ошибки ack: поля формы и тосты (бывший error-бранч `apply_ack`).
    fn ack_error(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        let msg = ack
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| format!("{:?}", ack.error_code()));
        match act {
            Act::Login => self.login_error = Some(msg),
            Act::Register => {
                // D35: код §9 → поле формы; текст — серверный (fallback §9).
                let code = ack
                    .error_code()
                    .unwrap_or(ld2_protocol::ErrorCode::Internal);
                self.register_error = Some(crate::register::RegisterError {
                    field: crate::register::register_field_for(&code),
                    message: msg,
                    retry_after_s: ack.error.as_ref().and_then(|e| e.retry_after_s),
                });
            }
            Act::Create => self.create_error = Some(msg),
            Act::Select => self.select_error = Some(msg),
            Act::Delete => {
                self.deleting_char = None;
                self.notice = Some(msg);
            }
            Act::Move
            | Act::Attack
            | Act::Examine
            | Act::Inspect
            | Act::PickupInfo
            | Act::NpcTalk
            | Act::NpcReply
            | Act::ShopList
            | Act::ShopBuy => self.world_error = Some(msg),
            // M16/F29h: ошибки листа — системные уведомления
            // (тосты), как и весь прочий фидбек.
            Act::Invest
            | Act::Equip
            | Act::Unequip
            | Act::Drop
            | Act::Sharpen
            | Act::Preview
            | Act::Upgrade
            | Act::Socket
            | Act::CrimeLog
            | Act::Learn
            | Act::Craft
            | Act::Quest => self.notice = Some(msg),
            Act::Cast => self.world_error = Some(msg),
            // Лимиты чата (E_RATE_LIMITED §9) и права (E_PERMISSION,
            // M14/F19) — тостом.
            Act::Chat | Act::ChatDelete => self.notice = Some(msg),
            // Соцдомен: ошибки (E_TRADE_STALE, E_PARTY_FULL §9) — тостом.
            Act::Party | Act::Trade | Act::GuildBrowse | Act::Guild => self.notice = Some(msg),
            // Игровые диалоги (M20, заготовка) — ошибки тостом.
            Act::DialogChoice | Act::DialogSubmit => self.notice = Some(msg),
            // Добыча (M20/F20b): отказы — в мир (как Examine/Move).
            Act::Gather => {
                // M39: гасим бар только своего действия — отказ сбора
                // не должен убирать прогресс разделки и наоборот.
                if self.harvest.as_ref().is_some_and(|h| h.action == "gather") {
                    self.harvest = None;
                }
                self.world_error = Some(msg);
            }
            // Профессии (M21): ранг/сброс — тосты (у тренера/граница).
            Act::CraftRankUp | Act::ProfessionReset => self.notice = Some(msg),
            // Админ-панель (M24): отказы — тост (каталог/ask/god/цель).
            Act::AdminCatalog
            | Act::AdminAsk
            | Act::AdminGod
            | Act::AdminTarget
            | Act::AdminClear => {
                self.notice = Some(msg);
            }
            // M26: отказ карты мира — тост (одна попытка на сессию).
            Act::MapWorld => {
                self.notice = Some(msg);
            }
            // Снятие шкур (M22): отказ до старта — чистим бар.
            Act::Harvest => {
                if self.harvest.as_ref().is_some_and(|h| h.action != "gather") {
                    self.harvest = None;
                }
                self.world_error = Some(msg);
            }
        }
    }

    /// Успешные ack сессии/персонажей.
    fn ack_session(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        match act {
            Act::Login => {}
            // Регистрация = немедленный вход (ADR-006): ack ok → тоже
            // ждём evt/session.start; ошибки — в register_error (M13).
            Act::Register => {
                self.register_error = None;
            }
            Act::Select => {}
            Act::Delete => {
                // M11.7: ack — карточка уходит из списка (ФР-02).
                if let Some(id) = self.deleting_char.take() {
                    if let Some(session) = self.session.as_mut() {
                        session.characters.retain(|c| c.id != id);
                    }
                }
                self.notice = Some("Персонаж удалён".into());
            }
            Act::Create => {
                // Аддитивное подтверждение: дополняем список персонажей (§7).
                if let (Some(session), Some(payload)) =
                    (self.session.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(character) =
                        serde_json::from_value::<CharacterSummary>(payload["character"].clone())
                    {
                        if !session.characters.iter().any(|c| c.id == character.id) {
                            session.characters.push(character);
                        }
                    }
                }
                self.notice = Some("Персонаж создан".into());
            }
            _ => {}
        }
    }

    /// Мир: движение, карта, вложение очков.
    fn ack_world(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        match act {
            Act::Invest => {
                // §7: ack несёт остаток пула.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Some(pool) = payload["pool_left"].as_i64() {
                        snap.char.xp_pool = pool;
                    }
                }
            }
            Act::Move => {
                // ack несёт локацию (§7): применяем сразу, событие дойдёт позже.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(location) =
                        serde_json::from_value::<LocationSnapshot>(payload["location"].clone())
                    {
                        snap.location = location;
                    }
                }
            }
            Act::MapWorld => {
                // M26: чанк карты — {locations, offset, total, root};
                // следующий чанк ставит map_request (насос в tick).
                if let Some(payload) = ack.payload.as_ref() {
                    if let Some(list) = payload["locations"].as_array() {
                        self.map_locations.extend(list.iter().cloned());
                    }
                    self.map_offset = payload["offset"].as_u64().unwrap_or(0) as usize;
                    self.map_total = payload["total"].as_u64().unwrap_or(0) as usize;
                    if let Some(root) = payload["root"].as_str() {
                        self.map_root = root.to_string();
                    }
                    self.map_request = self.map_offset < self.map_total;
                }
            }
            _ => {}
        }
    }

    /// Персонаж: экипировка, сокеты, профессии.
    fn ack_char(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        match act {
            Act::Equip | Act::Unequip => {
                // §7: ack несёт обновлённый `char` — события на смену экипировки нет.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(ch) = serde_json::from_value::<ld2_protocol::types::CharacterState>(
                        payload["char"].clone(),
                    ) {
                        snap.char = ch;
                    }
                }
            }
            Act::ProfessionReset => {
                // M21: после сброса — убрать профессию из снапшота
                // (слот основной освобождается).
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Some(id) = payload["profession_id"].as_str() {
                        snap.char.professions.retain(|p| p.id != id);
                    }
                }
            }
            Act::Learn | Act::CraftRankUp => {
                // §7: ack {profession} — добавляем/обновляем в снапшоте.
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(prof) = serde_json::from_value::<
                        ld2_protocol::types::ProfessionSnapshot,
                    >(payload["profession"].clone())
                    {
                        if let Some(existing) =
                            snap.char.professions.iter_mut().find(|p| p.id == prof.id)
                        {
                            *existing = prof;
                        } else {
                            snap.char.professions.push(prof);
                        }
                    }
                }
            }
            Act::Socket => {
                // §7: ack извлекаемого несёт {staff, rune}, вставки — {staff};
                // M18/F32: цель надета — ack дополнен `char` (бонусы
                // самоцвета/урон руны вступили в силу сразу).
                if let (Some(snap), Some(payload)) = (self.snapshot.as_mut(), ack.payload.as_ref())
                {
                    if let Ok(ch) = serde_json::from_value::<ld2_protocol::types::CharacterState>(
                        payload["char"].clone(),
                    ) {
                        snap.char = ch;
                    } else {
                        apply_item_patch(&mut snap.char, payload);
                    }
                }
                // M24 фикс: инкрустация стака — сервер разбил стак и
                // прислал НОВЫЙ uid целевой штуки; окно следует за
                // целью (иначе следующая вставка снова шла бы в стак
                // и каждый сокет попадал бы на свою штуку).
                if self.socketing.is_some() {
                    if let Some(staff_uid) = ack
                        .payload
                        .as_ref()
                        .and_then(|p| p["staff"]["uid"].as_str())
                    {
                        self.socketing = Some(staff_uid.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    /// Заточка: выбор камня и предпросмотр.
    fn ack_sharpen(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        match act {
            Act::Sharpen => {
                // M18/F54: сервер выбрал камень — запоминаем для окна
                // заточки (имя берём из снапшота, качество — из ack).
                if let Some(payload) = ack.payload.as_ref() {
                    // Типизированный ack (ld2-protocol, golden §7) —
                    // старая/чужая форма молча игнорируется.
                    let Ok(sel) =
                        serde_json::from_value::<ld2_protocol::StoneSelected>(payload.clone())
                    else {
                        return;
                    };
                    let name = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| {
                            s.char
                                .inventory
                                .iter()
                                .find(|i| i.uid == sel.stone_uid)
                                .map(|i| i.name.clone())
                        })
                        .unwrap_or_else(|| sel.stone_uid.clone());
                    self.sharpen_stone = Some(SharpenStone {
                        uid: sel.stone_uid,
                        name,
                        quality: sel.quality,
                    });
                    // M18/F55: окно открыто — гейт рендера (список).
                    self.sharpen_list = true;
                    // Свежий выбор камня — прошлый результат неактуален.
                    if let Some(upgrade) = self.upgrade.as_mut() {
                        upgrade.result = None;
                        upgrade.preview = None;
                    }
                }
            }
            Act::Preview => {
                if let (Some(upgrade), Some(payload)) =
                    (self.upgrade.as_mut(), ack.payload.as_ref())
                {
                    upgrade.preview = Some(payload.clone());
                    // M18/F55: итог прошлого броска НЕ затираем — он
                    // должен остаться на экране заточки (зелёным/красным)
                    // даже после автопревью. Сбрасывается при открытии
                    // предмета (`open_upgrade`), при смене камня и при
                    // старте следующего улучшения.
                }
            }
            _ => {}
        }
    }

    /// Социум: группа, гильдия, журнал, крафт.
    fn ack_social(&mut self, act: Act, ack: &ld2_protocol::Ack) {
        match act {
            Act::Party => {
                // §7: party.create/accept несут {party}.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(party) =
                        serde_json::from_value::<PartyState>(payload["party"].clone())
                    {
                        self.party = Some(party);
                    }
                }
            }
            Act::GuildBrowse => {
                // §7: guild.browse → {guilds[]} (идемпотентна, §7).
                if let Some(payload) = ack.payload.as_ref() {
                    if let Some(list) = payload["guilds"].as_array() {
                        self.guild_browse = list.clone();
                    }
                }
            }
            Act::Guild => {
                // §7: guild.create/join несут {guild}.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(guild) =
                        serde_json::from_value::<GuildState>(payload["guild"].clone())
                    {
                        self.guild = Some(guild);
                    }
                }
            }
            Act::Craft => {
                // §7: ack несёт список рецептов.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Some(recipes) = payload["recipes"].as_array() {
                        self.recipes = recipes.clone();
                    }
                }
                self.recipes_loaded = true;
            }
            Act::CrimeLog => {
                // §7: ack несёт журнал нарушений целиком.
                if let Some(payload) = ack.payload.as_ref() {
                    if let Ok(entries) =
                        serde_json::from_value::<Vec<CrimeLogEntry>>(payload["entries"].clone())
                    {
                        self.crime_log = entries;
                        self.crime_log_loaded = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Пост-обработка успешных ack по `act` (магазин, диалоги NPC, осмотр, админка).
    fn ack_post(&mut self, act: Act, ack: ld2_protocol::Ack) {
        // M18/F39: магазин — ack `{shop}` → панель внутри окна диалога.
        if act == Act::ShopList {
            if let (Some(dialog), Some(payload)) = (self.npc_dialog.as_mut(), ack.payload.as_ref())
            {
                if let Ok(shop) = serde_json::from_value::<NpcShopView>(payload["shop"].clone()) {
                    dialog.shop_currency = shop.tabs.first().map(|tab| tab.currency.clone());
                    dialog.shop = Some(shop);
                }
            }
        }
        // M18/F35: диалог NPC — ack `{dialog, node, quests}` → окно (до
        // Examine: там payload перемещается).
        if act == Act::NpcTalk {
            if let Some(payload) = ack.payload.as_ref() {
                self.npc_dialog = NpcDialogView::from_payload(payload);
                // M18/F41: диагностика повторного открытия диалога.
                tracing::debug!(
                    "ack npc.talk (seq={}) → открыт={}, узел={:?}, строк ленты={}",
                    ack.seq,
                    self.npc_dialog.is_some(),
                    self.npc_dialog
                        .as_ref()
                        .and_then(|d| d.node.as_ref().map(|n| n.id.clone())),
                    self.npc_dialog.as_ref().map_or(0, |d| d.transcript.len()),
                );
            } else {
                tracing::debug!("ack npc.talk (seq={}) без payload", ack.seq);
            }
        }
        // M18/F37: ответ персонажа — новый узел (`node: null` — конец);
        // список квестов освежается (приняли/сдали между открытиями).
        if act == Act::NpcReply {
            if let Some(payload) = ack.payload.as_ref() {
                let terminal = payload.get("node").is_none_or(|v| v.is_null());
                if terminal {
                    // M18/F38: конец диалога («Пока») — окно закрывается.
                    self.npc_dialog = None;
                } else if let Some(dialog) = self.npc_dialog.as_mut() {
                    if let Ok(mut node) = serde_json::from_value::<NpcNode>(payload["node"].clone())
                    {
                        // M18/F42: дедлайны для живого отсчёта таймеров.
                        node.stamp_wait_deadlines();
                        dialog.transcript.push(NpcTranscriptLine {
                            me: false,
                            text: node.text.clone(),
                        });
                        dialog.node = Some(node);
                        if let Ok(quests) = serde_json::from_value::<
                            Vec<ld2_protocol::types::QuestState>,
                        >(payload["quests"].clone())
                        {
                            dialog.quests = quests;
                        }
                    }
                }
            }
        }
        // Осмотр: payload ack'а — детали сущности (включая лут трупа, ФР-03/ФР-11).
        if act == Act::Inspect {
            // M35: карточка осмотра — только присланный публичный лист.
            self.inspect = ack
                .payload
                .and_then(|p| p.get("char").cloned())
                .and_then(|v| {
                    serde_json::from_value::<ld2_protocol::types::CharInspectView>(v).ok()
                });
        } else if act == Act::Examine {
            self.entity_detail = ack.payload;
        } else if act == Act::PickupInfo {
            // M23: окно подятия — тот же `entity.info`, но в отдельное
            // поле (окно не перетирает модалку осмотра и наоборот).
            self.pickup = ack.payload;
        } else if act == Act::AdminCatalog {
            // M24: каталог админ-окна — `{items:[…]}` → массив.
            self.admin_catalog = ack
                .payload
                .and_then(|p| p.get("items").cloned())
                .and_then(|v| v.as_array().cloned());
        } else if act == Act::AdminGod {
            // M24: эхо состояния тумблера (админ мог переключить
            // в другом окне — сервер истина).
            if let Some(enabled) = ack.payload.as_ref().and_then(|p| p["enabled"].as_bool()) {
                self.admin_god = enabled;
            }
        } else if act == Act::AdminTarget {
            // M24: содержимое цели правой колонки — `{items:[…]}`.
            self.admin_target_items = ack
                .payload
                .and_then(|p| p.get("items").cloned())
                .and_then(|v| v.as_array().cloned());
        }
    }
}
