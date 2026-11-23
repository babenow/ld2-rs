//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::cmd::{
    AdminClear, AdminContentReload, AdminGiveAsk, AdminGodMode, AdminItems, AdminRemoveAsk,
    AdminTargetInfo, ChatDelete, ChatSend, CraftLearn, CraftRankUp, CraftRecipes, CraftReset,
    CraftStart, CrimeLog, GuildBrowse, GuildCreate, GuildInvite, GuildJoin, GuildKick,
    GuildRankSet, MagicRuneCast, MagicScrollCast, NpcReply, NpcTalk, PartyAccept, PartyInvite,
    PartyKick, ResourceGather, ShopBuy, ShopList, SocketExtract, SocketInsert, TradeAddItem,
    TradeCancel, TradeOpen, TradePropose, TradeSetGold, UpgradePreview, UpgradeStart,
};
use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{
    CastSource, CharXp, CombatHit, CombatKind, CraftProfession, CrimeAdded, EntityDespawn,
    EntityHp, EntityMove, ItemAdded, ItemRemovalReason, ItemRemoved, ItemState, LocationChanged,
    MagicCast, Pong, TimeSync, UpgradeResult,
};
use ld2_protocol::types::{
    CharacterState, CrimeLogEntry, CrimeStatus, Entity, Hp, ProfessionSnapshot, Severity,
};
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use crate::content::ContentInput;
use crate::db;
use crate::state::SharedState;
use crate::trainers;

mod auth;
mod chars;
mod combat;
mod entity;
mod gates;
mod items;
mod location;
mod rules;

