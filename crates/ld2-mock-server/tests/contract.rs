//! Интеграционные тесты контракта мока (PLAN-M1 T3): hello, snapshot,
//! бой, мана+заряды (D4), крафт/профессии, чат/ping/time, chaos.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ld2_mock_server::{serve_configured, serve_with_listener, Chaos, Knobs};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Поднять сервер на эфемерном порту, вернуть ws-url.
async fn start_server(chaos: Chaos) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = serve_with_listener(listener, chaos).await;
    });
    format!("ws://{addr}")
}

/// Сервер с настроенным лимитом слотов (DoD M2: конфиг, не код).
async fn start_server_with_slots(limit: u32) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = serve_configured(listener, Chaos::disabled(), Knobs::active(), limit).await;
    });
    format!("ws://{addr}")
}

struct Client {
    sink: futures_util::stream::SplitSink<WsStream, Message>,
    reader: futures_util::stream::SplitStream<WsStream>,
    seq: u32,
}

impl Client {
    async fn connect(url: &str) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("подключение к моку");
        let (sink, reader) = ws.split();
        Self {
            sink,
            reader,
            seq: 0,
        }
    }

    async fn send(&mut self, msg_type: &str, payload: Value) -> u32 {
        self.seq += 1;
        let env = json!({
            "v": 1, "type": msg_type, "seq": self.seq,
            "ts": "2026-11-03T00:00:00Z", "payload": payload
        });
        self.sink
            .send(Message::Text(env.to_string().into()))
            .await
            .expect("отправка");
        self.seq
    }

    async fn recv_json(&mut self) -> Value {
        loop {
            let msg = self
                .reader
                .next()
                .await
                .expect("поток закрыт раньше события");
            match msg.expect("ошибка ws") {
                Message::Text(t) => return serde_json::from_str(t.as_str()).expect("json"),
                Message::Close(_) => return json!({ "type": "__close__" }),
                _ => continue,
            }
        }
    }

    async fn wait<F: FnMut(&Value) -> bool>(&mut self, mut pred: F) -> Value {
        let fut = async {
            loop {
                let env = self.recv_json().await;
                if pred(&env) {
                    return env;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("таймаут ожидания события")
    }

    async fn ack(&mut self, seq: u32) -> Value {
        self.wait(|v| v["type"] == "ack" && v["seq"] == seq).await
    }

    async fn wait_close(&mut self) {
        let fut = async {
            loop {
                let msg = self.reader.next().await.expect("поток закрыт");
                match msg {
                    Ok(Message::Close(_)) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("таймаут ожидания Close");
    }

    /// Сценарий hello: login demo/demo123 → ack + session.start (payload
    /// наружу — лимит слотов проверяют сами тесты, он конфигурируемый).
    async fn login(&mut self) -> Value {
        let seq = self
            .send(
                "cmd/auth.login",
                json!({ "login": "demo", "password": "demo123" }),
            )
            .await;
        let ack = self.ack(seq).await;
        assert_eq!(ack["result"], "ok", "login: {ack}");
        let start = self.wait(|v| v["type"] == "evt/session.start").await;
        assert_eq!(start["payload"]["profession_limit"], 2, "D10");
        let kinds = start["payload"]["crime"]["kinds"]
            .as_array()
            .expect("kinds");
        assert_eq!(kinds.len(), 3, "D5: Убийца + Мародёр + Вор");
        assert!(kinds.iter().any(|k| k["kind"] == "thief"));
        assert_eq!(
            start["payload"]["magic"]["mana"]["regen_out_s"], 45,
            "D4: конфиг маны"
        );
        assert_eq!(
            start["payload"]["magic"]["mana"]["rune_cost"], 10,
            "M5: стоимость каста в мане"
        );
        start["payload"].clone()
    }

    /// Сценарий snapshot: select c1 → ack + полный снапшот.
    async fn select(&mut self) -> Value {
        let seq = self
            .send("cmd/char.select", json!({ "char_id": "c1" }))
            .await;
        let ack = self.ack(seq).await;
        assert_eq!(ack["result"], "ok", "select: {ack}");
        assert!(ack["state_version"].is_number(), "select меняет состояние");
        self.wait(|v| v["type"] == "evt/state/snapshot").await
    }
}

// ─── hello ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hello_scenario_login_grants_session_config() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    let start = c.login().await;
    assert_eq!(
        start["char_slot_limit"], 8,
        "D6: дефолтный лимит из конфига"
    );
}

/// DoD M2: лимит слотов задаётся конфигом мока (`--char-slots` / CLI),
/// без правки кода клиента: лимит 3 → 4-й персонаж отклоняется.
#[tokio::test]
async fn char_slot_limit_comes_from_config() {
    let url = start_server_with_slots(3).await;
    let mut c = Client::connect(&url).await;
    let start = c.login().await;
    assert_eq!(
        start["char_slot_limit"], 3,
        "лимит пришёл из конфига сервера, не из кода"
    );

    // c1 уже есть в фикстурах → создаём ещё два, четвёртый — отбой.
    for name in ["Арн", "Бор"] {
        let seq = c
            .send(
                "cmd/char.create",
                json!({ "name": name, "profile": "warrior" }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "создание {name}: {ack}");
    }
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": "Вест", "profile": "mage" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_CHAR_LIMIT",
        "4-й персонаж при лимите 3"
    );

    // Список отражает лимит
    let seq = c.send("cmd/char.list", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["payload"]["characters"].as_array().expect("list").len(),
        3
    );
}

#[tokio::test]
async fn bad_credentials_rejected() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": "demo", "password": "wrong" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "error");
    assert_eq!(ack["error"]["code"], "E_BAD_CREDENTIALS");
}

#[tokio::test]
async fn commands_before_login_require_auth() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    let seq = c.send("cmd/char.select", json!({ "char_id": "c1" })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_AUTH_EXPIRED");
}

// ─── snapshot / мир ─────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_scenario_full_state() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    let snap = c.select().await;

    let p = &snap["payload"];
    assert_eq!(p["state_version"], 1002, "версия после login+select");
    assert_eq!(p["char"]["mp"]["cur"], 45, "D4: mp — объект");
    assert_eq!(p["char"]["xp_pool"], 120, "пул опыта квестов");
    assert_eq!(p["char"]["equip"]["staff"], "i_17");
    assert_eq!(p["char"]["crime"]["statuses"][0]["kind"], "murderer");
    let sockets = &p["char"]["inventory"][0]["sockets"];
    assert_eq!(sockets[0]["rune"]["charges"], 7, "руна в сокете 0");
    assert!(sockets[1]["rune"].is_null(), "сокет 1 свободен");
    assert_eq!(p["location"]["id"], "l_village");
    assert_eq!(p["location"]["flags"][0], "safe");

    // Переход в лес и обратно
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    assert_eq!(ack["payload"]["location"]["id"], "l_forest");
    let changed = c.wait(|v| v["type"] == "evt/location/changed").await;
    assert_eq!(changed["payload"]["location"]["flags"][0], "pvp");

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_west" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_EXIT_BLOCKED");

    // Труп игрока — маркер мародёрства (ФР-11)
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["payload"]["was_player"], true);
    assert!(ack["payload"]["loot"].is_array());
}

