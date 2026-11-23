//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{
    AdminClear, AdminContentReload, AdminGiveAsk, AdminGodMode, AdminItems, AdminRemoveAsk,
    AdminTargetInfo, ChatDelete, ChatSend,
};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{
    CharXp, ItemAdded, ItemRemovalReason, ItemRemoved, ItemState, LocationChanged, Pong, TimeSync,
};
use ld2_protocol::types::{CharacterState, Entity};
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use crate::db;
use crate::state::SharedState;

mod auth;
mod chars;
mod combat;
mod craft;
mod crime;
mod economy;
mod entity;
mod gates;
mod guild;
mod harvest;
mod items;
mod location;
mod magic;
mod npc;
mod party;
mod quests;
mod rules;
mod socketing;
mod trade;
mod upgrade;

use rules::{content_bonuses, entity_loot_defs, location_snapshot, UpgradePreviewState};
pub(crate) use rules::{item_snapshot, stub_location};

/// Исходящее сообщение соединения (как в моке: ack → события → опц. закрытие).
#[derive(Debug)]
pub enum Out {
    Text(String),
    Close,
}

pub struct Session {
    state: SharedState,
    peer: SocketAddr,
    /// §2.4.2: последние 128 обработанных `seq` → точный кэшированный ack.
    ack_cache: HashMap<u32, String>,
    seq_order: VecDeque<u32>,
    /// Аккаунт соединения (ADR-006): `Some` после login/register/resume.
    account_id: Option<i64>,
    /// M14/F19: роль аккаунта — права на system-чат/удаление (§9 E_PERMISSION).
    role: String,
    #[allow(dead_code)] // журнал/логика персонажей — M12.2 (фаза «персонажи»)
    login: Option<String>,
    authenticated: bool,
    /// Выбранный персонаж (`char.select`, §3): снапшот и ack-версии — его.
    selected_char_id: Option<String>,
    char: Option<CharacterState>,
    /// Текущая локация персонажа (M12.3): разрешается при входе/переходе.
    location_id: Option<String>,
    /// Канал исходящих конвертов соединения (рассылка тика боя, M12.5).
    /// R15: bounded (256) — см. transport.rs.
    out_tx: tokio::sync::mpsc::Sender<String>,
    /// R15: окно rate-limit мутирующих команд `(start_epoch, счётчик)`.
    cmd_rate_window: (i64, u32),
    /// Активное превью улучшения (ФР-10, M12.5c): stale → `E_UPGRADE_PREVIEW_STALE`.
    preview_state: Option<UpgradePreviewState>,
    /// M18/F54: выбранный камень усиления (uid) — активен, пока игрок
    /// не потратит его на улучшение. Ставится `cmd/item.use` камня,
    /// проверяется на превью/старте, тратится при старте (1 шт./улучшение).
    sharpen_stone: Option<String>,
    /// Кулдаун чата (`chat.cooldown_s`, ФР-13): момент последнего сообщения.
    last_chat_at: Option<std::time::Instant>,
    /// M21: переход «повышение ранга» наградой квеста — данные
    /// применяются после `set_state(done)` (граница проверена до).
    pending_rank_up: Option<(String, i64, i32, i32)>,
}