use rules::{
    content_bonuses, entity_loot_defs, location_snapshot, recipe_payload, rune_slots,
    socket_accepts, UpgradePreviewState,
};
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

    /// Снятие шкур: старт разделки — сервер-джоб на5 с (тиктаймер
    /// шлёт `evt/harvest/result`); все проверки — до старта:
    /// труп в локации с флагом действия и не сделан, профессия
    /// изучена, нож в инвентаре, гейт уровней (треб − уровень <25),
    /// нет активного джоба на этом трупе. M24: старт на **другом**
    /// трупе разрешён — текущая разделка прерывается, шкура
    /// брошенного трупа портится (для gut и skin одинаково).
    async fn corpse_harvest(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::CorpseHarvest>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.action != "skin" && cmd.action != "gut" {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректное действие")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.current_location(&char_id).await;
        // Труп, флаги, «уже сделано» — в мире.
        let (name, required) = {
            let world = self.state.world.lock().expect("мир");
            // M24: переключение на ДРУГОЙ труп разрешено — текущая
            // разделка прерывается ниже (шкура брошенного трупа
            // портится); повторный старт на том же трупе — E_COOLDOWN.
            let prev = world.harvest_job(&char_id).map(|j| j.corpse_id.clone());
            if prev.as_deref() == Some(cmd.entity_id.as_str()) {
                return vec![Self::err(
                    seq,
                    ErrorCode::Cooldown,
                    "вы уже разделываете труп",
                )];
            }
            let Some(corpse) = world.corpse(&cmd.entity_id) else {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            };
            if corpse.location_id != current {
                return vec![Self::err(seq, ErrorCode::TargetGone, "трупа нет")];
            }
            let Some(harv) = corpse.harvest.as_ref() else {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "это животное разделывать нельзя",
                )];
            };
            let allowed = harv
                .get(&cmd.action)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !allowed {
                return vec![Self::err(
                    seq,
                    ErrorCode::TargetGone,
                    "это животное разделывать нельзя",
                )];
            }
            let done = if cmd.action == "skin" {
                corpse.skin_done
            } else {
                corpse.gut_done
            };
            if done {
                return vec![Self::err(seq, ErrorCode::ItemGone, "уже разделано")];
            }
            let required = harv
                .get("level")
                .and_then(|v| v.as_i64())
                .unwrap_or(1)
                .clamp(1, 100) as u32;
            let name = corpse
                .name
                .strip_prefix("Труп: ")
                .unwrap_or(&corpse.name)
                .to_string();
            (name, required)
        };
        // Профессия изучена → уровень (гейт: треб − уровень <25).
        let learned = match db::professions::learned(&self.state.pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("harvest (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some((_, _, _, level)) = learned.into_iter().find(|(id, _, _, _)| id == "skinning")
        else {
            return vec![Self::err(
                seq,
                ErrorCode::Profession,
                "профессия не изучена",
            )];
        };
        let level = level.max(0) as u32;
        if required.saturating_sub(level) >= 25 {
            return vec![Self::err(
                seq,
                ErrorCode::LevelRequired,
                format!(
                    "не хватает {} уровней (требуется {required})",
                    required.saturating_sub(level)
                ),
            )];
        }
        // Нож для снятия шкур (паритет инструмента крафта).
        let has_knife =
            match db::items::has_item_type(&self.state.pool, &char_id, "skinning_knife").await {
                Ok(v) => v,
                Err(e) => {
                    warn!("harvest (нож): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        if !has_knife {
            return vec![Self::err(
                seq,
                ErrorCode::ToolMissing,
                "нужен нож для снятия шкур",
            )];
        }
        // M24: переключение — текущая разделка прерывается, шкура
        // брошенного трупа испорчена (для gut и skin одинаково:
        // снять шкуру после нельзя, повторить gut можно). Итог и
        // обновление трупа шлём ДО ack: клиент гасит бар по
        // evt/harvest/result и только потом получает старт нового.
        let interrupted = {
            let mut world = self.state.world.lock().expect("мир");
            let old = world.cancel_harvest(&char_id);
            if let Some(job) = &old {
                world.ruin_skin(&job.corpse_id);
            }
            old
        };
        let mut outs: Vec<Out> = Vec::new();
        if let Some(job) = interrupted {
            let result = ld2_protocol::evt::HarvestResult {
                entity_id: job.corpse_id.clone(),
                name: job.name.clone(),
                action: job.action.clone(),
                ok: false,
                ruined: true,
            };
            if let Some(text) = event_payload_text("evt/harvest/result", &result) {
                outs.push(Out::Text(text));
            }
            // Флаги «шкура испорчена» — всем в локации (окно подятия).
            outs.extend(self.broadcast_corpse(&job.corpse_id));
        }
        // Старт джоба (5 с).
        let at = crate::time::now_epoch() + 5;
        {
            let mut world = self.state.world.lock().expect("мир");
            world.start_harvest(crate::world::HarvestJob {
                char_id: char_id.clone(),
                corpse_id: cmd.entity_id.clone(),
                action: cmd.action.clone(),
                name: name.clone(),
                at,
            });
        }
        outs.push(Self::ack_payload(
            seq,
            &serde_json::json!({ "action": cmd.action, "name": name }),
        ));
        outs
    }

    /// M20/F20b: добыча ресурса — инструмент (`item_type` в
    /// инвентаре), ранг профессии, выдача 1 шт. + XP (паритет крафта),
    /// уменьшение запаса; патч шкалы/десктронизация — всем в локации.
    async fn resource_gather(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ResourceGather>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        // 1) узел: в текущей локации, видим, запас жив.
        let node = {
            let world = self.state.world.lock().expect("мир");
            match world.resource(&cmd.entity_id) {
                Some(n) if n.location_id == current && !n.hidden && n.current > 0 => n.clone(),
                _ => {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "ресурс недоступен")];
                }
            }
        };
        // 2) инструмент: предмет с item_type (паритет инструмента крафта).
        if let Some(tool) = &node.tool {
            let has = match db::items::has_item_type(pool, &char_id, tool).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("gather (инструмент): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if !has {
                return vec![Self::err(seq, ErrorCode::ToolMissing, "нужен инструмент")];
            }
        }
        // 3) ранг/уровень профессии (профессия должна быть изучена).
        let mut prof_xp: Option<(String, i64, i32, i32)> = None;
        if let Some(prof) = &node.profession {
            let learned = match db::professions::learned(pool, &char_id).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("gather (профессии): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            match learned.iter().find(|(id, _, _, _)| id == prof) {
                Some((_, xp, rank, level)) if *rank >= node.min_rank => {
                    prof_xp = Some((prof.clone(), *xp, *rank, *level));
                }
                Some(_) => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Profession,
                        "ранг профессии ниже требуемого",
                    )];
                }
                None => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::Profession,
                        "профессия не изучена",
                    )];
                }
            }
        }
        // 4) место в инвентаре до уменьшения запаса (паритет take_loot).
        let slots = self.state.config.gameplay.inventory.slots;
        match Self::take_loot_fits(pool, &char_id, &node.def_id, slots).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::InventoryFull,
                    "инвентарь заполнен",
                )];
            }
            Err(e) => {
                warn!("gather (слоты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        // 5) добыча в мире: −1 запас (при 0 — респавн, трава — скрытие).
        let after = {
            let mut world = self.state.world.lock().expect("мир");
            world.try_gather(&cmd.entity_id, &current)
        };
        let Some(after) = after else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "ресурс исчерпан")];
        };
        // 6) выдача предмета (место проверено выше).
        let Some(def) = (match db::items::def(pool, &node.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("gather (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения")];
        };
        let (row, stacked) = match db::items::grant_stacking(pool, &char_id, &node.def_id, 1).await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("gather (выдача): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        let item = item_snapshot(&row, &def);
        outs.push(if stacked {
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(row.qty));
            out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: row.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            )
        } else {
            out_event(
                "evt/item/added",
                None,
                serde_json::to_value(ItemAdded { item }).unwrap_or_default(),
            )
        });
        // 7) XP профессии (паритет крафта) → снапшот себе.
        if let Some((prof, xp_old, rank, level)) = prof_xp {
            let cfg = &self.state.config.gameplay.professions;
            let gain = cfg.xp_per_craft_base + cfg.xp_per_craft_per_rank * i64::from(rank);
            // M21: опыт растит УРОВЕНЬ; ранг меняет только тренер/квест.
            let (xp_new, level_new) =
                crate::systems::profession_level_up(cfg, xp_old, level.max(0) as u32, gain);
            if let Err(e) = db::professions::grant_progress(
                pool,
                &char_id,
                &prof,
                xp_new,
                rank,
                level_new as i32,
            )
            .await
            {
                warn!("gather (XP профессии {char_id}): {e}");
            } else {
                let snapshot =
                    crate::systems::profession_snapshot(cfg, &prof, xp_new, rank, level_new);
                let name = db::professions::profession_name(pool, &prof)
                    .await
                    .ok()
                    .flatten();
                let text = event_payload_text(
                    "evt/craft/profession",
                    &CraftProfession {
                        profession: snapshot,
                        name,
                        gain: Some(gain),
                    },
                );
                if let Some(text) = text {
                    outs.push(Out::Text(text));
                }
            }
        }
        // 8) запас в локации: трава исчезла → despawn, жила/дерево →
        // патч шкалы (паттерн entity/move).
        let world_evt = if after.hidden {
            event_payload_text(
                "evt/entity/despawn",
                &EntityDespawn {
                    entity_id: after.id.clone(),
                    location_id: after.location_id.clone(),
                    kind: Some(ld2_protocol::types::EntityKind::Resource),
                    picked_by: None,
                    picked_gender: None,
                },
            )
        } else {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "hp".into(),
                serde_json::json!({"cur": after.current, "max": after.max}),
            );
            event_payload_text(
                "evt/entity/move",
                &EntityMove {
                    entity_id: after.id.clone(),
                    patch,
                },
            )
        };
        {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&after.location_id) {
                if let Some(text) = &world_evt {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }
        outs
    }

    /// M20 (заготовка): выбор действия в многостраничном диалоге —
    /// подтверждение без переходов (серверные переходы появятся с
    /// контентом игровых диалогов); ack с echo выбора.
    async fn dialog_choice(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::DialogChoice>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        tracing::debug!(
            "dialog.choice: dialog={} page={} action={}",
            cmd.dialog_id,
            cmd.page_id,
            cmd.action_id
        );
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog_id": cmd.dialog_id,
                "page_id": cmd.page_id,
                "action_id": cmd.action_id,
            }),
        )]
    }

    /// M20 (заготовка): ввод execute-диалога — сервер принимает набор
    /// значений и возвращает его в ack (обратная связь «ввод получен»);
    /// игровая обработка (квесты/настройки) подключится позже.
    async fn dialog_submit(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<ld2_protocol::cmd::DialogSubmit>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        // M24: админ-диалоги количества (выдача/удаление) — своя ветка;
        // прочие диалоги — прежний echo (заготовка M20).
        if let Some(rest) = cmd.dialog_id.strip_prefix("admin.") {
            return self.admin_dialog_submit(rest, &cmd.values, seq).await;
        }
        tracing::debug!(
            "dialog.submit: dialog={} values={:?}",
            cmd.dialog_id,
            cmd.values
        );
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog_id": cmd.dialog_id,
                "values": cmd.values,
            }),
        )]
    }

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

    /// `cmd/socket.insert` (§7, ADR-002): руна → свободный сокет посоха.
    /// ack `{staff}`; руна расходуется (`evt/item/removed`/qty), сокеты —
    /// патчем `evt/item/state`.
    async fn socket_insert(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
        let mut result = vec![Self::ack_payload(seq, &Value::Object(payload))];
        result.extend(outs);
        result
    }

    /// `cmd/socket.extract` (§7): извлечь руну → в инвентарь (заряды
    /// сбрасываются к максимуму — отклонение M12.5b: per-item charges нет).
    /// ack `{staff, rune}`.
    async fn socket_extract(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
        vec![Self::ack_payload(seq, &Value::Object(payload))]
    }

    /// `cmd/magic.rune_cast` (§7, D4): мана + заряд руны → урон цели
    /// (`stats.damage`), `evt/magic/cast` всем в локации, XP убийства — в
    /// школу (`stats.school`). Ошибки: `E_NO_RUNE_SLOT`, `E_NO_CHARGES`,
    /// `E_NOT_ENOUGH_MP`, `E_TARGET_GONE`.
    async fn magic_rune_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicRuneCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(mut staff) = (match db::items::find(&pool, &cmd.staff_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("rune_cast (поиск посоха): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        let Some(staff_def) = (match db::items::def(&pool, &staff.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения посоха",
            )];
        };
        let sockets: Vec<Value> = serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        let Some(slot) = sockets.get(cmd.socket_index as usize) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "нет такого сокета")];
        };
        let Some(rune_info) = slot.get("rune").filter(|r| !r.is_null()) else {
            return vec![Self::err(seq, ErrorCode::NoRuneSlot, "сокет пуст")];
        };
        let charges = rune_info
            .get("charges")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let rune_def_id = slot
            .get("def_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(rune_def) = (match db::items::def(&pool, &rune_def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("rune_cast (определение руны): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет определения руны")];
        };
        // M18/F58c (ADR-012 §4): затрата — поле руны `cost` (деф. 1),
        // зарядов должно хватать на неё целиком.
        let cost_charges = crate::systems::rune_charge_cost(&rune_def.stats);
        if charges < cost_charges {
            return vec![Self::err(seq, ErrorCode::NoCharges, "руна разряжена")];
        }
        // M25 (заказчик): каст с любого предмета с сокетами — панель
        // показывает руны мечей, активация должна работать (раньше
        // «только посох» — E_NO_RUNE_SLOT на кнопке меча).
        if staff_def.sockets == 0 {
            return vec![Self::err(
                seq,
                ErrorCode::NoRuneSlot,
                "у предмета нет сокетов",
            )];
        }
        // M24: класс руны (stats.class) — каст только своим классом
        // (нет поля — любой, §11). После проверки сокетов.
        if let Some(class) = rune_def.stats.get("class").and_then(|v| v.as_str()) {
            let profile = db::characters::get(&pool, &char_id)
                .await
                .ok()
                .flatten()
                .map(|r| r.profile);
            if profile.as_deref() != Some(class) {
                let need = match class {
                    "mage" => "маг",
                    "warrior" => "воин",
                    "archer" => "лучник",
                    other => other,
                };
                return vec![Self::err(
                    seq,
                    ErrorCode::RequirementAttr,
                    format!("руна требует класс: {need}"),
                )];
            }
        }
        // M17/F30g: кулдаун каста руны (cooldown_s, дефолт 1 с).
        if let Some(err) = self.item_cd_gate(seq, &char_id, &rune_def).await {
            return vec![err];
        }

        // Мана (D4: каст тратит ману И заряд).
        let cost = i64::from(self.state.config.gameplay.magic.mana.rune_cost);
        let Some(row) = (match db::characters::get(&pool, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("rune_cast (персонаж): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        if i64::from(row.mp_cur) < cost {
            return vec![Self::err(seq, ErrorCode::NotEnoughMp, "не хватает маны")];
        }
        let spell = rune_info
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = rune_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = rune_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Цель (опциональна): моб должен быть в локации персонажа.
        let location = self.current_location(&char_id).await;
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        // Расход: мана и заряд руны (D27 — сразу в БД).
        let new_mp = (i64::from(row.mp_cur) - cost).max(0) as i32;
        if let Err(e) = db::characters::set_mp(&pool, &char_id, new_mp).await {
            warn!("rune_cast (mp): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let mut sockets: Vec<Value> =
            serde_json::from_value(staff.sockets.clone()).unwrap_or_default();
        if let Some(slot) = sockets.get_mut(cmd.socket_index as usize) {
            if let Some(r) = slot.get_mut("rune") {
                r["charges"] = serde_json::json!(charges - cost_charges);
            }
        }
        let sockets_value = Value::Array(sockets);
        if let Err(e) = db::items::set_sockets(&pool, &staff.uid, sockets_value.clone()).await {
            warn!("rune_cast (заряды): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        // M18/F58d: кэш рун — charges изменились.
        refresh_char_runes(&self.state, &char_id).await;
        // M18/F58c: клиенту — патч сокетов (иначе заряды на кнопке
        // отстают до ресинка; сокеты в wire-виде — с `accepts`, §6).
        staff.sockets = sockets_value;
        let mut item_patch = serde_json::Map::new();
        item_patch.insert(
            "sockets".into(),
            serde_json::to_value(
                item_snapshot(&staff, &staff_def)
                    .sockets
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null),
        );
        let charge_event = out_event(
            "evt/item/state",
            None,
            serde_json::to_value(ItemState {
                uid: staff.uid.clone(),
                patch: item_patch,
            })
            .unwrap_or_default(),
        );

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();

        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Rune,
            target: cmd.target_id.clone(),
            charges_left: Some((charges - cost_charges) as i32),
            mp_left: Some(new_mp),
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text("evt/magic/cast", &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if !matches!(&charge_event, Out::Text(t) if t.is_empty()) {
            mine.push(charge_event);
        }
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }

        // cast-событие — всем в локации (свой — уже в `mine`).
        let mut others: Vec<(String, String)> = Vec::new();
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    others.push((addr, text.clone()));
                }
            }
        }
        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    // M17/F30f: строка со словами заклинания — кастеру
                    // «вы», наблюдателям — имя; урон жирный (§8).
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Rune,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: String::new(),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        let mut per = hit.clone();
                        per.text = crate::combat_log::spell_hit_text(
                            &spell,
                            &by_name,
                            &mob_name,
                            damage,
                            addr == char_id,
                        );
                        plan.push((
                            addr.clone(),
                            event_payload_text("evt/combat/hit", &per).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                "evt/entity/hp",
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            // XP убийства — в школу заклинания (skills.md §2.3).
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
                if text.is_empty() {
                    continue;
                }
                if addr == char_id {
                    mine.push(Out::Text(text));
                } else {
                    others.push((addr, text));
                }
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        {
            let world = self.state.world.lock().expect("мир");
            for (addr, text) in others {
                world.send_to_char(&addr, text);
            }
        }
        // M17/F30g: кулдаун каста → таймер кнопки.
        mine.push(self.item_cd_started(&char_id, &rune_def));
        mine
    }

    /// `cmd/magic.scroll_cast` (§7): свиток расходуется, мана не тратится;
    /// `scroll_requires_target` → цель обязательна (`E_TARGET_GONE`).
    async fn magic_scroll_cast(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MagicScrollCast>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(scroll) = (match db::items::find(&pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("scroll_cast (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "свитка нет")];
        };
        let Some(scroll_def) = (match db::items::def(&pool, &scroll.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("scroll_cast (определение): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "нет определения свитка",
            )];
        };
        if scroll_def.item_type != "scroll" {
            return vec![Self::err(seq, ErrorCode::ItemGone, "это не свиток")];
        }
        // M17/F30g: кулдаун каста свитка (cooldown_s, дефолт 1 с).
        if let Some(err) = self.item_cd_gate(seq, &char_id, &scroll_def).await {
            return vec![err];
        }

        let location = self.current_location(&char_id).await;
        let requires_target = self.state.config.gameplay.magic.scroll_requires_target;
        if requires_target && cmd.target_id.is_none() {
            return vec![Self::err(seq, ErrorCode::TargetGone, "цель не указана")];
        }
        if let Some(target) = cmd.target_id.as_deref() {
            let alive = {
                let world = self.state.world.lock().expect("мир");
                world.mob_location(target).as_deref() == Some(location.as_str())
            };
            if !alive {
                return vec![Self::err(seq, ErrorCode::TargetGone, "цель недоступна")];
            }
        }

        let spell = scroll_def
            .stats
            .get("spell")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let damage = scroll_def
            .stats
            .get("damage")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let school = scroll_def
            .stats
            .get("school")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Расход свитка — до событий (§7: consumable).
        let mut consumable = Vec::new();
        if scroll.qty > 1 {
            let new_qty = i64::from(scroll.qty) - 1;
            if let Err(e) = db::items::set_qty(&pool, &scroll.uid, new_qty).await {
                warn!("scroll_cast (qty): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            let mut patch = serde_json::Map::new();
            patch.insert("qty".into(), serde_json::json!(new_qty));
            consumable.push(out_event(
                "evt/item/state",
                None,
                serde_json::to_value(ItemState {
                    uid: scroll.uid.clone(),
                    patch,
                })
                .unwrap_or_default(),
            ));
        } else {
            if let Err(e) = db::items::delete(&pool, &scroll.uid).await {
                warn!("scroll_cast (удаление): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
            consumable.push(out_event(
                "evt/item/removed",
                None,
                serde_json::to_value(ItemRemoved {
                    uid: scroll.uid.clone(),
                    reason: ItemRemovalReason::Consumed,
                })
                .unwrap_or_default(),
            ));
        }

        let by_name = self
            .char
            .as_ref()
            .map(|c| c.name.clone())
            .or_else(|| {
                let world = self.state.world.lock().expect("мир");
                world.char_name(&char_id)
            })
            .unwrap_or_default();
        let cast = MagicCast {
            by: char_id.clone(),
            by_name: by_name.clone(),
            spell: spell.clone(),
            what: CastSource::Scroll,
            target: cmd.target_id.clone(),
            charges_left: None,
            mp_left: None,
            ok: true,
            extra: Default::default(),
        };
        let cast_json = event_payload_text("evt/magic/cast", &cast);

        let mut mine: Vec<Out> = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(text) = &cast_json {
            mine.push(Out::Text(text.clone()));
        }
        mine.extend(consumable);

        // cast-событие — остальным в локации (свой уже в `mine`).
        if let Some(text) = &cast_json {
            let world = self.state.world.lock().expect("мир");
            for addr in world.chars_in_location(&location) {
                if addr != char_id {
                    world.send_to_char(&addr, text.clone());
                }
            }
        }

        let mut xp_work: Option<crate::tick::KillXp> = None;
        if let Some(target) = cmd.target_id.as_deref() {
            let skills_cfg = &self.state.config.skills;
            let mut plan: Vec<(String, String)> = Vec::new();
            {
                let mut world = self.state.world.lock().expect("мир");
                if let Some(hp) = world.damage_mob(target, damage) {
                    let mob_name = world
                        .mob(target)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let mob_max = world.mob_max_hp(target);
                    // M17/F30f: свиток кастует заклинание — слова и
                    // «чем бьёт» как у руны (§8).
                    let hit = CombatHit {
                        source: char_id.clone(),
                        source_name: by_name.clone(),
                        target: target.to_string(),
                        target_name: mob_name.clone(),
                        kind: CombatKind::Scroll,
                        amount: damage,
                        crit: false,
                        miss: false,
                        text: String::new(),
                        extra: Default::default(),
                    };
                    for addr in world.chars_in_location(&location) {
                        let mut per = hit.clone();
                        per.text = crate::combat_log::spell_hit_text(
                            &spell,
                            &by_name,
                            &mob_name,
                            damage,
                            addr == char_id,
                        );
                        plan.push((
                            addr.clone(),
                            event_payload_text("evt/combat/hit", &per).unwrap_or_default(),
                        ));
                        plan.push((
                            addr,
                            event_payload_text(
                                "evt/entity/hp",
                                &EntityHp {
                                    entity_id: target.to_string(),
                                    hp: Hp {
                                        cur: hp,
                                        max: mob_max,
                                    },
                                },
                            )
                            .unwrap_or_default(),
                        ));
                    }
                    if hp == 0 {
                        xp_work = crate::tick::mob_kill_effects(
                            &mut world, &mut plan, target, &location, &char_id, skills_cfg,
                        );
                        if let Some(work) = &mut xp_work {
                            work.skill = school.clone();
                        }
                    }
                }
            }
            for (addr, text) in plan {
                if text.is_empty() {
                    continue;
                }
                if addr == char_id {
                    mine.push(Out::Text(text));
                } else {
                    let world = self.state.world.lock().expect("мир");
                    world.send_to_char(&addr, text);
                }
            }
        }
        if let Some(work) = &xp_work {
            if let Some(text) = crate::tick::grant_kill_xp(&self.state, work).await {
                mine.push(Out::Text(text));
            }
            if let Some(target) = &work.target {
                for (_, text) in
                    crate::tick::quest_kill_progress(&self.state, &char_id, target).await
                {
                    if !text.is_empty() {
                        mine.push(Out::Text(text));
                    }
                }
            }
        }
        // M17/F30g: кулдаун каста → таймер кнопки.
        mine.push(self.item_cd_started(&char_id, &scroll_def));
        mine
    }

    /// Пустые слоты посоха (форма хранения JSONB, ADR-002).
    fn empty_sockets(count: i32) -> Value {
        Value::Array(Self::empty_sockets_list(count))
    }

    fn empty_sockets_list(count: i32) -> Vec<Value> {
        (0..count.max(0))
            .map(|i| serde_json::json!({ "index": i, "rune": null }))
            .collect()
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

    /// M18/F54: активный камень усиления — предмет (`upgrade_stone`)
    /// из инвентаря текущего персонажа; `None` — не выбран или потерян
    /// (выброшен/продан после выбора).
    async fn active_stone(
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

    /// M18/F54: качество камня (`stats.quality`, процентные пункты;
    /// ключа нет → 0 — нейтральный «средний» камень).
    fn stone_quality(def: &db::items::ItemDefRow) -> i32 {
        def.stats
            .get("quality")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32
    }

    /// M18/F54: итоговый шанс заточки — база конфига + бонус камня
    /// (затухание 1/(1+level)) × рейты сервера.
    fn sharpen_chance(&self, base: f64, stone_quality: i32, level: i32) -> f64 {
        let rates = self.state.config.gameplay.rates.chance;
        crate::systems::upgrade_chance_with_stone(
            base,
            i64::from(stone_quality),
            i64::from(level),
            rates,
        )
    }

    /// `cmd/upgrade.preview` (§7, ФР-10): превью «сейчас → после» —
    /// `preview_id`, `delta.upgrade`, `cost.gold`, `chance` (если
    /// `shows_chance`). `E_UPGRADE_MAX` — уровень уже предельный.
    async fn upgrade_preview(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
    async fn upgrade_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

        // ack → события (§2.2).
        let mut result = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        result.extend(outs);
        result
    }

    /// F29a: события списания стаков монет — патч `qty` уцелевшего
    /// стака либо `evt/item/removed` опустевшего (Consumed).
    fn money_events(changes: Vec<(String, Option<i64>)>) -> Vec<Out> {
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

    /// `cmd/crime.log` (§7): журнал нарушений (новые первые), limit —
    /// из payload (по умолчанию 50, максимум 500).
    async fn crime_log(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CrimeLog>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let limit = cmd.limit.unwrap_or(50).clamp(1, 500) as i64;
        let entries = match db::crime::log(&self.state.pool, &char_id, limit).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(kind, at, delta_s, total_s)| CrimeLogEntry {
                    kind,
                    at,
                    delta_s,
                    total_s,
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("crime.log: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "entries": entries }),
        )]
    }

    /// Статус из строки БД: severity — из `crime.kinds` конфига (D25).
    fn crime_status(&self, kind: &str, expires_at: i64) -> CrimeStatus {
        let severity = self
            .state
            .config
            .gameplay
            .crime
            .kinds
            .iter()
            .find(|k| k.kind == kind)
            .map(|k| crate::auth::severity(&k.severity))
            .unwrap_or(Severity::Unknown);
        CrimeStatus {
            kind: kind.to_string(),
            severity,
            expires_at,
        }
    }

    /// Новое нарушение (§10): blend-эскалация срока → статус + журнал в БД
    /// (D27) → `evt/crime/added` адресату. `None` — неизвестный kind.
    async fn apply_crime(&self, char_id: &str, kind: &str) -> Option<(String, String)> {
        let cfg = &self.state.config.gameplay.crime;
        // §5: ключи base — с суффиксом срока (`marauder_s`); kind — без.
        let base = cfg
            .base
            .get(kind)
            .or_else(|| cfg.base.get(&format!("{kind}_s")))
            .copied()? as i64;
        let now = crate::time::now_epoch();
        let t_cur = db::crime::total_term(&self.state.pool, char_id, now)
            .await
            .unwrap_or_else(|e| {
                warn!("crime (общий срок {char_id}): {e}");
                0
            });
        let (total, delta) =
            crate::systems::crime_blend(t_cur, base, cfg.t_max_s as i64, cfg.blend_k);
        if delta <= 0 {
            return None; // потолок §10 — вклада нет, статус не растёт
        }
        let (expires_at, _) =
            match db::crime::add_status(&self.state.pool, char_id, kind, now, delta, total).await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("crime (добавление {kind} {char_id}): {e}");
                    return None;
                }
            };
        let payload = CrimeAdded {
            status: self.crime_status(kind, expires_at),
            log_entry: CrimeLogEntry {
                kind: kind.to_string(),
                at: now,
                delta_s: delta,
                total_s: total,
            },
        };
        let text = crate::session::event_text(
            "evt/crime/added",
            None,
            serde_json::to_value(&payload).unwrap_or_default(),
        );
        if text.is_empty() {
            return None;
        }
        Some((char_id.to_string(), text))
    }

    // ── сделка (ФР-15, M12.7c) ──────────────────────────────────────────────

    /// Payload `evt/trade/state` для конкретного участника (зеркальная
    /// перспектива §8: `you` — своя сторона, `them` — чужая).
    async fn trade_state_payload(
        &self,
        char_id: &str,
        trade: &crate::social::Trade,
    ) -> serde_json::Value {
        let pool = &self.state.pool;
        let (mine, theirs) = if trade.a == char_id {
            (&trade.side_a, &trade.side_b)
        } else {
            (&trade.side_b, &trade.side_a)
        };
        let snapshots = |side: &crate::social::TradeSide| {
            let pool = pool.clone();
            let uids: Vec<String> = side.items.iter().map(|(u, _)| u.clone()).collect();
            async move {
                let mut out = Vec::new();
                for uid in uids {
                    if let Ok(Some(item)) = db::items::find_any(&pool, &uid).await {
                        if let Ok(Some(def)) = db::items::def(&pool, &item.def_id).await {
                            out.push(item_snapshot(&item, &def));
                        }
                    }
                }
                out
            }
        };
        let you = snapshots(mine).await;
        let them = snapshots(theirs).await;
        serde_json::json!({
            "offer_id": trade.id,
            "version": trade.version,
            "you": you,
            "them": them,
            "you_gold": mine.gold,
            "them_gold": theirs.gold,
            "you_locked": mine.locked,
            "them_locked": theirs.locked,
        })
    }

    /// Рассылка `evt/trade/state` обоим участникам (зеркала разные).
    async fn trade_broadcast_state(&self, trade: &crate::social::Trade) {
        let (a, b) = (trade.a.clone(), trade.b.clone());
        let payload_a = self.trade_state_payload(&a, trade).await;
        let payload_b = self.trade_state_payload(&b, trade).await;
        let text_a = event_payload_text("evt/trade/state", &payload_a);
        let text_b = event_payload_text("evt/trade/state", &payload_b);
        let world = self.state.world.lock().expect("мир");
        if let Some(t) = text_a {
            world.send_to_char(&a, t);
        }
        if let Some(t) = text_b {
            world.send_to_char(&b, t);
        }
    }

    /// Закрыть сделку событием `evt/trade/closed` (§8).
    async fn trade_close_event(
        &self,
        offer_id: &str,
        participants: &[String],
        reason: &str,
        result: Option<serde_json::Value>,
    ) {
        let mut payload = serde_json::json!({ "offer_id": offer_id, "reason": reason });
        if let Some(result) = result {
            payload["result"] = result;
        }
        let Some(text) = event_payload_text("evt/trade/closed", &payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in participants {
            world.send_to_char(addr, text.clone());
        }
    }

    /// `cmd/trade.open` (§7): открыть сделку — оба онлайн и в одной
    /// локации (ФР-15) → ack `{offer_id}` + `evt/trade/state` обоим.
    async fn trade_open(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeOpen>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "нельзя торговать с собой",
            )];
        }
        let location = self.current_location(&char_id).await;
        {
            let world = self.state.world.lock().expect("мир");
            let partner_loc = world.char_location(&cmd.player_id);
            if partner_loc.as_deref() != Some(location.as_str()) {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в вашей локации",
                )];
            }
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в сети",
                )];
            }
        }
        let opened = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .open_trade(&char_id, &cmd.player_id)
                .map(|id| social.trade(&id).cloned().expect("создана"))
        };
        match opened {
            Ok(trade) => {
                let offer_id = trade.id.clone();
                self.trade_broadcast_state(&trade).await;
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "offer_id": offer_id }),
                )]
            }
            Err(e) => vec![Self::err(seq, ErrorCode::TradeStale, e.message())],
        }
    }

    /// `cmd/trade.add_item` (§7): предмет в предложение (проверка
    /// владельца) → version+1, блокировки сняты → state обоим.
    async fn trade_add_item(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeAddItem>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        // Владелец и остаток стака — до изменения состава.
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("trade.add (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        // R8: надетый предмет в сделку нельзя (перенос сбрасывал бы
        // `equipped_slot` у получателя — бонусы без экипирования).
        if item.equipped_slot.is_some() {
            return vec![Self::err(
                seq,
                ErrorCode::Busy,
                "предмет надет — снимите его перед сделкой",
            )];
        }
        let qty = cmd
            .qty
            .unwrap_or(i64::from(item.qty))
            .clamp(1, i64::from(item.qty));

        let added = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_add_item(&char_id, &cmd.offer_id, &cmd.item_uid, qty)
                .cloned()
        };
        let trade = match added {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    crate::social::TradeError::Gone => ErrorCode::TradePartnerGone,
                    _ => ErrorCode::ItemGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.set_gold` (§7): золото стороны (баланс — из БД).
    async fn trade_set_gold(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeSetGold>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let balance =
            match db::items::count_by_type(&self.state.pool, &char_id, db::items::MONEY_TYPE).await
            {
                Ok(gold) => gold,
                Err(e) => {
                    warn!("trade.gold (баланс): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        if cmd.gold > balance {
            return vec![Self::err(
                seq,
                ErrorCode::TradeStale,
                format!("золота меньше: {balance}"),
            )];
        }
        let set = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_set_gold(&char_id, &cmd.offer_id, cmd.gold)
                .cloned()
        };
        let trade = match set {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    _ => ErrorCode::TradePartnerGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.propose` / `cmd/trade.accept` (§7): блокировка стороны
    /// со сверкой `version` (устаревшая → `E_TRADE_STALE`); обе стороны
    /// заблокированы → исполнение в одной транзакции (D27).
    async fn trade_lock_cmd(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradePropose>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // R9: лок и клон сделки — под одним локом мьютекса (между
        // двумя скоупами партнёр успевал изменить состав/версию).
        let (both_locked, trade) = {
            let mut social = self.state.social.lock().expect("соц");
            let both_locked = match social.trade_lock(&char_id, &cmd.offer_id, cmd.version) {
                Ok(locked) => locked,
                Err(e) => {
                    drop(social);
                    let code = match e {
                        crate::social::TradeError::Stale => ErrorCode::TradeStale,
                        _ => ErrorCode::TradePartnerGone,
                    };
                    return vec![Self::err(seq, code, e.message())];
                }
            };
            (both_locked, social.trade(&cmd.offer_id).cloned())
        };
        let Some(trade) = trade else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        if !both_locked {
            self.trade_broadcast_state(&trade).await;
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }
        // R9: исполнение — только если обе стороны всё ещё заблокированы
        // и версия не изменилась (изменение состава бампает version и
        // снимает блокировки; исполнение неподтверждённого клона =
        // обход партнёра).
        let confirmed = {
            let social = self.state.social.lock().expect("соц");
            social
                .trade(&trade.id)
                .filter(|t| t.version == trade.version && t.side_a.locked && t.side_b.locked)
                .cloned()
        };
        let Some(trade) = confirmed else {
            self.trade_broadcast_state(&trade).await;
            return vec![Self::err(
                seq,
                ErrorCode::TradeStale,
                "сделка изменилась — подтвердите заново",
            )];
        };
        // Оба заблокированы → исполнение одной транзакцией (предметы+золото).
        match self.trade_execute(&trade).await {
            Ok(result) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "done",
                    Some(result),
                )
                .await;
                vec![Self::ack_payload(seq, &serde_json::json!({}))]
            }
            Err(e) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "failed",
                    Some(serde_json::json!({ "error": e })),
                )
                .await;
                vec![Self::err(seq, ErrorCode::TradeStale, e)]
            }
        }
    }

    /// Исполнение сделки: перемещение предметов и золота сторон **одной**
    /// транзакцией (D27); при любом конфликте — откат и `failed`.
    async fn trade_execute(
        &self,
        trade: &crate::social::Trade,
    ) -> Result<serde_json::Value, String> {
        let pool = &self.state.pool;
        let mut tx = pool.begin().await.map_err(|e| format!("tx: {e}"))?;
        let sides = [
            (&trade.a, &trade.b, &trade.side_a),
            (&trade.b, &trade.a, &trade.side_b),
        ];
        let mut moved = Vec::new();
        for (from, to, side) in sides {
            for (uid, qty) in &side.items {
                // Предмет всё ещё у стороны? Частичный перенос стака.
                let cur: Option<(i32, String)> =
                    sqlx::query_as("SELECT qty, character_id FROM items WHERE uid = $1 FOR UPDATE")
                        .bind(uid)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(|e| format!("читаю {uid}: {e}"))?;
                let Some((cur_qty, owner)) = cur else {
                    return Err(format!("предмет {uid} исчез"));
                };
                if owner != *from {
                    return Err(format!("предмет {uid} не у владельца"));
                }
                if i64::from(cur_qty) < *qty {
                    return Err(format!("предмет {uid}: не хватает стака"));
                }
                if i64::from(cur_qty) == *qty {
                    // R8: перенос сбрасывает «надетость» — получатель
                    // применяет бонусы только после экипирования.
                    sqlx::query(
                        "UPDATE items SET character_id = $2, equipped_slot = NULL WHERE uid = $1",
                    )
                    .bind(uid)
                    .bind(to)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("перенос {uid}: {e}"))?;
                } else {
                    sqlx::query("UPDATE items SET qty = qty - $2 WHERE uid = $1")
                        .bind(uid)
                        .bind(qty)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("уменьшение {uid}: {e}"))?;
                    let def_id: String =
                        sqlx::query_scalar("SELECT def_id FROM items WHERE uid = $1")
                            .bind(uid)
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(|e| format!("деф {uid}: {e}"))?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(qty)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("вставка {uid}: {e}"))?;
                }
                moved.push(serde_json::json!({ "uid": uid, "qty": qty }));
            }
            // F29a: деньги — стаки монет: списание у `from` (стаки
            // убывают, пустые удаляются) и начисление `to` (в старый
            // стак либо новым) — той же транзакцией, что и предметы.
            if side.gold > 0 {
                let rows: Vec<(String, i32)> = sqlx::query_as(
                    "SELECT i.uid, i.qty FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid FOR UPDATE",
                )
                .bind(from)
                .bind(db::items::MONEY_TYPE)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| format!("монеты {from}: {e}"))?;
                let mut left = side.gold;
                for (uid, qty) in rows {
                    if left <= 0 {
                        break;
                    }
                    let take = i64::from(qty).min(left);
                    left -= take;
                    let rest = qty - take as i32;
                    if rest <= 0 {
                        sqlx::query("DELETE FROM items WHERE uid = $1")
                            .bind(&uid)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("списание {uid}: {e}"))?;
                    } else {
                        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
                            .bind(&uid)
                            .bind(rest)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("стак {uid}: {e}"))?;
                    }
                }
                if left > 0 {
                    return Err(format!("недостаточно монет у {from}"));
                }
                let target: Option<String> = sqlx::query_scalar(
                    "SELECT i.uid FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid LIMIT 1 FOR UPDATE",
                )
                .bind(to)
                .bind(db::items::MONEY_TYPE)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("стак {to}: {e}"))?;
                if let Some(uid) = target {
                    sqlx::query("UPDATE items SET qty = qty + $2 WHERE uid = $1")
                        .bind(&uid)
                        .bind(side.gold)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("зачисление {uid}: {e}"))?;
                } else {
                    let def_id: String = sqlx::query_scalar(
                        "SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1",
                    )
                    .bind(db::items::MONEY_TYPE)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| format!("определение валюты: {e}"))?
                    .ok_or_else(|| "нет определения валюты в контенте".to_string())?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(side.gold)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("зачисление {to}: {e}"))?;
                }
            }
        }
        tx.commit().await.map_err(|e| format!("commit: {e}"))?;
        Ok(serde_json::json!({ "moved": moved }))
    }

    /// `cmd/trade.cancel` (§7): отмена любой стороной.
    async fn trade_cancel(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeCancel>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let closed = {
            let mut social = self.state.social.lock().expect("соц");
            let is_member = social
                .trade(&cmd.offer_id)
                .is_some_and(|t| t.a == char_id || t.b == char_id);
            if is_member {
                social.close_trade(&cmd.offer_id)
            } else {
                None
            }
        };
        let Some((participants, trade)) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        self.trade_close_event(&trade.id, &participants, "cancelled", None)
            .await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    // ── гильдия (ФР-16, M12.7d) ─────────────────────────────────────────────

    /// `GuildState` из БД: участники + online по реестру мира.
    async fn guild_state(&self, guild_id: &str) -> Option<ld2_protocol::types::GuildState> {
        let row = db::guilds::find(&self.state.pool, guild_id).await.ok()??;
        let members = db::guilds::members(&self.state.pool, guild_id)
            .await
            .unwrap_or_default();
        let world = self.state.world.lock().expect("мир");
        let members = members
            .into_iter()
            .map(|(id, name, rank)| ld2_protocol::types::GuildMember {
                online: world.char_name(&id).is_some(),
                id,
                name,
                rank,
                extra: Default::default(),
            })
            .collect();
        Some(ld2_protocol::types::GuildState {
            id: row.0,
            name: row.1,
            members,
            extra: Default::default(),
        })
    }

    /// Рассылка `evt/guild/updated` адресатам (свой — в outs после ack).
    fn guild_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let Some(text) = event_payload_text("evt/guild/updated", payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Гильдия персонажа для снапшота (§3 `guild: null` — не состоит).
    async fn guild_state_of(&self, char_id: &str) -> Option<ld2_protocol::types::GuildState> {
        let guild = db::guilds::guild_of(&self.state.pool, char_id)
            .await
            .ok()
            .flatten()?;
        self.guild_state(&guild.0).await
    }

    /// `cmd/guild.browse` (§7): поиск гильдий → `{guilds[]}`.
    async fn guild_browse(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildBrowse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let guilds = match db::guilds::browse(&self.state.pool, cmd.query.as_deref()).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("guild.browse: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guilds": guilds }),
        )]
    }

    /// `cmd/guild.create` (§7): создание (правило сервера: имя уникально,
    /// автор — лидер) → ack `{guild}` + журнал.
    async fn guild_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildCreate>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let name = cmd.name.trim();
        if name.is_empty() || name.chars().count() > 50 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "имя гильдии: 1..=50 символов",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        let guild_id = match db::guilds::create(&self.state.pool, name, &char_id).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "имя занято")];
            }
            Err(e) => {
                warn!("guild.create: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.join` (§7): вступление (открытое или по приглашению —
    /// `guild_id` из инвайта) → ack `{guild}`.
    async fn guild_join(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildJoin>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        // Приглашение (если есть) подтверждает вступление и закрывается.
        let invited = {
            let mut social = self.state.social.lock().expect("соц");
            let mut found = None;
            if let Some((invite_id, _)) = social
                .guild_invites_of(&char_id)
                .into_iter()
                .find(|(_, gid)| cmd.guild_id.as_deref().is_none_or(|g| g == gid))
            {
                if let Some(invite) = social.take_guild_invite(&char_id, &invite_id) {
                    found = Some(invite.guild_id);
                }
            }
            found
        };
        let guild_id = match cmd.guild_id.clone().or(invited) {
            Some(id) => id,
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нет приглашения в гильдию",
                )];
            }
        };
        let Some(guild) = (match db::guilds::find(&self.state.pool, &guild_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.join (гильдия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::GuildPermission, "гильдии нет")];
        };
        match db::guilds::join(&self.state.pool, &guild_id, &char_id, "member").await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "уже состоите")];
            }
            Err(e) => {
                warn!("guild.join: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        // Событие — всем участникам (включая нового).
        if let Some(state) = &state {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let _ = guild;
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.leave` (§7): выход (лидер передаёт ранг первому).
    async fn guild_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Перед выходом лидера — ранг первому оставшемуся (атомарно в БД).
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        if my_rank.as_deref() == Some("leader") {
            let _ = self.promote_first_member(&char_id).await;
        }
        let closed = match db::guilds::leave(&self.state.pool, &char_id).await {
            Ok(id) => id,
            Err(e) => {
                warn!("guild.leave: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(guild_id) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        // Оставшимся — обновлённый состав; ушедшему — guild: null.
        if let Some(state) = self.guild_state(&guild_id).await {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let null = serde_json::json!({ "guild": serde_json::Value::Null });
        self.guild_broadcast(std::slice::from_ref(&char_id), &null, &mut outs, "");
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

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

    /// `cmd/guild.invite` (§7): приглашение → `evt/guild/updated` адресату.
    async fn guild_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        let Some(my_rank) = my_rank else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав приглашать",
            )];
        }
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.invite: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        {
            let world = self.state.world.lock().expect("мир");
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
            }
        }
        if db::guilds::rank_of(&self.state.pool, &cmd.player_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "игрок уже в гильдии")];
        }
        let from_name = self.current_name();
        let invite_id = {
            let mut social = self.state.social.lock().expect("соц");
            social.guild_invite(&cmd.player_id, &char_id, &from_name, &guild.0, &guild.1)
        };
        let payload = {
            let social = self.state.social.lock().expect("соц");
            let invite = social.guild_invite_payload(&invite_id);
            let mut payload = serde_json::json!({ "guild": serde_json::Value::Null });
            if let Some(invite) = invite {
                payload["invite"] = invite;
            }
            payload
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        self.guild_broadcast(
            std::slice::from_ref(&cmd.player_id),
            &payload,
            &mut outs,
            "",
        );
        outs
    }

    /// `cmd/guild.kick` (§7): исключение (leader/officer).
    async fn guild_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // R1: гильдия и ранг кикера одной выборкой — кик только в своей.
        let Some((my_guild_id, my_rank)) = db::guilds::membership(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
        else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав исключать",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя исключить себя (выйдите через guild.leave)",
            )];
        }
        // Нельзя исключить лидера; офицер не исключает офицера.
        // R1: цель — только участник той же гильдии (раньше офицер A
        // выкидывал участника B: kick не фильтровал по guild_id).
        let Some((target_guild_id, target_rank)) =
            db::guilds::membership(&self.state.pool, &cmd.player_id)
                .await
                .ok()
                .flatten()
        else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "игрок не в гильдии",
            )];
        };
        if target_guild_id != my_guild_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "игрок не в вашей гильдии",
            )];
        }
        match target_rank.as_str() {
            "leader" => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нельзя исключить лидера",
                )];
            }
            "officer" if my_rank == "officer" => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "офицер не может исключить офицера",
                )];
            }
            _ => {}
        }
        match db::guilds::kick(&self.state.pool, &my_guild_id, &cmd.player_id).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.kick: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let guild_id = db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .map(|g| g.0);
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(guild_id) = &guild_id {
            if let Some(state) = self.guild_state(guild_id).await {
                let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
                let payload = serde_json::json!({ "guild": state });
                self.guild_broadcast(&members, &payload, &mut outs, &char_id);
            }
            let null = serde_json::json!({ "guild": serde_json::Value::Null });
            self.guild_broadcast(std::slice::from_ref(&cmd.player_id), &null, &mut outs, "");
            self.guild_log_event(guild_id, &mut outs).await;
        }
        outs
    }

    /// `cmd/guild.rank_set` (§7): смена ранга (атомарно; только leader).
    async fn guild_rank_set(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildRankSet>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !matches!(cmd.rank.as_str(), "leader" | "officer" | "member") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "неизвестный ранг",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some("leader")
        {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "только лидер меняет ранги",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя менять ранг себе",
            )];
        }
        match db::guilds::set_rank(&self.state.pool, &guild.0, &cmd.player_id, &cmd.rank).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(state) = self.guild_state(&guild.0).await {
            let mut members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            if !members.contains(&cmd.player_id) {
                members.push(cmd.player_id.clone());
            }
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        self.guild_log_event(&guild.0, &mut outs).await;
        outs
    }

    /// Реплей журнала гильдии → события `evt/guild/log` (ФР-16: системный
    /// лог при входе; кольцо — последние 50).
    async fn guild_log_event(&self, guild_id: &str, outs: &mut Vec<Out>) {
        let entries = match db::guilds::log_entries(&self.state.pool, guild_id).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("guild (журнал {guild_id}): {e}");
                return;
            }
        };
        for (kind, text, at) in entries {
            let payload = serde_json::json!({
                "entry": { "kind": kind, "text": text, "at": at }
            });
            if let Some(t) = event_payload_text("evt/guild/log", &payload) {
                outs.push(Out::Text(t));
            }
        }
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

    /// Рассылка `evt/party/updated` адресатам (свой конверт — в ack-ветке
    /// нельзя: события строго после своего ack, §2.2 — поэтому событие
    /// автору идёт через `outs`, остальным — в каналы соединений).
    fn party_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let text = event_payload_text("evt/party/updated", payload);
        let Some(text) = text else { return };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Payload `evt/party/updated` `{party}` (+ опц. `invite`, §11).
    fn party_payload(
        party: Option<ld2_protocol::types::PartyState>,
        invite: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({ "party": party });
        if let Some(invite) = invite {
            payload["invite"] = invite;
        }
        payload
    }

    /// `cmd/party.create` (§7): создать группу (лидер — автор) → `{party}`.
    async fn party_create(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.create_party(&char_id, &name) {
            Some(party) => {
                drop(social);
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )]
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::Internal, "вы уже в группе")]
            }
        }
    }

    /// `cmd/party.invite` (§7): приглашение → `evt/party/updated` c
    /// `invite` адресату (§11).
    async fn party_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Адресат по id: в мире (онлайн) — иначе отказ.
        let target_name = {
            let world = self.state.world.lock().expect("мир");
            match world.char_name(&cmd.player_id) {
                Some(name) => name,
                None => {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                }
            }
        };
        let _ = target_name;
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.invite(&char_id, &name, &cmd.player_id) {
            Ok(invite_id) => {
                let invite = social.invite_payload(&invite_id);
                let payload = Self::party_payload(None, invite);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &payload,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::PartyFull, e.message())]
            }
        }
    }

    /// `cmd/party.accept` (§7): принять приглашение → `{party}` +
    /// `evt/party/updated` всем участникам.
    async fn party_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, true).await
    }

    /// `cmd/party.decline` (§7): отклонить → `{}`.
    async fn party_decline(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, false).await
    }

    /// Общий обработчик accept/decline.
    async fn party_answer(&mut self, env: &Envelope, seq: u32, accept: bool) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyAccept>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let max = self.state.config.gameplay.party_max_members;
        let mut social = self.state.social.lock().expect("соц");
        if !accept {
            if social.decline_invite(&char_id, &cmd.invite_id) {
                return vec![Self::ack_payload(seq, &serde_json::json!({}))];
            }
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "приглашение не найдено",
            )];
        }
        match social.accept_invite(&char_id, &name, &cmd.invite_id, max) {
            Ok((party, members)) => {
                let payload = Self::party_payload(Some(party.clone()), None);
                drop(social);
                let mut outs = vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )];
                self.party_broadcast(&members, &payload, &mut outs, &char_id);
                outs
            }
            Err(e) => {
                drop(social);
                let code = match e {
                    crate::social::InviteError::Full => ErrorCode::PartyFull,
                    _ => ErrorCode::TargetGone,
                };
                vec![Self::err(seq, code, e.message())]
            }
        }
    }

    /// `cmd/party.leave` (§7): выйти → `{}` + `evt/party/updated` бывшим
    /// участникам (`party: null` — если группа распалась).
    async fn party_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.leave_party(&char_id) {
            Some((party, members)) => {
                // Оставшимся — обновлённый состав; ушедшему — party: null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &char_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                let left = Self::party_payload(None, None);
                self.party_broadcast(&remaining, &payload, &mut outs, "");
                self.party_broadcast(std::slice::from_ref(&char_id), &left, &mut outs, "");
                outs
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")]
            }
        }
    }

    /// `cmd/party.kick` (§7): исключить участника (только лидер).
    async fn party_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.kick_member(&char_id, &cmd.player_id) {
            Ok((party, members)) => {
                // Оставшимся — состав без исключённого; исключённому — null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &cmd.player_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(&remaining, &payload, &mut outs, &char_id);
                let removed = Self::party_payload(None, None);
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &removed,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::GuildPermission, e.message())]
            }
        }
    }

    // ── профессии и крафт (ФР-08, M12.6) ───────────────────────────────────

    /// Профессия персонажа → снапшот §6 (`xp_to_next` из конфига, D25).
    fn profession_snapshot(&self, id: &str, xp: i64, rank: i32, level: u32) -> ProfessionSnapshot {
        // M21: общая сборка (уровень/ранг/extra) — в systems.
        crate::systems::profession_snapshot(
            &self.state.config.gameplay.professions,
            id,
            xp,
            rank,
            level,
        )
    }

    /// Окно профессий: сброс изученной профессии — прогресс теряется,
    /// слот основной освобождается; ack несёт `profession_id`
    /// (`E_PROFESSION` — не изучена).
    async fn craft_reset(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftReset>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        match db::professions::forget(&self.state.pool, &char_id, &cmd.profession_id).await {
            Ok(true) => vec![Self::ack_payload(
                seq,
                &serde_json::json!({ "profession_id": cmd.profession_id }),
            )],
            Ok(false) => {
                vec![Self::err(
                    seq,
                    ErrorCode::Profession,
                    "профессия не изучена",
                )]
            }
            Err(e) => {
                warn!("craft.reset ({}): {e}", cmd.profession_id);
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// M21: повышение ранга **у тренера** (`cmd/craft.rank_up`) —
    /// уровень набран до границы (`E_LEVEL_REQUIRED`), высший ранг →
    /// `E_RANK_CAP`, золото по прогрессии `200 × 1.5^ранг`
    /// (`E_INGREDIENTS`); ack несёт `{profession}` + событие.
    async fn craft_rank_up(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftRankUp>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;
        // Тренер по этой профессии в локации (E_NO_TRAINER).
        let trainer = match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Profession,
            &cmd.profession_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("rank_up (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if trainer.is_none() {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этой профессии",
            )];
        }
        // Изучена → (xp, rank, level).
        let learned = match db::professions::learned(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("rank_up (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some((_, xp, rank, level)) = learned
            .into_iter()
            .find(|(id, _, _, _)| id == &cmd.profession_id)
        else {
            return vec![Self::err(
                seq,
                ErrorCode::Profession,
                "профессия не изучена",
            )];
        };
        let cfg = &self.state.config.gameplay.professions;
        let rank_u32 = rank.max(1) as u32;
        if rank_u32 >= crate::systems::profession_rank_max(cfg) {
            return vec![Self::err(seq, ErrorCode::RankCap, "достигнут высший ранг")];
        }
        // Граница следующего ранга: level ≥ rank × span.
        let need_level = rank_u32.saturating_mul(cfg.rank_span);
        if (level.max(0) as u32) < need_level {
            return vec![Self::err(
                seq,
                ErrorCode::LevelRequired,
                format!("нужен уровень {need_level} для ранга {}", rank_u32 + 1),
            )];
        }
        // Золото по прогрессии (200 × 1.5^ранг) — та же касса, что и
        // обучение (монеты-стаки, F29a).
        let cost = crate::systems::profession_rank_up_cost(cfg, rank_u32);
        let price = trainers::TrainerPrice {
            gold_base: cost,
            gold_per_rank: 0,
            items: Vec::new(),
        };
        let money_events = match self.pay_trainer(seq, &char_id, &price, cost).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };
        let new_rank = rank + 1;
        if let Err(e) =
            db::professions::grant_progress(pool, &char_id, &cmd.profession_id, xp, new_rank, level)
                .await
        {
            warn!("rank_up ({}): {e}", cmd.profession_id);
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        let snapshot =
            self.profession_snapshot(&cmd.profession_id, xp, new_rank, level.max(0) as u32);
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "profession": snapshot }),
        )];
        outs.push(out_event(
            "evt/craft/profession",
            None,
            serde_json::to_value(CraftProfession {
                profession: snapshot,
                name: None,
                gain: None,
            })
            .unwrap_or_default(),
        ));
        outs.extend(money_events);
        outs
    }

    /// `cmd/craft.list` (§7): мои профессии `{professions[]}`.
    async fn craft_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let rows = match db::professions::learned(&self.state.pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let professions: Vec<ProfessionSnapshot> = rows
            .iter()
            .map(|(id, xp, rank, level)| self.profession_snapshot(id, *xp, *rank, *level as u32))
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "professions": professions }),
        )]
    }

    /// `cmd/craft.recipes` (§7): список рецептов `{recipes[]}` (payload
    /// skills.md §5.4; фильтр по профессии опционален).
    async fn craft_recipes(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftRecipes>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let rows = match db::craft::recipes(&self.state.pool, cmd.profession.as_deref()).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.recipes: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let recipes: Vec<serde_json::Value> = rows.iter().map(recipe_payload).collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "recipes": recipes }),
        )]
    }

    /// `cmd/craft.learn` (§7): изучение профессии; лимит первичных —
    /// `profession_limit` (D25) → `E_PROFESSION_LIMIT`.
    async fn craft_learn(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftLearn>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Локация до заимствования pool (current_location требует &mut self).
        let location = self.current_location(&char_id).await;
        let pool = &self.state.pool;

        let prof_type = match sqlx::query_scalar::<_, String>(
            "SELECT prof_type FROM professions WHERE id = $1",
        )
        .bind(&cmd.profession_id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(t)) => t,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::Profession, "нет такой профессии")];
            }
            Err(e) => {
                warn!("craft.learn (профессия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Уже изучена → идемпотентный ack без повторной оплаты (§7, M14).
        // Профессии живут в character_professions, а не в character_skills.
        match db::professions::learned(pool, &char_id).await {
            Ok(rows) => {
                if let Some((_, xp, rank, level)) = rows
                    .into_iter()
                    .find(|(id, _, _, _)| id == &cmd.profession_id)
                {
                    let snapshot =
                        self.profession_snapshot(&cmd.profession_id, xp, rank, level as u32);
                    return vec![Self::ack_payload(
                        seq,
                        &serde_json::json!({ "profession": snapshot }),
                    )];
                }
            }
            Err(e) => {
                warn!("craft.learn (изученные): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // M14: обучение только у тренера в локации (E_NO_TRAINER).
        let Some(price) = (match trainers::trainer_price(
            pool,
            &location,
            trainers::TrainerKind::Profession,
            &cmd.profession_id,
        )
        .await
        {
            Ok(price) => price,
            Err(e) => {
                warn!("craft.learn (тренер): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::NoTrainer,
                "здесь нет тренера по этой профессии",
            )];
        };

        if prof_type == "primary" {
            let limit = i64::from(self.state.config.gameplay.profession_limit);
            match db::professions::primary_count(pool, &char_id).await {
                Ok(count) if count >= limit => {
                    return vec![Self::err(
                        seq,
                        ErrorCode::ProfessionLimit,
                        format!("лимит первичных профессий: {limit}"),
                    )];
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("craft.learn (счётчик): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // M14: оплата тренеру (новая профессия — ранг 0 → базовая цена).
        let gold = trainers::gold_price(&price, 0);
        // F29a: оплата монетами — события списания в конце ответа.
        let money_events = match self.pay_trainer(seq, &char_id, &price, gold).await {
            Ok(events) => events,
            Err(e) => return vec![e],
        };

        match db::professions::learn(pool, &char_id, &cmd.profession_id).await {
            Ok(_) => {}
            Err(e) => {
                warn!("craft.learn: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let snapshot = self.profession_snapshot(&cmd.profession_id, 0, 1, 0);
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "profession": snapshot }),
        )];
        outs.push(out_event(
            "evt/craft/profession",
            None,
            serde_json::to_value(CraftProfession {
                profession: snapshot,
                name: None,
                gain: None,
            })
            .unwrap_or_default(),
        ));
        outs.extend(money_events);
        outs
    }

    /// `cmd/craft.start` (§7): проверки (ранг/станок/инструмент/ингредиенты)
    /// → списание ингредиентов + джоб в БД (D27); итог — `evt/craft/result`
    /// по готовности (тик-луп). Ошибки: `E_RANK_CAP`, `E_STATION_REQUIRED`,
    /// `E_TOOL_MISSING`, `E_INGREDIENTS`.
    async fn craft_start(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CraftStart>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if cmd.qty < 1 || cmd.qty > 100 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "qty должно быть 1..=100",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = self.state.pool.clone();

        let Some(recipe) = (match db::craft::recipe(&pool, &cmd.recipe_id).await {
            Ok(r) => r,
            Err(e) => {
                warn!("craft.start (рецепт): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "рецепта нет")];
        };

        // Ранг профессии.
        let learned = match db::professions::learned(&pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("craft.start (профессии): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let prof_rank = learned
            .iter()
            .find(|(id, _, _, _)| *id == recipe.profession)
            .map(|(_, _, rank, _)| *rank)
            .unwrap_or(0);
        if prof_rank < recipe.rank {
            return vec![Self::err(
                seq,
                ErrorCode::RankCap,
                format!("нужен ранг {} профессии {}", recipe.rank, recipe.profession),
            )];
        }

        // Станок: сущность с `extra.station` в текущей локации.
        if let Some(station) = recipe.station.as_deref() {
            let location = self.current_location(&char_id).await;
            let has_station = match db::locations::get(&pool, &location).await {
                Ok(Some(row)) => {
                    let entities: Vec<Entity> =
                        serde_json::from_value(row.entities.clone()).unwrap_or_default();
                    entities
                        .iter()
                        .any(|e| e.extra.get("station").and_then(|s| s.as_str()) == Some(station))
                }
                _ => false,
            };
            if !has_station {
                return vec![Self::err(
                    seq,
                    ErrorCode::StationRequired,
                    format!("нужен станок: {station}"),
                )];
            }
        }

        // Инструмент: предмет с `item_type` в инвентаре.
        if let Some(tool) = recipe.tool_type.as_deref() {
            let has_tool = match db::items::inventory_has_type(&pool, &char_id, tool).await {
                Ok(has) => has,
                Err(e) => {
                    warn!("craft.start (инструмент): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if !has_tool {
                return vec![Self::err(
                    seq,
                    ErrorCode::ToolMissing,
                    format!("нет инструмента: {tool}"),
                )];
            }
        }

        // Ингредиенты: qty × inputs.
        let inputs: Vec<ContentInput> =
            serde_json::from_value(recipe.inputs.clone()).unwrap_or_default();
        for input in &inputs {
            let need = input.qty * cmd.qty;
            let have = match db::items::count_by_type(&pool, &char_id, &input.item_type).await {
                Ok(have) => have,
                Err(e) => {
                    warn!("craft.start (ингредиенты): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
            if have < need {
                return vec![Self::err(
                    seq,
                    ErrorCode::Ingredients,
                    format!("не хватает {}: {have}/{need}", input.item_type),
                )];
            }
        }
        for input in &inputs {
            let need = input.qty * cmd.qty;
            if let Err(e) =
                db::items::consume_by_type(&pool, &char_id, &input.item_type, need).await
            {
                warn!("craft.start (списание): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }

        // Джоб: материалы уже списаны, рестарт не теряет их (D27).
        let finishes_at =
            crate::time::now_epoch() as f64 + f64::from(recipe.time_s) * cmd.qty as f64;
        if let Err(e) = db::craft::enqueue(&pool, &char_id, &recipe, cmd.qty, finishes_at).await {
            warn!("craft.start (джоб): {e}");
            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
        }
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    // ── диалоги NPC (M18/F35; ветки/реплики F37) ──────────────────────────

    /// NPC из сида текущей локации (паритет с `entity.info`);
    /// не-NPC/несущность → `E_TARGET_GONE`.
    async fn require_npc(&self, seq: u32, entity_id: &str) -> Result<Entity, Out> {
        let pool = &self.state.pool;
        let current = self.location_id.clone().unwrap_or_default();
        let entities = match db::locations::get(pool, &current).await {
            Ok(Some(row)) => {
                serde_json::from_value::<Vec<Entity>>(row.entities.clone()).unwrap_or_default()
            }
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!("npc (чтение {current}): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let Some(entity) = entities.into_iter().find(|e| e.id == entity_id) else {
            return Err(Self::err(seq, ErrorCode::TargetGone, "сущность не найдена"));
        };
        if entity.kind != ld2_protocol::types::EntityKind::Npc {
            return Err(Self::err(seq, ErrorCode::TargetGone, "это не NPC"));
        }
        Ok(entity)
    }

    /// Контекст персонажа для условий диалога (имя/пол/уровень/attrs/
    /// навыки/профессии/рюкзак — M18/F35).
    async fn dialog_context(
        &self,
        char_id: &str,
        seq: u32,
    ) -> Result<crate::dialogs::DialogContext, Out> {
        let pool = &self.state.pool;
        let row = match db::characters::get(pool, char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return Err(Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")),
            Err(e) => {
                warn!("npc (лист): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let (level, _, _) =
            crate::systems::level_for_total(row.total_xp, &self.state.config.gameplay.level);
        let attrs: std::collections::BTreeMap<String, i64> =
            serde_json::from_value(row.attrs.clone()).unwrap_or_default();
        let mut skills = std::collections::HashMap::new();
        let mut professions = std::collections::HashMap::new();
        let mut items = std::collections::HashMap::new();
        match db::skills::learned(pool, char_id).await {
            Ok(rows) => {
                for s in rows {
                    skills.insert(s.skill_id, i64::from(s.rank));
                }
            }
            Err(e) => {
                warn!("npc (навыки): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        match db::professions::learned(pool, char_id).await {
            Ok(rows) => {
                for p in rows {
                    professions.insert(p.0, i64::from(p.2));
                }
            }
            Err(e) => {
                warn!("npc (профессии): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        match db::items::inventory(pool, char_id).await {
            Ok(rows) => {
                for it in rows {
                    *items.entry(it.def_id).or_insert(0) += i64::from(it.qty);
                }
            }
            Err(e) => {
                warn!("npc (рюкзак): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        }
        Ok(crate::dialogs::DialogContext {
            name: row.name,
            gender: row.gender,
            level: i64::from(level),
            attrs,
            skills,
            professions,
            items,
        })
    }

    /// Доступные квесты NPC (giver == id, фаза available): id-ы для
    /// гейта строк + wire-список для ack + флаг «NPC вообще даёт
    /// квесты» (кнопка статус-бара, M18/F38).
    async fn npc_quests(
        &self,
        char_id: &str,
        npc_id: &str,
        seq: u32,
    ) -> Result<
        (
            std::collections::HashSet<String>,
            Vec<serde_json::Value>,
            bool,
        ),
        Out,
    > {
        let pool = &self.state.pool;
        let (defs, progress) = match (
            db::quests::defs(pool).await,
            db::quests::progress(pool, char_id).await,
        ) {
            (Ok(d), Ok(p)) => (d, p),
            (Err(e), _) | (_, Err(e)) => {
                warn!("npc (квесты): {e}");
                return Err(Self::err(seq, ErrorCode::Internal, "внутренняя ошибка"));
            }
        };
        let mut quests = Vec::new();
        let mut available = std::collections::HashSet::new();
        let mut gives_quests = false;
        for def in &defs {
            if def.2.as_deref() != Some(npc_id) {
                continue;
            }
            gives_quests = true;
            let progress_row = progress.iter().find(|(id, _, _)| id == &def.0);
            let state = db::quests::build_state(def, progress_row);
            if state.state == ld2_protocol::types::QuestPhase::Available {
                available.insert(state.id.clone());
                quests.push(serde_json::to_value(&state).unwrap_or_default());
            }
        }
        Ok((available, quests, gives_quests))
    }

    /// `cmd/npc.talk` (M18/F35/F37): NPC текущей локации → **корень
    /// ветки** (узел с видимыми ответами) и доступные квесты.
    /// ack `{dialog: {id, name}, node: {...}|null, quests[], actions[]}`;
    /// `node: null` — записи диалога нет (квесты отдаются). Поле
    /// `greeting` убрано (M18/F42, ADR-008).
    async fn npc_talk(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<NpcTalk>(env, seq) else {
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
        let context = match self.dialog_context(&char_id, seq).await {
            Ok(context) => context,
            Err(out) => return vec![out],
        };
        let (available, quests, gives_quests) =
            match self.npc_quests(&char_id, &entity.id, seq).await {
                Ok(triple) => triple,
                Err(out) => return vec![out],
            };
        // M18/F38: контекстные кнопки статус-бара — только реально
        // существующие системы: «Квесты» (NPC — выдающий) и «Обучение»
        // (trainer в extra сущности); торговля/склад — когда появятся
        // их системы.
        let mut actions: Vec<&str> = Vec::new();
        if gives_quests {
            actions.push("quests");
        }
        if entity.extra.contains_key("trainer") {
            actions.push("train");
        }
        if self.state.content().shops.contains_key(&entity.id) {
            actions.push("shop");
        }
        let pool = &self.state.pool;
        let shown = match db::dialogs::shown_lines(pool, &char_id, &entity.id).await {
            Ok(map) => map,
            Err(e) => {
                warn!("npc.talk (кулдауны строк): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let now = crate::time::now_epoch();
        let ctx = context.line_ctx(&available);
        let content = self.state.content();
        let node = match content.dialogs.get(&entity.id) {
            Some(def) => {
                let root = def
                    .root
                    .clone()
                    .or_else(|| def.lines.first().map(|l| l.id.clone()))
                    .unwrap_or_default();
                // M18/F42: приветствие убрано (ADR-008) — окно открывается
                // сразу с корневой репликой.
                crate::dialogs::enter_node(def, &root, &ctx, now, &shown)
            }
            None => None,
        };
        if let Some((node, mark)) = &node {
            if *mark {
                if let Err(e) =
                    db::dialogs::mark_shown(pool, &char_id, &entity.id, &node.id, now).await
                {
                    // Показ уже уходит клиенту — кулдаун догонит в следующий раз.
                    warn!("npc.talk (отметка показа {}): {e}", node.id);
                }
            }
        }
        let node =
            serde_json::to_value(node.map(|(node, _)| node)).unwrap_or(serde_json::Value::Null);
        // M18/F44: обучение NPC из контента диалога (ADR-009) — список
        // навыков с ценой; у NPC без training — пустой массив.
        let training = self
            .state
            .content()
            .dialogs
            .get(&entity.id)
            .map(|def| {
                serde_json::to_value(&def.training).unwrap_or_else(|_| serde_json::json!([]))
            })
            .unwrap_or_else(|| serde_json::json!([]));
        // M21: профессии, которым обучает этот NPC (маркер trainer
        // сущности), с ценой `price_profession` — панель «Обучение»
        // учит и профессиям (изучение — cmd/craft.learn).
        let professions = {
            let marker = entity.extra.get("trainer");
            let ids: Vec<&str> = marker
                .and_then(|m| m.get("professions"))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let price = marker
                .and_then(|m| m.get("price_profession"))
                .or_else(|| marker.and_then(|m| m.get("price")));
            let gold = price
                .and_then(|p| p.get("gold_base"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            ids.into_iter()
                .map(|id| serde_json::json!({ "id": id, "gold": gold }))
                .collect::<Vec<_>>()
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "dialog": {
                    "id": entity.id,
                    "name": entity.name,
                },
                "node": node,
                "quests": quests,
                "actions": actions,
                "training": training,
                "professions": professions,
            }),
        )]
    }

    /// `cmd/npc.reply` (M18/F37): ответ персонажа — переход по связи
    /// `node_id` → `reply_id`; у ответа без `next` диалог заканчивается
    /// (`{node: null}`). ack `{node: {...}|null, quests[]}` — список
    /// квестов освежается (приняли/сдали между открытиями).
    async fn npc_reply(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<NpcReply>(env, seq) else {
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
        let content = self.state.content();
        let Some(def) = content.dialogs.get(&entity.id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "диалога нет")];
        };
        let context = match self.dialog_context(&char_id, seq).await {
            Ok(context) => context,
            Err(out) => return vec![out],
        };
        let (available, quests, _) = match self.npc_quests(&char_id, &entity.id, seq).await {
            Ok(triple) => triple,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        let shown = match db::dialogs::shown_lines(pool, &char_id, &entity.id).await {
            Ok(map) => map,
            Err(e) => {
                warn!("npc.reply (кулдауны строк): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let now = crate::time::now_epoch();
        let ctx = context.line_ctx(&available);

        // Валидация связи (stateless): строка → ответ → цель.
        let Some(node_line) = def.lines.iter().find(|line| line.id == cmd.node_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такой строки")];
        };
        let Some(reply) = node_line
            .replies
            .iter()
            .find(|reply| reply.id == cmd.reply_id)
        else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "нет такой реплики")];
        };
        if !crate::dialogs::reply_allowed(reply, &ctx) {
            return vec![Self::err(seq, ErrorCode::TargetGone, "реплика недоступна")];
        }
        let node = match &reply.next {
            // Конец диалога — без следующего узла.
            None => serde_json::Value::Null,
            Some(next_id) => {
                let Some((view, mark)) =
                    crate::dialogs::enter_node(def, next_id, &ctx, now, &shown)
                else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "строка недоступна")];
                };
                if mark {
                    if let Err(e) =
                        db::dialogs::mark_shown(pool, &char_id, &entity.id, next_id, now).await
                    {
                        warn!("npc.reply (отметка показа {next_id}): {e}");
                    }
                }
                serde_json::to_value(&view).unwrap_or_default()
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "node": node, "quests": quests }),
        )]
    }

    // ── магазины NPC (M18/F39) ─────────────────────────────────────────────

    /// Скидка персонажа в bp: аккаунт (премиум — SQL,
    /// `accounts.shop_discount_bp`) + навыки из
    /// `[gameplay.shop].skill_discounts` (множитель за ранг); потолок
    /// применяет `shops::buy_price_gold`.
    async fn shop_discount_bp(&self, char_id: &str, seq: u32) -> Result<i64, Out> {
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
    async fn shop_list(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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
    async fn shop_buy(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
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

    // ── квесты (ФР-12, M12.6) ───────────────────────────────────────────────

    /// Журнал квестов: все контент-квесты + прогресс персонажа (§7).
    async fn quest_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let (defs, progress) = match (
            db::quests::defs(&self.state.pool).await,
            db::quests::progress(&self.state.pool, &char_id).await,
        ) {
            (Ok(d), Ok(p)) => (d, p),
            (Err(e), _) | (_, Err(e)) => {
                warn!("quest.list: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let quests: Vec<serde_json::Value> = defs
            .iter()
            .map(|def| {
                let row = progress.iter().find(|(id, _, _)| id == &def.0);
                serde_json::to_value(db::quests::build_state(def, row)).unwrap_or_default()
            })
            .collect();
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "quests": quests }),
        )]
    }

    /// `cmd/quest.accept` (§7): взять квест → ack `{quest}` +
    /// `evt/quest/updated`.
    async fn quest_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "accept").await
    }

    /// `cmd/quest.turn_in` (§7): сдать выполненный → награда (XP в пул +
    /// предметы) → ack `{quest}` + события.
    async fn quest_turn_in(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "turn_in").await
    }

    /// `cmd/quest.abandon` (§7): бросить → снова «доступен».
    async fn quest_abandon(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.quest_transition(env, seq, "abandon").await
    }

    /// Общий переход квеста (accept/turn_in/abandon) — одна точка проверок.
    async fn quest_transition(&mut self, env: &Envelope, seq: u32, op: &str) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(value) = Self::payload_of::<Value>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let Some(quest_id) = value.get("quest_id").and_then(|q| q.as_str()) else {
            return vec![Self::err(seq, ErrorCode::Internal, "нет quest_id")];
        };
        let quest_id = quest_id.to_string();
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;

        let defs = match db::quests::defs(pool).await {
            Ok(defs) => defs,
            Err(e) => {
                warn!("quest (квесты): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(def) = defs.iter().find(|d| d.0 == quest_id) else {
            return vec![Self::err(seq, ErrorCode::TargetGone, "квеста нет")];
        };
        let progress = match db::quests::progress(pool, &char_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("quest (прогресс): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);

        let mut outs = Vec::new();
        // M21: событие награды-ранга — отправляется ПОСЛЕ ack (иначе
        // клиент, ждущий ack, поглощает событие раньше времени).
        let mut rank_up_evt: Option<ProfessionSnapshot> = None;
        match op {
            "accept" => {
                // M18/F36: лимит текущих квестов — из конфига сервера
                // (§5 `quests.max_active`); взятые = active/turn_in.
                let current = progress
                    .iter()
                    .filter(|(_, state, _)| matches!(state.as_str(), "active" | "turn_in"))
                    .count();
                if current
                    >= usize::try_from(self.state.config.gameplay.quests.max_active)
                        .unwrap_or(usize::MAX)
                {
                    return vec![Self::err(
                        seq,
                        ErrorCode::QuestLimit,
                        "лимит текущих квестов достигнут",
                    )];
                }
                if row.is_some() {
                    return vec![Self::err(seq, ErrorCode::Internal, "квест уже взят")];
                }
                let goal_count = def.3.as_array().map(|a| a.len()).unwrap_or(0);
                if let Err(e) = db::quests::accept(pool, &char_id, &quest_id, goal_count).await {
                    warn!("quest (accept): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
            "turn_in" => {
                let Some((_, state, _)) = row else {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "квест не взят")];
                };
                if state != "turn_in" {
                    return vec![Self::err(seq, ErrorCode::Internal, "цели ещё не выполнены")];
                }
                // M21: награда «повышение ранга» — та же граница
                // уровней (ранг — без золота и тренера); проверка ДО
                // выдачи: квест остаётся «к сдаче» до набора уровня.
                let reward_rank_up = def
                    .4
                    .get("rank_up")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(prof_id) = reward_rank_up {
                    let learned = match db::professions::learned(pool, &char_id).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            warn!("quest (rank_up профессии): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                    match learned.into_iter().find(|(id, _, _, _)| *id == prof_id) {
                        None => {
                            return vec![Self::err(
                                seq,
                                ErrorCode::Profession,
                                "награда: профессия не изучена",
                            )];
                        }
                        Some((_, xp, rank, level)) => {
                            let cfg = &self.state.config.gameplay.professions;
                            let rank_u32 = rank.max(1) as u32;
                            if rank_u32 >= crate::systems::profession_rank_max(cfg) {
                                // Высший ранг достигнут — награда
                                // деградирует, квест сдаётся (без
                                // изменения pending).
                            } else {
                                let need_level = rank_u32.saturating_mul(cfg.rank_span);
                                if (level.max(0) as u32) < need_level {
                                    return vec![Self::err(
                                        seq,
                                        ErrorCode::LevelRequired,
                                        format!(
                                            "награда: нужен уровень {need_level} для ранга {}",
                                            rank_u32 + 1
                                        ),
                                    )];
                                }
                                // Применяем после set_state (ниже).
                                self.pending_rank_up = Some((prof_id, xp, rank + 1, level));
                            }
                        }
                    }
                }
                // Награда: XP в пул (D3/D4) + предметы (слоты — из конфига).
                let reward_xp = def.4.get("xp").and_then(|x| x.as_i64()).unwrap_or(0);
                let reward_items: Vec<(String, i64)> = def
                    .4
                    .get("items")
                    .and_then(|i| i.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|it| {
                                Some((
                                    it.get("type")?.as_str()?.to_string(),
                                    it.get("qty").and_then(|q| q.as_i64()).unwrap_or(1),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // R6: награда и завершение — одной транзакцией; обрыв
                // после выдачи награды больше не оставляет квест
                // «к сдаче» (повторная сдача = дюп XP/предметов).
                {
                    let mut tx = match pool.begin().await {
                        Ok(tx) => tx,
                        Err(e) => {
                            warn!("quest (tx): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    };
                    if reward_xp > 0 {
                        if let Err(e) =
                            db::characters::add_xp_pool(&mut *tx, &char_id, reward_xp).await
                        {
                            warn!("quest (награда XP): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    }
                    for (item_type, qty) in &reward_items {
                        if let Err(e) =
                            db::items::insert_by_type_tx(&mut tx, &char_id, item_type, *qty).await
                        {
                            warn!("quest (награда предмет): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                    }
                    if let Err(e) =
                        db::quests::set_state(&mut *tx, &char_id, &quest_id, "done").await
                    {
                        warn!("quest (done): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                    // M21: применить награду-ранг (граница проверена выше).
                    if let Some((prof_id, xp, new_rank, level)) = self.pending_rank_up.take() {
                        if let Err(e) = db::professions::grant_progress(
                            &mut *tx, &char_id, &prof_id, xp, new_rank, level,
                        )
                        .await
                        {
                            warn!("quest (rank_up {prof_id}): {e}");
                            return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                        }
                        rank_up_evt = Some(self.profession_snapshot(
                            &prof_id,
                            xp,
                            new_rank,
                            level.max(0) as u32,
                        ));
                    }
                    if let Err(e) = tx.commit().await {
                        warn!("quest (commit): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                }
            }
            _ => {
                // abandon: активный квест сбрасывается; завершённый — нельзя.
                if let Some((_, state, _)) = row {
                    if state == "done" {
                        return vec![Self::err(seq, ErrorCode::Internal, "квест завершён")];
                    }
                }
                if let Err(e) = db::quests::abandon(pool, &char_id, &quest_id).await {
                    warn!("quest (abandon): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            }
        }

        // Свежее состояние после перехода → ack `{quest}` + событие.
        let progress = db::quests::progress(pool, &char_id)
            .await
            .unwrap_or_default();
        let row = progress.iter().find(|(id, _, _)| *id == quest_id);
        let state = db::quests::build_state(def, row);
        outs.push(Self::ack_payload(
            seq,
            &serde_json::json!({ "quest": state }),
        ));
        outs.push(out_event(
            "evt/quest/updated",
            None,
            serde_json::json!({ "quest": state }),
        ));
        // Сдача: обновить пул в снапшоте сессии.
        if op == "turn_in" {
            if let Ok(Some(row)) = db::characters::get(pool, &char_id).await {
                if let Some(snap) = self.char.as_mut() {
                    snap.xp_pool = row.xp_pool;
                }
            }
        }
        // M21: награда-ранг — событие после ack (см. объявление выше).
        if let Some(profession) = rank_up_evt {
            outs.push(out_event(
                "evt/craft/profession",
                None,
                serde_json::to_value(CraftProfession {
                    profession,
                    name: None,
                    gain: None,
                })
                .unwrap_or_default(),
            ));
        }
        outs
    }
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