#[tokio::test]
async fn resync_returns_fresh_snapshot() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1002 }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let snap = c.wait(|v| v["type"] == "evt/state/snapshot").await;
    assert!(snap["payload"]["char"]["name"] == "Ким");
}

// ─── бой ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn combat_kills_wolf_and_leaves_corpse() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    c.send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    // Волк 18 hp, урон 7 → три удара
    for i in 0..3 {
        let seq = c
            .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "удар {i}: {ack}");
        let hit = c.wait(|v| v["type"] == "evt/combat/hit").await;
        assert_eq!(hit["payload"]["amount"], 7);
        let _ = c.wait(|v| v["type"] == "evt/entity/hp").await;
        if i == 2 {
            let _ = c.wait(|v| v["type"] == "evt/entity/despawn").await;
            let corpse = c.wait(|v| v["type"] == "evt/corpse/created").await;
            assert_eq!(corpse["payload"]["entity"]["kind"], "corpse");
        }
    }
    // Цель мертва — атака отклоняется
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE");
}

// ─── магия: D4 двойной ресурс ───────────────────────────────────────────────

#[tokio::test]
async fn rune_cast_costs_mana_and_charge_scroll_free() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    // Свиток: без маны (D4), свиток израсходован
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": "s_1", "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "свиток: {ack}");
    let cast = c.wait(|v| v["type"] == "evt/magic/cast").await;
    assert_eq!(cast["payload"]["what"], "scroll");
    assert!(
        cast["payload"]["mp_left"].is_null(),
        "D4: свиток ману не тратит"
    );

    // Руна: мана 45 → 35 и заряд 7 → 6
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": "i_17", "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "руна: {ack}");
    let cast = c.wait(|v| v["type"] == "evt/magic/cast").await;
    assert_eq!(cast["payload"]["what"], "rune");
    assert_eq!(cast["payload"]["charges_left"], 6, "заряд −1");
    assert_eq!(cast["payload"]["mp_left"], 35, "мана −10 (D4)");
    let _ = c.wait(|v| v["type"] == "evt/item/state").await;

    // Пустой сокет → E_NO_RUNE_SLOT
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": "i_17", "socket_index": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT");

    // Расходуем ману до нуля: 35 → 25 → 15 → 5, пятый каст → E_NOT_ENOUGH_MP
    for expect_mp in [25, 15, 5] {
        let seq = c
            .send(
                "cmd/magic.rune_cast",
                json!({ "staff_uid": "i_17", "socket_index": 0 }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "каст при mp={expect_mp}: {ack}");
        let cast = c.wait(|v| v["type"] == "evt/magic/cast").await;
        assert_eq!(cast["payload"]["mp_left"], expect_mp);
        let _ = c.wait(|v| v["type"] == "evt/item/state").await;
    }
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": "i_17", "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NOT_ENOUGH_MP", "мана кончилась");
}