impl Session {
    pub fn new(
        state: SharedState,
        peer: SocketAddr,
        out_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            state,
            peer,
            out_tx,
            ack_cache: HashMap::new(),
            seq_order: VecDeque::new(),
            account_id: None,
            role: "player".to_string(),
            login: None,
            authenticated: false,
            selected_char_id: None,
            char: None,
            location_id: None,
            preview_state: None,
            sharpen_stone: None,
            pending_rank_up: None,
            last_chat_at: None,
            cmd_rate_window: (crate::time::now_epoch(), 0),
        }
    }

    /// Входящий текст → исходящие (ровно один ack на команду, §2.2).
    pub async fn on_text(&mut self, text: &str) -> Vec<Out> {
        let env: Envelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(e) => {
                warn!("неразборчивое сообщение: {e}");
                return vec![];
            }
        };
        // §11: сервер говорит только v=1; более новое не разбираем — игнор.
        if env.v != PROTOCOL_VERSION {
            warn!("конверт v={} (ожидается {PROTOCOL_VERSION}) — игнор", env.v);
            return vec![];
        }
        if !env.msg_type.starts_with("cmd/") {
            warn!("не-команда от клиента: {}", env.msg_type);
            return vec![];
        }
        // §2.4.2: повтор seq → точный кэшированный ack, без повторных событий.
        if let Some(seq) = env.seq {
            if let Some(cached) = self.ack_cache.get(&seq) {
                return vec![Out::Text(cached.clone())];
            }
        }
        let outs = self.dispatch(&env).await;
        // Первый ответ диспетчера — всегда ack (§2.2); кэшируем его текст.
        if let (Some(seq), Some(Out::Text(ack_text))) = (env.seq, outs.first()) {
            self.ack_cache.insert(seq, ack_text.clone());
            self.seq_order.push_back(seq);
            while self.seq_order.len() > 128 {
                if let Some(old) = self.seq_order.pop_front() {
                    self.ack_cache.remove(&old);
                }
            }
        }
        outs
    }

    async fn dispatch(&mut self, env: &Envelope) -> Vec<Out> {
        let seq = env.seq.unwrap_or(0);
        // M24: разделка занята — блок действий персонажа (§9 E_BUSY).
        // Исключения: item-* (прерывают — M23), информационные
        // (осмотр/снапшот/чат/диалоги), admin-панель, corpse.harvest
        // (переключение на другой труп разрешено; E_COOLDOWN — только
        // повторный старт на том же трупе).
        const HARVEST_BLOCKED: &[&str] = &[
            "cmd/location.move",
            "cmd/combat.attack",
            "cmd/combat.stop",
            "cmd/combat.flee",
            "cmd/skill.use",
            "cmd/magic.rune_cast",
            "cmd/magic.scroll_cast",
            "cmd/resource.gather",
            "cmd/craft.start",
            "cmd/quest.accept",
            "cmd/quest.turn_in",
            "cmd/quest.abandon",
            "cmd/trade.open",
            "cmd/trade.add_item",
            "cmd/trade.set_gold",
            "cmd/trade.propose",
            "cmd/trade.accept",
            "cmd/trade.cancel",
            "cmd/party.create",
            "cmd/party.invite",
            "cmd/party.accept",
            "cmd/party.decline",
            "cmd/party.leave",
            "cmd/party.kick",
            "cmd/guild.create",
            "cmd/guild.join",
            "cmd/guild.leave",
            "cmd/guild.invite",
            "cmd/guild.kick",
            "cmd/guild.rank_set",
            "cmd/shop.buy",
            "cmd/upgrade.preview",
            "cmd/upgrade.start",
            "cmd/socket.insert",
            "cmd/socket.extract",
        ];
        if HARVEST_BLOCKED.contains(&env.msg_type.as_str()) && self.char_is_harvesting() {
            return vec![Self::err(seq, ErrorCode::Busy, "персонаж занят разделкой")];
        }
        // M25: дебаффы контроля (эффекты приёмов босса): стан — ничего,
        // немота — магия, ослепление — атаки/навыки.
        if let Some((code, msg)) = self.char_effect_gate(env.msg_type.as_str()) {
            return vec![Self::err(seq, code, msg)];
        }
        // R15: per-session rate-limit на мутирующие команды (D25: лимит
        // `[gameplay] cmd_rate_per_min`, 0 — выключен). auth/chat — свои
        // лимиты; каждая итерация берёт mutex мира и ходит в БД.
        if HARVEST_BLOCKED.contains(&env.msg_type.as_str()) || env.msg_type == "cmd/item.use" {
            if let Some(out) = self.cmd_rate_gate(seq) {
                return vec![out];
            }
        }

        match env.msg_type.as_str() {
            // auth / session (ADR-006, §4.5)
            "cmd/auth.register" => self.auth_register(env, seq).await,
            "cmd/auth.login" => self.auth_login(env, seq).await,
            "cmd/auth.resume" => self.auth_resume(env, seq).await,
            "cmd/auth.logout" => self.auth_logout(seq).await,
            "cmd/session.refresh" => self.auth_refresh(seq).await,
            // персонажи (ФР-02, §5/§3)
            "cmd/char.list" => self.char_list(seq).await,
            "cmd/char.create" => self.char_create(env, seq).await,
            "cmd/char.select" => self.char_select(env, seq).await,
            "cmd/char.delete" => self.char_delete(env, seq).await,
            // мир (ФР-03, M12.3)
            "cmd/location.look" => self.location_look(seq).await,
            "cmd/location.move" => self.location_move(env, seq).await,
            "cmd/map.world" => self.map_world(env, seq).await,
            "cmd/entity.info" => self.entity_info(env, seq).await,
            // M18/F35: диалог NPC (поговорить/квесты)
            "cmd/npc.talk" => self.npc_talk(env, seq).await,
            // M18/F37: ответ персонажа — переход по ветке
            "cmd/npc.reply" => self.npc_reply(env, seq).await,
            // M18/F39: магазин NPC
            "cmd/shop.list" => self.shop_list(env, seq).await,
            "cmd/shop.buy" => self.shop_buy(env, seq).await,
            "cmd/state.resync" => self.state_resync(env, seq).await,
            // бой (ФР-05, M12.5)
            "cmd/combat.attack" => self.combat_attack(env, seq).await,
            "cmd/combat.stop" => self.combat_stop(seq).await,
            "cmd/combat.flee" => self.combat_flee(seq).await,
            // гильдия (ФР-16, M12.7d)
            "cmd/guild.browse" => self.guild_browse(env, seq).await,
            "cmd/guild.create" => self.guild_create(env, seq).await,
            "cmd/guild.join" => self.guild_join(env, seq).await,
            "cmd/guild.leave" => self.guild_leave(seq).await,
            "cmd/guild.invite" => self.guild_invite(env, seq).await,
            "cmd/guild.kick" => self.guild_kick(env, seq).await,
            "cmd/guild.rank_set" => self.guild_rank_set(env, seq).await,
            // сделка (ФР-15, M12.7c)
            "cmd/trade.open" => self.trade_open(env, seq).await,
            "cmd/trade.add_item" => self.trade_add_item(env, seq).await,
            "cmd/trade.set_gold" => self.trade_set_gold(env, seq).await,
            "cmd/trade.propose" => self.trade_lock_cmd(env, seq).await,
            "cmd/trade.accept" => self.trade_lock_cmd(env, seq).await,
            "cmd/trade.cancel" => self.trade_cancel(env, seq).await,
            // группа (ФР-14, M12.7b)
            "cmd/party.create" => self.party_create(seq).await,
            "cmd/party.invite" => self.party_invite(env, seq).await,
            "cmd/party.accept" => self.party_accept(env, seq).await,
            "cmd/party.decline" => self.party_decline(env, seq).await,
            "cmd/party.leave" => self.party_leave(seq).await,
            "cmd/party.kick" => self.party_kick(env, seq).await,
            // профессии/крафт (ФР-08, M12.6)
            "cmd/craft.list" => self.craft_list(seq).await,
            "cmd/craft.recipes" => self.craft_recipes(env, seq).await,
            "cmd/craft.learn" => self.craft_learn(env, seq).await,
            "cmd/craft.reset" => self.craft_reset(env, seq).await,
            "cmd/craft.rank_up" => self.craft_rank_up(env, seq).await,
            "cmd/craft.start" => self.craft_start(env, seq).await,
            // квесты (ФР-12, M12.6)
            "cmd/quest.list" => self.quest_list(seq).await,
            "cmd/quest.accept" => self.quest_accept(env, seq).await,
            "cmd/quest.turn_in" => self.quest_turn_in(env, seq).await,
            "cmd/quest.abandon" => self.quest_abandon(env, seq).await,
            // преступность (ФР-11, §10, M12.5d)
            "cmd/crime.log" => self.crime_log(env, seq).await,
            // улучшение предметов (ФР-10, M12.5c)
            "cmd/upgrade.preview" => self.upgrade_preview(env, seq).await,
            "cmd/upgrade.start" => self.upgrade_start(env, seq).await,
            // активные способности (§4, M14)
            "cmd/skill.use" => self.skill_use(env, seq).await,
            // магия и сокеты (ФР-09, M12.5b)
            "cmd/magic.rune_cast" => self.magic_rune_cast(env, seq).await,
            "cmd/magic.scroll_cast" => self.magic_scroll_cast(env, seq).await,
            "cmd/socket.insert" => self.socket_insert(env, seq).await,
            "cmd/socket.extract" => self.socket_extract(env, seq).await,
            // добыча ресурсов (M20/F20b)
            "cmd/resource.gather" => self.resource_gather(env, seq).await,
            // снятие шкур (сервер-джоб5 с)
            "cmd/corpse.harvest" => self.corpse_harvest(env, seq).await,
            // игровые диалоги (M20 — заготовка)
            "cmd/dialog.choice" => self.dialog_choice(env, seq).await,
            "cmd/dialog.submit" => self.dialog_submit(env, seq).await,
            // M24: админ-панель предметов (гейт role=admin)
            "cmd/admin.items" => self.admin_items(env, seq).await,
            "cmd/admin.give.ask" => self.admin_give_ask(env, seq).await,
            "cmd/admin.remove.ask" => self.admin_remove_ask(env, seq).await,
            "cmd/admin.god_mode" => self.admin_god_mode(env, seq).await,
            "cmd/admin.target.info" => self.admin_target_info(env, seq).await,
            "cmd/admin.clear" => self.admin_clear(env, seq).await,
            "cmd/admin.content.reload" => self.admin_content_reload(env, seq).await,
            // лист/инвентарь/опыт (ФР-04/07, M12.4)
            "cmd/skill.train" => self.skill_train(env, seq).await,
            "cmd/item.use" => {
                let mut outs = self.maybe_interrupt_harvest(seq).await;
                outs.extend(self.item_use(env, seq).await);
                outs
            }
            "cmd/item.equip" => {
                let mut outs = self.maybe_interrupt_harvest(seq).await;
                outs.extend(self.item_equip(env, seq).await);
                outs
            }
            "cmd/item.unequip" => {
                let mut outs = self.maybe_interrupt_harvest(seq).await;
                outs.extend(self.item_unequip(env, seq).await);
                outs
            }
            "cmd/item.drop" => {
                let mut outs = self.maybe_interrupt_harvest(seq).await;
                outs.extend(self.item_drop(env, seq).await);
                outs
            }
            "cmd/item.move" => {
                let mut outs = self.maybe_interrupt_harvest(seq).await;
                outs.extend(self.item_move(env, seq).await);
                outs
            }
            // dev-сценарии (`/scenario …`); чат — ФР-13 в фазе M12.7
            "cmd/chat.send" => self.chat_send(env, seq).await,
            "cmd/chat.delete" => self.chat_delete(env, seq).await,
            // Сервисный ping (§7): ack + echo; RTT клиент считает по своему
            // `ts` — сервер отвечает 0, как и мок.
            "cmd/ping" => {
                let pong = Pong {
                    ts: crate::time::now_rfc3339(),
                    rtt_ms: 0,
                };
                vec![
                    out_ack(Ack::ok(seq, None)),
                    out_event(
                        "evt/pong",
                        None,
                        serde_json::to_value(&pong).unwrap_or_default(),
                    ),
                ]
            }
            "cmd/time.sync" => {
                let sync = TimeSync {
                    server_ts: crate::time::now_epoch(),
                    rtt_hint_ms: Some(0),
                };
                vec![
                    out_ack(Ack::ok(seq, None)),
                    out_event(
                        "evt/time.sync",
                        None,
                        serde_json::to_value(&sync).unwrap_or_default(),
                    ),
                ]
            }
            other => {
                warn!("неизвестная команда {other}");
                vec![out_ack(Ack::error(
                    seq,
                    None,
                    &ErrorCode::Internal,
                    format!("неизвестная команда: {other}"),
                ))]
            }
        }
    }
    // ── мир (ФР-03, M12.3) ─────────────────────────────────────────────────

    /// Выбранный персонаж (для отвязки от мира при дисконнекте, M12.5).
    pub fn selected_char(&self) -> Option<String> {
        self.selected_char_id.clone()
    }

    // ── лист, инвентарь, опыт (ФР-04/07, M12.4) ────────────────────────────

    // ── M24: админ-панель предметов ──────────────────────────────────────────

    /// `cmd/admin.items` (§7): каталог всех определений — левая панель
    /// окна выдачи (вариант A: одним запросом, клиент кэширует и
    /// фильтрует локально). Тяжёлые поля (stats/requires) не отдаём.
    async fn admin_items(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

    /// M24: цель существует и валидна для текущего контекста:
    /// `self` (выбран персонаж), `char:<имя>` (есть в БД, в т.ч.
    /// оффлайн), `corpse:<id>`/`monster:<id>` (в текущей локации).
    /// Сундук/магазин — F24d.
    async fn admin_target_valid(&self, target: &str) -> bool {
        if target == "self" {
            return self.selected_char_id.is_some();
        }
        if let Some(name) = target.strip_prefix("char:") {
            return matches!(
                db::characters::id_by_name(&self.state.pool, name).await,
                Ok(Some(_))
            );
        }
        let current = self.location_id.clone().unwrap_or_default();
        // Динамические цели — память мира (возврат внутри scope:
        // блокировка не держится через await).
        if target.starts_with("corpse:") || target.starts_with("monster:") {
            let world = self.state.world.lock().expect("мир");
            return if let Some(id) = target.strip_prefix("corpse:") {
                world.corpse(id).is_some_and(|c| c.location_id == current)
            } else if let Some(id) = target.strip_prefix("monster:") {
                world.mob(id).is_some_and(|m| m.location_id == current)
            } else {
                false
            };
        }
        // Статические цели (F24d): сундук/магазин — сущности текущей
        // локации из БД + магазины контента.
        let (container, id) = if let Some(id) = target.strip_prefix("container:") {
            (true, id)
        } else if let Some(id) = target.strip_prefix("shop:") {
            (false, id)
        } else {
            return false;
        };
        let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
            return false;
        };
        let entities: Vec<Entity> = serde_json::from_value(loc_row.entities).unwrap_or_default();
        let kind = if container {
            ld2_protocol::types::EntityKind::Container
        } else {
            ld2_protocol::types::EntityKind::Npc
        };
        if !entities.iter().any(|e| e.id == id && e.kind == kind) {
            return false;
        }
        if !container {
            // Магазин существует и имеет валютные вкладки.
            return self
                .state
                .content()
                .shops
                .get(id)
                .is_some_and(|shop| !shop.currencies.is_empty());
        }
        true
    }

    /// M24: char_id цели-игрока (`self` → свой персонаж).
    async fn admin_target_char(&self, target: &str) -> Option<String> {
        if target == "self" {
            return self.selected_char_id.clone();
        }
        let name = target.strip_prefix("char:")?;
        db::characters::id_by_name(&self.state.pool, name)
            .await
            .ok()
            .flatten()
    }

    /// M24: имя предмета для заголовка диалога удаления (по цели и ref):
    /// uid инвентаря / `loot:`-слот / `inner:`-слот / `bonus:`-лут монстра.
    async fn admin_item_def(&self, target: &str, item_ref: &str) -> Option<db::items::ItemDefRow> {
        let def_id = if let Some(rest) = item_ref.strip_prefix("loot:") {
            let (corpse_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.corpse(corpse_id)?.loot.get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("inner:") {
            let (corpse_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.corpse(corpse_id)?.inner_loot.get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("bonus:") {
            let idx: usize = rest.parse().ok()?;
            let mob_id = target.strip_prefix("monster:")?;
            let world = self.state.world.lock().expect("мир");
            world.mob_bonus(mob_id).get(idx)?.clone()
        } else if let Some(rest) = item_ref.strip_prefix("cont:") {
            let (entity_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let current = self.location_id.clone().unwrap_or_default();
            let loc_row = db::locations::get(&self.state.pool, &current)
                .await
                .ok()??;
            let entities: Vec<Entity> = serde_json::from_value(loc_row.entities).ok()?;
            let entity = entities.into_iter().find(|e| e.id == entity_id)?;
            entity_loot_defs(&entity).get(idx)?.clone()
        } else if let Some(rest) = item_ref.strip_prefix("contx:") {
            let (entity_id, idx) = rest.rsplit_once(':')?;
            let idx: usize = idx.parse().ok()?;
            let world = self.state.world.lock().expect("мир");
            world.container_extra_slot(entity_id, idx)?.0
        } else if let Some(rest) = item_ref.strip_prefix("xshop:") {
            let idx: usize = rest.parse().ok()?;
            let npc_id = target.strip_prefix("shop:")?;
            let world = self.state.world.lock().expect("мир");
            world.shop_extra(npc_id).get(idx)?.clone()?
        } else if let Some(rest) = item_ref.strip_prefix("shop:") {
            let (tab, idx) = rest.rsplit_once(':')?;
            let (tab, idx): (usize, usize) = (tab.parse().ok()?, idx.parse().ok()?);
            let npc_id = target.strip_prefix("shop:")?;
            self.state
                .content()
                .shops
                .get(npc_id)?
                .currencies
                .get(tab)?
                .items
                .get(idx)?
                .clone()
        } else {
            let char_id = self.admin_target_char(target).await?;
            let row = db::items::find(&self.state.pool, item_ref, &char_id)
                .await
                .ok()??;
            row.def_id
        };
        db::items::def(&self.state.pool, &def_id)
            .await
            .ok()
            .flatten()
    }

    /// M24: имя предмета по цели/ref — заголовок диалога удаления.
    async fn admin_item_name(&self, target: &str, item_ref: &str) -> Option<String> {
        self.admin_item_def(target, item_ref).await.map(|d| d.name)
    }

    /// `cmd/admin.give.ask` (§7): валидация def/цели → открытие
    /// execute-диалога количества; id `admin.give:<target>#<def_id>`
    /// кодирует действие — submit stateless (роль там повторно).
    async fn admin_give_ask(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
                "evt/dialog/open",
                None,
                serde_json::to_value(open).unwrap_or_default(),
            ),
        ]
    }

    /// `cmd/admin.remove.ask` (§7): execute-диалог количества на
    /// удаление; ref — uid / `loot:`/`inner:`-слот / `bonus:`.
    async fn admin_remove_ask(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
                "evt/dialog/open",
                None,
                serde_json::to_value(open).unwrap_or_default(),
            ),
        ]
    }

    /// `cmd/admin.god_mode` (§7): toggle неуязвимости — состояние в
    /// мире (tick::apply_mob_hit смотрит оттуда), гейт admin.
    async fn admin_god_mode(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

    /// `cmd/admin.target.info` (§7): содержимое цели для правой
    /// колонки админ-окна — инвентарь (`self`/`char:`), слоты лута
    /// трупа (`loot:`/`inner:`), админ-лут монстра (`bonus:`).
    async fn admin_target_info(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<AdminTargetInfo>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не найдена")];
        }
        // Инвентарь (self/char) — одним SQL с джойном определений.
        if let Some(char_id) = self.admin_target_char(&cmd.target).await {
            let rows = match db::items::admin_inventory(&self.state.pool, &char_id).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("admin target.info (инвентарь {char_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "ref": r.uid,
                        "id": r.def_id,
                        "name": r.name,
                        "type": r.item_type,
                        "rarity": r.rarity,
                        "qty": r.qty,
                    })
                })
                .collect();
            return vec![Self::ack_payload(
                seq,
                &serde_json::json!({ "items": items }),
            )];
        }
        // Сундук/магазин (F24d) — статика локации/контента; труп/
        // монстр — память мира; блокировки не держатся через await.
        let slots: Vec<(String, String, Option<i64>)> = if cmd.target.starts_with("container:") {
            let id = &cmd.target["container:".len()..];
            let current = self.location_id.clone().unwrap_or_default();
            let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "сундука нет")];
            };
            let entities: Vec<Entity> =
                serde_json::from_value(loc_row.entities).unwrap_or_default();
            let Some(entity) = entities.into_iter().find(|e| e.id == id) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "сундука нет")];
            };
            let defs = entity_loot_defs(&entity);
            let (removed, extra) = {
                let world = self.state.world.lock().expect("мир");
                let removed: std::collections::HashSet<usize> = (0..defs.len())
                    .filter(|i| world.container_base_removed(id, *i))
                    .collect();
                (removed, world.container_extra(id))
            };
            let mut out: Vec<(String, String, Option<i64>)> = defs
                .iter()
                .enumerate()
                .filter(|(i, _)| !removed.contains(i))
                .map(|(i, d)| (format!("cont:{id}:{i}"), d.clone(), None))
                .collect();
            out.extend(extra.iter().enumerate().filter_map(|(n, slot)| {
                slot.as_ref()
                    .map(|(d, q)| (format!("contx:{id}:{n}"), d.clone(), Some(*q)))
            }));
            out
        } else if cmd.target.starts_with("shop:") {
            let npc = &cmd.target["shop:".len()..];
            let content = self.state.content();
            let Some(shop) = content.shops.get(npc) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
            };
            let (extra, removed) = {
                let world = self.state.world.lock().expect("мир");
                let mut removed = std::collections::HashSet::new();
                for (tab, currency) in shop.currencies.iter().enumerate() {
                    for idx in 0..currency.items.len() {
                        if world.shop_content_removed(npc, tab, idx) {
                            removed.insert((tab, idx));
                        }
                    }
                }
                (world.shop_extra(npc), removed)
            };
            let mut out: Vec<(String, String, Option<i64>)> = Vec::new();
            for (tab, currency) in shop.currencies.iter().enumerate() {
                for (idx, def_id) in currency.items.iter().enumerate() {
                    if removed.contains(&(tab, idx)) {
                        continue;
                    }
                    out.push((format!("shop:{tab}:{idx}"), def_id.clone(), None));
                }
            }
            out.extend(extra.iter().enumerate().filter_map(|(n, slot)| {
                slot.as_ref()
                    .map(|d| (format!("xshop:{n}"), d.clone(), None))
            }));
            out
        } else {
            let world = self.state.world.lock().expect("мир");
            if let Some(id) = cmd.target.strip_prefix("corpse:") {
                let Some(corpse) = world.corpse(id) else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
                };
                corpse
                    .loot
                    .iter()
                    .enumerate()
                    .filter_map(|(i, slot)| {
                        slot.as_ref()
                            .map(|d| (format!("loot:{id}:{i}"), d.clone(), None))
                    })
                    .chain(
                        corpse
                            .inner_loot
                            .iter()
                            .enumerate()
                            .filter_map(|(i, slot)| {
                                slot.as_ref()
                                    .map(|d| (format!("inner:{id}:{i}"), d.clone(), None))
                            }),
                    )
                    .collect()
            } else if let Some(id) = cmd.target.strip_prefix("monster:") {
                world
                    .mob_bonus(id)
                    .into_iter()
                    .enumerate()
                    .map(|(i, d)| (format!("bonus:{i}"), d, None))
                    .collect()
            } else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "неизвестная цель")];
            }
        };
        let mut items: Vec<serde_json::Value> = Vec::new();
        for (item_ref, def_id, custom_qty) in slots {
            let Some(def) = db::items::def(&self.state.pool, &def_id)
                .await
                .ok()
                .flatten()
            else {
                continue; // слот ссылается на несуществующее определение
            };
            items.push(serde_json::json!({
                "ref": item_ref,
                "id": def.id,
                "name": def.name,
                "type": def.item_type,
                "rarity": def.rarity,
                // M24: у стака надстройки — своё количество.
                "qty": custom_qty.unwrap_or_else(|| i64::from(def.qty)),
            }));
        }
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "items": items }),
        )]
    }

    /// `cmd/admin.clear` (§7): удалить ВСЕ предметы цели (кнопка
    /// «Удалить все» в админ-окне): инвентарь — все записи, труп —
    /// все слоты (broadcast), сундук — база tombstone-ами + надстройка,
    /// магазин — контент + надстройка, монстр — весь лут (восстановится
    /// респавном). Аудит каждой операции.
    async fn admin_clear(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        if let Some(err) = self.require_admin(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<AdminClear>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.admin_target_valid(&cmd.target).await {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не найдена")];
        }

        // ── инвентарь (self/char) ──
        if let Some(char_id) = self.admin_target_char(&cmd.target).await {
            let own = match self.require_char_id(seq) {
                Ok(id) => id,
                Err(out) => return vec![out],
            };
            let rows = match db::items::delete_all(&self.state.pool, &char_id).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("admin clear (инвентарь {char_id}): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            tracing::info!(
                "admin clear inventory: {char_id} ({} предметов)",
                rows.len()
            );
            if own == char_id {
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                for row in rows {
                    outs.push(out_event(
                        "evt/item/removed",
                        None,
                        serde_json::to_value(ItemRemoved {
                            uid: row.uid,
                            reason: ItemRemovalReason::Consumed,
                        })
                        .unwrap_or_default(),
                    ));
                }
                return outs;
            }
            // Чужой персонаж: события — в его коннект (оффлайн просто
            // увидит пустой инвентарь при входе).
            {
                let world = self.state.world.lock().expect("мир");
                for row in rows {
                    world.send_to_char(
                        &char_id,
                        event_text(
                            "evt/item/removed",
                            None,
                            serde_json::to_value(ItemRemoved {
                                uid: row.uid,
                                reason: ItemRemovalReason::Consumed,
                            })
                            .unwrap_or_default(),
                        ),
                    );
                }
            }
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── труп: все слоты + broadcast ──
        if let Some(id) = cmd.target.strip_prefix("corpse:") {
            let cleared = {
                let mut world = self.state.world.lock().expect("мир");
                world.corpse_clear(id)
            };
            if cleared.is_none() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            }
            tracing::info!("admin clear corpse: {id}");
            let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
            outs.extend(self.broadcast_corpse(id));
            return outs;
        }

        // ── сундук: база → tombstone (глобально) + надстройка ──
        if let Some(id) = cmd.target.strip_prefix("container:") {
            let current = self.location_id.clone().unwrap_or_default();
            let defs_len = {
                let Ok(Some(loc_row)) = db::locations::get(&self.state.pool, &current).await else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "локации нет")];
                };
                let entities: Vec<Entity> =
                    serde_json::from_value(loc_row.entities).unwrap_or_default();
                entities
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| entity_loot_defs(e).len())
                    .unwrap_or(0)
            };
            {
                let mut world = self.state.world.lock().expect("мир");
                for i in 0..defs_len {
                    world.container_base_remove(id, i);
                }
                world.container_clear(id);
            }
            tracing::info!("admin clear container: {id} (база {defs_len})");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── магазин: контент → tombstone + надстройка ──
        if let Some(npc) = cmd.target.strip_prefix("shop:") {
            let content = self.state.content();
            let Some(shop) = content.shops.get(npc) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "магазина нет")];
            };
            let tabs: Vec<(usize, usize)> = shop
                .currencies
                .iter()
                .enumerate()
                .map(|(tab, currency)| (tab, currency.items.len()))
                .collect();
            {
                let mut world = self.state.world.lock().expect("мир");
                for (tab, len) in tabs {
                    for idx in 0..len {
                        world.shop_content_remove(npc, tab, idx);
                    }
                }
                world.shop_clear(npc);
            }
            tracing::info!("admin clear shop: {npc}");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        // ── монстр: весь живой лут (восстановится респавном) ──
        if let Some(id) = cmd.target.strip_prefix("monster:") {
            let ok = {
                let mut world = self.state.world.lock().expect("мир");
                world.mob_drops_clear(id)
            };
            if !ok {
                return vec![Self::err(seq, ErrorCode::TargetGone, "монстра нет")];
            }
            tracing::info!("admin clear monster: {id}");
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }

        vec![Self::err(seq, ErrorCode::TargetGone, "неизвестная цель")]
    }

    /// `cmd/admin.content.reload` (M27, фаза 1): перечитать каталог
    /// контента на работающем сервере — сид в БД (если версия изменилась),
    /// замена диалогов/магазинов/версии в памяти и сброс кэша карты.
    /// Только `role=admin` (§9 `E_PERMISSION`), аудит в лог.
    async fn admin_content_reload(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
    async fn admin_dialog_submit(
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
            if qty > 999 {
                // Валюта (type=money) — без потолка 999 (указание
                // заказчика); предел — INTEGER-стак в БД.
                let ty = db::items::def(&self.state.pool, def_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.item_type);
                if ty.as_deref() != Some("money") || qty > i64::from(i32::MAX) {
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
            if qty > 999 {
                // Валюта (type=money) — без потолка 999 (указание
                // заказчика); удаление и так зажимается остатком.
                let ty = self
                    .admin_item_def(target, item_ref)
                    .await
                    .map(|d| d.item_type);
                if ty.as_deref() != Some("money") || qty > i64::from(i32::MAX) {
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

    /// M24: выдать `qty` в инвентарь `target_char` — стак/слот по
    /// общим правилам (`take_loot_fits` + `grant_stacking`), сокеты
    /// новому экземпляру (паритет `scenario_give`); себе — события
    /// Out, другому (онлайн) — в его коннект (оффлайн увидит при
    /// входе — инвентарь из БД); аудит.
    async fn admin_give_to(
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
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        } else {
            event_text(
                "evt/item/added",
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
    async fn admin_remove_from(
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
                "evt/item/removed",
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
                "evt/item/state",
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
    async fn admin_give_corpse(
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
    async fn admin_remove_corpse_slot(
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
    async fn admin_give_monster(
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
    async fn admin_remove_monster(
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
    async fn admin_give_container(
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
    async fn admin_remove_container(
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
    async fn admin_give_shop(
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
    async fn admin_remove_shop_content(
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
    async fn admin_remove_shop_extra(
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

    /// `cmd/chat.send` (§7): dev-сценарии `/scenario …` (ФР-06/тесты);
    /// полноценный чат (ФР-13) — фаза M12.7.
    /// M14/F19: `cmd/chat.delete` — удаление сообщения из истории
    /// (только admin/moderator, §9 E_PERMISSION) + `evt/chat/deleted`
    /// всем онлайн (история общая — как system/global).
    async fn chat_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatDelete>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "удалять сообщения могут только админ и модератор",
            )];
        }
        match db::chat::delete(&self.state.pool, cmd.id as i64).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "сообщение не найдено",
                )];
            }
            Err(e) => {
                warn!("chat (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let payload = ld2_protocol::evt::ChatDeleted { id: cmd.id };
        if let Some(text) = event_payload_text("evt/chat/deleted", &payload) {
            let world = self.state.world.lock().expect("мир");
            for addr in world.online_chars() {
                world.send_to_char(&addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    async fn chat_send(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ChatSend>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if let Some(scenario) = cmd.text.strip_prefix("/scenario ") {
            // M24: дыра из бэклога — раньше префикс не проверялся
            // вовсе (любой игрок мог `/scenario give`); теперь —
            // админ либо явный флаг конфига `server.allow_scenario`
            // (dev/тесты); прод-конфиг без ключа → запрещено.
            if self.role != "admin" && !self.state.config.server.allow_scenario {
                return vec![Self::err(seq, ErrorCode::Permission, "сценарии отключены")];
            }
            return self.scenario(scenario.trim(), seq).await;
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Копия нужного из конфига (D25): cfg-заимствование мешает &mut ниже.
        let (channels, max_len, cooldown_s, history_limit) = {
            let chat = &self.state.config.gameplay.chat;
            (
                chat.channels.clone(),
                chat.max_len,
                chat.cooldown_s,
                chat.server_history,
            )
        };

        // Канал из конфига §5 (открытый список).
        if !channels.contains(&cmd.channel) {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                format!("нет канала: {}", cmd.channel),
            )];
        }
        // M14/F19: системный чат — только admin/moderator (§9 E_PERMISSION).
        if cmd.channel == "system" && !self.is_moderator() {
            return vec![Self::err(
                seq,
                ErrorCode::Permission,
                "недостаточно прав для system-канала",
            )];
        }
        // Длина текста (§12.2: chat.max_len → E_RATE_LIMITED).
        if cmd.text.chars().count() > max_len as usize {
            return vec![Self::err(
                seq,
                ErrorCode::RateLimited,
                format!("слишком длинное сообщение (до {max_len})"),
            )];
        }
        if cmd.text.trim().is_empty() {
            return vec![Self::err(seq, ErrorCode::Internal, "пустое сообщение")];
        }
        // Кулдаун (§12.2: chat.cooldown_s → E_RATE_LIMITED + retry_after_s).
        if let Some(last) = self.last_chat_at {
            let elapsed = last.elapsed().as_secs_f64();
            if elapsed < cooldown_s {
                let retry = (cooldown_s - elapsed).ceil().max(1.0) as u64;
                return vec![Self::ack_rate_limited(seq, retry)];
            }
        }

        // Рассылка: location — в локации, global/system — всем онлайн,
        // whisper — адресату (по имени) и автору, party/guild — участникам
        // (группы/гильдии — M12.7b/d: до тех пор только автор).
        let name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();
        let location = self.current_location(&char_id).await;
        // Каналы группы/гильдии (ФР-13): участникам своего состава.
        if cmd.channel == "party" {
            let members = {
                let social = self.state.social.lock().expect("соц");
                social.party_members(&char_id)
            };
            if members.is_empty() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")];
            }
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        if cmd.channel == "guild" {
            let guild = db::guilds::guild_of(&self.state.pool, &char_id)
                .await
                .ok()
                .flatten();
            let Some(guild) = guild else {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "вы не в гильдии",
                )];
            };
            let members = db::guilds::members(&self.state.pool, &guild.0)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<Vec<_>>();
            return self
                .chat_deliver(seq, &cmd, &char_id, &name, &members, history_limit)
                .await;
        }
        let recipients: Vec<String> = {
            let world = self.state.world.lock().expect("мир");
            match cmd.channel.as_str() {
                "location" => world.chars_in_location(&location),
                "global" | "system" => world.online_chars(),
                "whisper" => {
                    let target = cmd.to.as_deref().and_then(|to| world.char_id_by_name(to));
                    match target {
                        Some(id) if id != char_id => vec![char_id.clone(), id],
                        Some(_) => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::TargetGone,
                                "нельзя шептать себе",
                            )];
                        }
                        None => {
                            return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                        }
                    }
                }
                _ => vec![char_id.clone()],
            }
        };

        self.chat_deliver(seq, &cmd, &char_id, &name, &recipients, history_limit)
            .await
    }

    /// Общая доставка сообщения: история в БД (кольцо server_history,
    /// D25) → рассылка адресатам → ack (§2.2: ack раньше событий).
    async fn chat_deliver(
        &mut self,
        seq: u32,
        cmd: &ChatSend,
        char_id: &str,
        name: &str,
        recipients: &[String],
        history_limit: u32,
    ) -> Vec<Out> {
        let (msg_id, sent_at) = match db::chat::push(
            &self.state.pool,
            &cmd.channel,
            char_id,
            name,
            cmd.text.trim(),
            history_limit,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("chat (история): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        self.last_chat_at = Some(std::time::Instant::now());

        let payload = ld2_protocol::evt::ChatMessage {
            // M14/F19: id строки — для cmd/chat.delete (модерация).
            id: Some(msg_id.max(0) as u64),
            channel: cmd.channel.clone(),
            from: char_id.to_string(),
            name: name.to_string(),
            text: cmd.text.trim().to_string(),
            tags: None,
            sent_at,
            extra: Default::default(),
        };
        let text = event_payload_text("evt/chat/message", &payload);
        if let Some(text) = text {
            let world = self.state.world.lock().expect("мир");
            for addr in recipients {
                world.send_to_char(addr, text.clone());
            }
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// Реплей истории чата при выборе персонажа (§5 `server_history`):
    /// события `evt/chat/message` после snapshot (кольцо переживает рестарт).
    async fn chat_history_events(&self) -> Vec<Out> {
        let limit = self.state.config.gameplay.chat.server_history;
        if limit == 0 {
            return Vec::new();
        }
        let rows = match db::chat::recent(&self.state.pool, limit).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("chat (реплей истории): {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(id, channel, from, name, text, sent_at)| {
                // M14/F22: реплей помечается — клиент не показывает
                // прошлую сессию новому входу (кольцо D25 в БД живёт,
                // совместимость — через extra §11).
                let mut extra = serde_json::Map::new();
                extra.insert("replay".to_string(), serde_json::Value::Bool(true));
                let payload = ld2_protocol::evt::ChatMessage {
                    // M14/F19: id переживает рестарт — реплей тоже удаляем.
                    id: Some(id.max(0) as u64),
                    channel,
                    from,
                    name,
                    text,
                    tags: None,
                    sent_at,
                    extra,
                };
                event_payload_text("evt/chat/message", &payload).map(Out::Text)
            })
            .collect()
    }

    /// `/scenario jump <location_id>` (M25): телепорт в любую
    /// засеенную локацию — для обхода ад-карты (не связана с миром
    /// выходами). Персист + мир + события — паритет `location.move`.
    async fn scenario_jump(&mut self, loc_id: &str, seq: u32) -> Vec<Out> {
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
        self.location_id = Some(loc_id.to_string());
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, loc_id);
            world.end_combat(&char_id);
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

    /// `/scenario death` (ФР-06): общий поток смерти (death.rs) — теряется
    /// **только лут**, опыт/пул сохраняются (skills.md §2.2); респаун в спавне.
    async fn scenario(&mut self, scenario: &str, seq: u32) -> Vec<Out> {
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

    /// `/scenario give <def_id>` (dev): выдать предмет персонажу —
    /// сокеты посоха создаются сразу (ADR-002, иначе UI не увидит слоты).
    /// `/scenario kit` (M16/F29b): тестовый набор в рюкзак — предметы
    /// разных типов и редкостей с улучшениями +N, полная экипировка
    /// (слоты из конфига) и стак монет. Наполнение окна рюкзака для
    /// смоука; повторный вызов добавляет стаки (слоты переносятся).
    async fn scenario_kit(&mut self, seq: u32) -> Vec<Out> {
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

    async fn scenario_give(&mut self, def_id: &str, seq: u32) -> Vec<Out> {
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
        if db::items::count(pool, &char_id).await.unwrap_or(i64::MAX)
            >= i64::from(self.state.config.gameplay.inventory.slots)
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
    async fn scenario_gold(&mut self, amount: &str, seq: u32) -> Vec<Out> {
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
    async fn scenario_crime(&mut self, kind: &str, seq: u32) -> Vec<Out> {
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

    // ── сделка (ФР-15, M12.7c) ──────────────────────────────────────────────

    // ── гильдия (ФР-16, M12.7d) ─────────────────────────────────────────────

    /// Передать лидерство первому участнику при выходе лидера.
    async fn promote_first_member(&self, leaving: &str) -> Result<(), sqlx::Error> {
        let Some(guild) = db::guilds::guild_of(&self.state.pool, leaving).await? else {
            return Ok(());
        };
        let members = db::guilds::members(&self.state.pool, &guild.0).await?;
        let Some(first) = members
            .into_iter()
            .map(|(id, _, _)| id)
            .find(|id| id != leaving)
        else {
            return Ok(());
        };
        db::guilds::set_rank(&self.state.pool, &guild.0, &first, "leader").await?;
        Ok(())
    }

    // ── группа (ФР-14, M12.7b) ──────────────────────────────────────────────

    /// Текущее имя персонажа (для участников группы §8).
    fn current_name(&self) -> String {
        self.char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let char_id = self.selected_char_id.clone()?;
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default()
    }

    // ── профессии и крафт (ФР-08, M12.6) ───────────────────────────────────

    // ── диалоги NPC (M18/F35; ветки/реплики F37) ──────────────────────────

    // ── магазины NPC (M18/F39) ─────────────────────────────────────────────

    // ── квесты (ФР-12, M12.6) ───────────────────────────────────────────────
}

/// M18/F58d (ADR-012 §5): пересобрать кэш рун экипированного оружия
/// в `world` — из raw-сокетов `main_hand` + параметров рун (`stats`:
/// `chance`/`damage`/`cost`/`buff`). Руны без `chance` в авто-приём
/// не участвуют. Вызывается на входе, экипировке, инкрустации/извлечении,
/// касте и из тикера зарядов (charges в кэше должен совпадать с БД).
pub(crate) async fn refresh_char_runes(state: &crate::state::SharedState, char_id: &str) {
    use crate::world::{RuneBuff, SocketedRune};

    // M24: класс руны (stats.class) — авто-приём только своего класса
    // (нет поля — любой, §11); профиль запрашиваем один раз.
    let profile = db::characters::get(&state.pool, char_id)
        .await
        .ok()
        .flatten()
        .map(|r| r.profile);
    let Some(main) = db::items::find_equipped_in_slot(&state.pool, char_id, "main_hand")
        .await
        .ok()
        .flatten()
    else {
        let mut world = state.world.lock().expect("мир");
        world.set_char_runes(char_id, Vec::new());
        return;
    };
    let slots: Vec<Value> = serde_json::from_value(main.sockets.clone()).unwrap_or_default();
    let mut runes: Vec<SocketedRune> = Vec::new();
    for slot in &slots {
        let Some(rune) = slot.get("rune").filter(|r| !r.is_null()) else {
            continue;
        };
        let Some(rune_def_id) = slot.get("def_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(Some(rune_def)) = db::items::def(&state.pool, rune_def_id).await else {
            continue;
        };
        // M24: класс руны — авто-приём только своего класса.
        if let Some(class) = rune_def.stats.get("class").and_then(|v| v.as_str()) {
            if profile.as_deref() != Some(class) {
                continue;
            }
        }
        // Шанс — контент-параметр: без него руна не участвует в
        // авто-приёме (прежние руны продолжают работать кастом).
        let Some(chance) = rune_def.stats.get("chance").and_then(|v| v.as_f64()) else {
            continue;
        };
        if !chance.is_finite() || chance <= 0.0 {
            continue;
        }
        let buff = rune_def
            .stats
            .get("buff")
            .and_then(|b| b.as_object())
            .map(|b| RuneBuff {
                kind: rune_def.id.clone(),
                title: rune_def.name.clone(),
                param: b
                    .get("param")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                value: b.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
                pct: b.get("pct").and_then(|v| v.as_bool()).unwrap_or(false),
                duration_s: b.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(10),
                max_stacks: b.get("max_stacks").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
            });
        // M18/F58d: эффект на моба (stats.effect): stun/root/bleed.
        let effect = rune_def
            .stats
            .get("effect")
            .and_then(|e| e.as_object())
            .and_then(|e| {
                let kind = e.get("kind")?.as_str()?.to_string();
                Some(crate::world::RuneEffect {
                    kind,
                    value: e.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
                    duration_s: e.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(5),
                    period_s: e
                        .get("period_s")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(1)
                        .max(1),
                })
            });
        runes.push(SocketedRune {
            staff_uid: main.uid.clone(),
            socket_index: slot.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            def_id: rune_def.id.clone(),
            spell: rune
                .get("spell")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            chance,
            damage: rune_def
                .stats
                .get("damage")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            cost: crate::systems::rune_charge_cost(&rune_def.stats),
            charges: rune.get("charges").and_then(|v| v.as_i64()).unwrap_or(0),
            buff,
            effect,
        });
    }
    let mut world = state.world.lock().expect("мир");
    world.set_char_runes(char_id, runes);
}

impl Session {}

/// Стартовые профили `char.create` (ADR-003: Воин/Лучник/Маг — M17/F30b
/// «Вор» переименован в «Лучник»); расширение списка — capability (§7).
const PROFILES: &[&str] = &["warrior", "archer", "mage"];

/// Ack → текст конверта (§2.2).
fn out_ack(ack: Ack) -> Out {
    Out::Text(serde_json::to_string(&ack).expect("ack сериализуется"))
}

/// Событие → текст конверта (§2.3) с `ts`; сверх лимита §1 — закрытие
/// (нарезка крупных событий — M12.8, §1).
pub(crate) fn out_event(ty: &str, state_version: Option<u64>, payload: Value) -> Out {
    let text = event_text(ty, state_version, payload);
    if text.is_empty() {
        Out::Close
    } else {
        Out::Text(text)
    }
}

/// Событие в список адресованной рассылки (адресат, текст) — планы боя/каста.
fn push_evt<T: serde::Serialize>(
    plan: &mut Vec<(String, String)>,
    addr: &str,
    ty: &str,
    payload: &T,
) {
    if let Some(text) = event_payload_text(ty, payload) {
        plan.push((addr.to_string(), text));
    }
}

/// JSON-текст конверта события из payload'а (`None` — сверх лимита §1).
fn event_payload_text<T: serde::Serialize>(ty: &str, payload: &T) -> Option<String> {
    let text = event_text(ty, None, serde_json::to_value(payload).unwrap_or_default());
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Текст конверта события (§2.3); пустая строка — сверх лимита §1
/// (death.rs адресует события строками).
pub(crate) fn event_text(ty: &str, state_version: Option<u64>, payload: Value) -> String {
    let mut env = Envelope::new(ty, Some(payload));
    env.state_version = state_version;
    env.ts = Some(crate::time::now_rfc3339());
    env.to_json_limited().unwrap_or_else(|e| {
        warn!("событие {ty} не влезает в лимит 64 KiB: {e}");
        String::new()
    })
}

/// Первые 12 символов sha256-версии контента — для компактного лога.
fn short_version(version: &str) -> &str {
    version.get(..12).unwrap_or(version)
}
