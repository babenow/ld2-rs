//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{Pong, TimeSync};
use ld2_protocol::types::CharacterState;
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use crate::db;
use crate::state::SharedState;

mod admin;
mod auth;
mod chars;
mod chat;
mod combat;
mod craft;
mod crime;
mod dev;
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
pub(crate) mod presence;
mod quests;
mod rules;
mod socketing;
mod trade;
mod upgrade;

use rules::UpgradePreviewState;
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
        if gates::HARVEST_BLOCKED.contains(&env.msg_type.as_str()) && self.char_is_harvesting() {
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
        if gates::HARVEST_BLOCKED.contains(&env.msg_type.as_str()) || env.msg_type == "cmd/item.use"
        {
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

    /// Выбранный персонаж (для отвязки от мира при дисконнекте, M12.5).
    pub fn selected_char(&self) -> Option<String> {
        self.selected_char_id.clone()
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