// ─── XP-пул (D3) ────────────────────────────────────────────────────────────

#[tokio::test]
async fn xp_invest_spends_quest_pool() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 50 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    assert_eq!(ack["payload"]["pool_left"], 70, "120 − 50");
    let xp = c.wait(|v| v["type"] == "evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "sword");
    assert_eq!(xp["payload"]["pool_left"], 70);

    // Вложить больше пула нельзя
    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 999 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_XP_POOL");
}

// ─── крафт / профессии (skills.md) ──────────────────────────────────────────

#[tokio::test]
async fn craft_and_profession_limits() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    // Рецепты из skills.md §5.4
    let seq = c.send("cmd/craft.recipes", json!({})).await;
    let ack = c.ack(seq).await;
    let recipes = ack["payload"]["recipes"].as_array().expect("recipes");
    assert_eq!(recipes.len(), 2);
    assert!(recipes.iter().any(|r| r["id"] == "rec_sword_iron"));

    // Нет ингредиентов для зелья (herb отсутствует)
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_hp", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_INGREDIENTS");

    // Меч из слитков/досок есть в инвентаре
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_sword_iron", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "крафт меча: {ack}");
    let result = c.wait(|v| v["type"] == "evt/craft/result").await;
    assert_eq!(result["payload"]["ok"], true);
    let added = c.wait(|v| v["type"] == "evt/item/added").await;
    assert_eq!(added["payload"]["item"]["type"], "sword_iron");

    // Профессии: scribe уже есть; blacksmith влезает (лимит 2), alchemist — нет
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "blacksmith" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let _ = c.wait(|v| v["type"] == "evt/craft/profession").await;

    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_PROFESSION_LIMIT", "лимит 2 (D10)");
}

