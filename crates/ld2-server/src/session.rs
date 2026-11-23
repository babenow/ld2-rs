//! Сессия WS-соединения: seq-окно с кэшем ack (§2.4.2), диспетчер `cmd/*`
//! → `ack` + `evt/*` (§2.2: события — строго после своего ack) и
//! авторизация соединения (ADR-006, D29/D30).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use ld2_protocol::envelope::{Ack, Envelope, PROTOCOL_VERSION};
use ld2_protocol::evt::{CharXp, ItemAdded, ItemState, LocationChanged, Pong, TimeSync};
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

use rules::{content_bonuses, location_snapshot, UpgradePreviewState};
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