// ─── улучшение (ФР-10) ──────────────────────────────────────────────────────

#[tokio::test]
async fn upgrade_preview_then_start() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": "i_17" }))
        .await;
    let ack = c.ack(seq).await;
    let preview_id = ack["payload"]["preview_id"]
        .as_str()
        .expect("preview_id")
        .to_string();
    assert_eq!(ack["payload"]["chance"], 0.75);

    // Чужой preview_id → E_UPGRADE_PREVIEW_STALE
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": "i_17", "preview_id": "pv_hacked" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_UPGRADE_PREVIEW_STALE");

    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": "i_17", "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let result = c.wait(|v| v["type"] == "evt/upgrade/result").await;
    assert_eq!(result["payload"]["new_upgrade"], 3, "было +2 → стало +3");
    let state = c.wait(|v| v["type"] == "evt/item/state").await;
    assert_eq!(state["payload"]["patch"]["upgrade"], 3);
}

// ─── чат / ping / time ──────────────────────────────────────────────────────

#[tokio::test]
async fn chat_ping_and_time_sync() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    // Чат: ack + эхо с тегами статусов (ФР-11 бейджи)
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": "привет" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let msg = c.wait(|v| v["type"] == "evt/chat/message").await;
    assert_eq!(msg["payload"]["name"], "Ким");
    assert_eq!(msg["payload"]["text"], "привет");
    assert_eq!(msg["payload"]["tags"][0], "murderer");

    // Длинное сообщение → E_RATE_LIMITED (max_len 500)
    let long = "х".repeat(501);
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": long }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RATE_LIMITED");

    // ping → ack + evt/pong
    let seq = c.send("cmd/ping", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let pong = c.wait(|v| v["type"] == "evt/pong").await;
    assert!(pong["payload"]["ts"].is_string());

    // time.sync → evt/time.sync c epoch-секундами (§1.1)
    let seq = c.send("cmd/time.sync", json!({})).await;
    let _ack = c.ack(seq).await;
    let sync = c.wait(|v| v["type"] == "evt/time.sync").await;
    assert!(sync["payload"]["server_ts"].as_i64().expect("epoch") > 1_700_000_000);
}

// ─── chaos ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chaos_drops_connection_after_first_message() {
    // every=1 → обрыв после КАЖДОГО обработанного сообщения (детерминированно).
    let url = start_server(Chaos::new(true, 1, 42)).await;
    let mut c = Client::connect(&url).await;
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": "demo", "password": "demo123" }),
        )
        .await;
    // Ответы до обрыва доходят…
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    // …потом соединение рвётся (клиент уйдёт в reconnect — T2/T4).
    c.wait_close().await;
}

// ─── M4.1: лут и смерть ─────────────────────────────────────────────────────

/// Лут с монстра — автодобавление в инвентарь (ФР-07).
#[tokio::test]
async fn kill_drops_loot_into_inventory() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    c.send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    c.ack(c.seq).await;

    for i in 0..3 {
        let seq = c
            .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "удар {i}");
        if i == 2 {
            let added = c.wait(|v| v["type"] == "evt/item/added").await;
            let name = added["payload"]["item"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(name.contains("Волк"), "трофея с волка: {name}");
        }
    }

    // Предмет реально в инвентаре: его можно надеть (иначе E_ITEM_GONE).
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": "loot_m_3", "slot": "body" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "трофея надевается: {ack}");
}

/// DoD M4-контракт: смерть → сводка утраченного → полная потеря → респаун.
#[tokio::test]
async fn death_scenario_loses_everything_and_respawns() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": "/scenario death" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "триггер сценария: {ack}");

    // ФР-06: сводка утраченного (посох, свиток, ингредиенты — стартовый набор).
    let death = c.wait(|v| v["type"] == "evt/char/death").await;
    assert_eq!(death["payload"]["victim_char"], true);
    assert_eq!(death["payload"]["respawn"]["location_id"], "l_village");
    let lost = death["payload"]["lost_items"].as_array().expect("сводка");
    assert!(
        lost.len() >= 4,
        "утрачено всё: {} предметов, первый — {}",
        lost.len(),
        lost[0]["name"]
    );

    // Ресурсы восстановлены и респаун в деревню.
    let stats = c.wait(|v| v["type"] == "evt/char/stats").await;
    assert_eq!(
        stats["payload"]["patch"]["hp"]["cur"], stats["payload"]["patch"]["hp"]["max"],
        "hp полный после респауна"
    );
    let changed = c.wait(|v| v["type"] == "evt/location/changed").await;
    assert_eq!(changed["payload"]["location"]["id"], "l_village");

    // Инвентарь пуст — прежний предмет недоступен.
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": "i_17", "slot": "staff" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_ITEM_GONE",
        "посох потерян при смерти"
    );
}

// ─── M5: улучшение и инкрустация ────────────────────────────────────────────

/// DoD M5-мок: /scenario upgrade_fail → провал по break_on_fail,
/// предмет разрушен (событие результата + удаление из инвентаря).
#[tokio::test]
async fn upgrade_fail_scenario_destroys_item() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    // Триггер сценария — обычный текст чата.
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": "/scenario upgrade_fail" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let notice = c.wait(|v| v["type"] == "evt/chat/message").await;
    assert!(
        notice["payload"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("провалится"),
        "сервер предупреждает о сценарии"
    );

    // Превью → старт → провал.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": "i_17" }))
        .await;
    let ack = c.ack(seq).await;
    let preview_id = ack["payload"]["preview_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": "i_17", "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["result"], "ok",
        "команда прошла — результат событием (ФР-10)"
    );

    let result = c.wait(|v| v["type"] == "evt/upgrade/result").await;
    assert_eq!(result["payload"]["ok"], false, "провал");
    assert_eq!(result["payload"]["item_destroyed"], true, "break_on_fail");
    let removed = c.wait(|v| v["type"] == "evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], "i_17");

    // Предмета больше нет: новое превью — ошибка.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": "i_17" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE");
}

/// Инкрустация: извлечь руну из сокета и вставить обрати (ФР-09).
#[tokio::test]
async fn socket_extract_insert_roundtrip() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;

    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": "i_17", "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение: {ack}");
    let rune = &ack["payload"]["rune"];
    assert_eq!(rune["name"], "Руна огня");
    assert_eq!(rune["stats"]["charges"], 7, "заряды в stats предмета");
    let staff = &ack["payload"]["staff"];
    assert!(staff["sockets"][0]["rune"].is_null(), "сокет 0 освобождён");

    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": "i_17", "socket_index": 0, "rune_uid": "rune_i_17_0" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "вставка: {ack}");
    let staff = &ack["payload"]["staff"];
    assert_eq!(staff["sockets"][0]["rune"]["charges"], 7, "руна на месте");

    // Руна потреблена при вставке (удалена из инвентаря) — повтор невозможен.
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": "i_17", "socket_index": 0, "rune_uid": "rune_i_17_0" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_ITEM_GONE",
        "предмет-руна потреблён при вставке"
    );

    // Извлечение из пустого сокета → E_NO_RUNE_SLOT.
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": "i_17", "socket_index": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT", "сокет 1 пуст");
}

// ─── M6: преступность и бой ─────────────────────────────────────────────────

/// **DoD M6:** формула §4.10 на моке — сбор лута трупа игрока вешает
/// «Мародёр» со сроком `k·t·(1 − T/T_MAX)` (частичное сложение, НЕ полное).
#[tokio::test]
async fn corpse_loot_maraduer_partial_summation_formula() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    c.send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    c.ack(c.seq).await;

    // Осмотр трупа игрока: лут доступен.
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["payload"]["was_player"], true);
    assert_eq!(ack["payload"]["looted"], false);
    let loot_uid = ack["payload"]["loot"][0]["uid"]
        .as_str()
        .expect("uid лута")
        .to_string();

    // Сбор лута → статус «Мародёр».
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": loot_uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "сбор лута: {ack}");

    let _added = c.wait(|v| v["type"] == "evt/item/added").await;
    let looted = c.wait(|v| v["type"] == "evt/corpse/looted").await;
    assert_eq!(looted["payload"]["was_player"], true);

    let added = c.wait(|v| v["type"] == "evt/crime/added").await;
    let entry = &added["payload"]["log_entry"];
    assert_eq!(entry["kind"], "marauder");
    let delta = entry["delta_s"].as_i64().expect("вклад");
    let total = entry["total_s"].as_i64().expect("итог");
    let before = total - delta;

    // Формула §4.10: delta = round(k · t · (1 − T/T_MAX)), k=0.5, t=7200.
    let expected = (0.5_f64 * 7200.0 * (1.0 - before as f64 / 604_800.0)).round() as i64;
    assert!(
        (delta - expected).abs() <= 1,
        "вклад {delta} ≠ формуле {expected} (T={before})"
    );
    assert!(
        delta < 7200,
        "частичное сложение: вклад {delta} < базы 7200 (§4.10)"
    );
    assert_eq!(total, before + delta, "итог = T + вклад");

    // Статус применён сервером (полная замена §8).
    let status = c.wait(|v| v["type"] == "evt/crime/status").await;
    let marauder = status["payload"]["statuses"]
        .as_array()
        .expect("статусы")
        .iter()
        .find(|s| s["kind"] == "marauder")
        .expect("мараудёр назначен");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expires = marauder["expires_at"].as_i64().expect("срок");
    assert!((expires - now - delta).abs() <= 2, "срок ≈ now + вклад");

    // Повторный сбор невозможен.
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": loot_uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "лут уже собран");
}

/// ФР-05: активка шлёт hit + evt/combat/cooldown (требует sword ≥ 5 —
/// фикстура M6 обновлена до rank 5).
#[tokio::test]
async fn skill_use_emits_hit_and_cooldown() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    c.send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    c.ack(c.seq).await;

    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["result"], "ok",
        "активка проходит требование rank 5: {ack}"
    );

    let hit = c.wait(|v| v["type"] == "evt/combat/hit").await;
    assert_eq!(hit["payload"]["amount"], 13, "урон активки");
    let cd = c.wait(|v| v["type"] == "evt/combat/cooldown").await;
    assert_eq!(cd["payload"]["skill_id"], "power_strike");
    let until = cd["payload"]["until_epoch"].as_i64().expect("до");
    assert!(until > 0, "кулдаун в будущем");
}

// ─── M7: квесты ─────────────────────────────────────────────────────────────

/// M7: убийство двигает цель квеста «к сдаче», сдача начисляет пул опыта
/// (evt/quest/updated {pool_left} — §7, ФР-12).
#[tokio::test]
async fn quest_progress_then_turn_in_awards_pool() {
    let url = start_server(Chaos::disabled()).await;
    let mut c = Client::connect(&url).await;
    c.login().await;
    c.select().await;
    c.send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    c.ack(c.seq).await;

    // Убийство волка (4/5 → 5/5 «к сдаче»).
    for _ in 0..3 {
        let seq = c
            .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok");
    }
    let progress = c.wait(|v| v["type"] == "evt/quest/progress").await;
    assert_eq!(progress["payload"]["quest"]["id"], "q_wolves");
    assert_eq!(progress["payload"]["quest"]["state"], "turn_in");
    assert_eq!(progress["payload"]["quest"]["goals"][0]["cur"], 5);

    // Сдача: пул 120 + 150 = 270 (§7).
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolves" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "сдача: {ack}");
    let updated = c.wait(|v| v["type"] == "evt/quest/updated").await;
    assert_eq!(updated["payload"]["quest"]["state"], "done");
    assert_eq!(
        updated["payload"]["pool_left"], 270,
        "пул: 120 стартовых + 150 награды"
    );
}
