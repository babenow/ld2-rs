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
        self.select_char("c1").await
    }

    /// Выбор конкретного персонажа (два аккаунта — c1/c2, DoD M8).
    async fn select_char(&mut self, char_id: &str) -> Value {
        let seq = self
            .send("cmd/char.select", json!({ "char_id": char_id }))
            .await;
        let ack = self.ack(seq).await;
        assert_eq!(ack["result"], "ok", "select {char_id}: {ack}");
        assert!(ack["state_version"].is_number(), "select меняет состояние");
        self.wait(|v| v["type"] == "evt/state/snapshot").await
    }

    /// Вход произвольным аккаунтом (demo / demo2 — DoD M8 «2 клиента»).
    async fn login_as(&mut self, login: &str, password: &str) -> Value {
        let seq = self
            .send(
                "cmd/auth.login",
                json!({ "login": login, "password": password }),
            )
            .await;
        let ack = self.ack(seq).await;
        assert_eq!(ack["result"], "ok", "login {login}: {ack}");
        let start = self.wait(|v| v["type"] == "evt/session.start").await;
        start["payload"].clone()
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
    let chars = ack["payload"]["characters"].as_array().expect("list");
    assert_eq!(chars.len(), 3);
    assert_eq!(
        chars[0]["profile"], "warrior",
        "profile из char.create в списке (ADR-005)"
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

// ─── M8: 2 клиента против мока (DoD: party + сделка + whisper) ──────────────

/// Два соединения с разными аккаунтами: состояния не перетирают друг друга
/// (до M8 мок делил одну Session на все соединения).
#[tokio::test]
async fn m8_two_accounts_do_not_clobber_each_other() {
    let url = start_server(Chaos::disabled()).await;
    let mut a = Client::connect(&url).await;
    let mut b = Client::connect(&url).await;

    let start_a = a.login().await; // demo → c1 «Ким»
    let start_b = b.login_as("demo2", "demo123").await; // demo2 → c2 «Астра»

    let names = |start: &Value| -> Vec<String> {
        start["characters"]
            .as_array()
            .expect("characters")
            .iter()
            .map(|c| c["name"].as_str().expect("name").to_string())
            .collect()
    };
    assert_eq!(names(&start_a), vec!["Ким"], "аккаунт demo → только c1");
    assert_eq!(
        start_a["characters"][0]["profile"], "warrior",
        "profile в session.start (ADR-005)"
    );
    assert_eq!(names(&start_b), vec!["Астра"], "аккаунт demo2 → только c2");

    let snap_a = a.select().await;
    let snap_b = b.select_char("c2").await;
    assert_eq!(snap_a["payload"]["char"]["name"], "Ким");
    assert_eq!(snap_b["payload"]["char"]["name"], "Астра");

    // A продолжает быть собой: его снапшот после входа B не изменился.
    let seq = a.send("cmd/location.look", json!({})).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "A жив после входа B: {ack}");
}

/// Whitelist DoD: whisper A → B доставляется адресату (роутинг по имени).
#[tokio::test]
async fn m8_whisper_routed_to_second_client() {
    let url = start_server(Chaos::disabled()).await;
    let mut a = Client::connect(&url).await;
    let mut b = Client::connect(&url).await;
    a.login().await;
    b.login_as("demo2", "demo123").await;
    a.select().await;
    b.select_char("c2").await;

    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "whisper", "to": "Астра", "text": "привет!" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "whisper: {ack}");

    // Эхо — отправителю.
    let echo = a.wait(|v| v["type"] == "evt/chat/message").await;
    assert_eq!(echo["payload"]["text"], "привет!");
    // И адресату — B получает своё событие.
    let got = b.wait(|v| v["type"] == "evt/chat/message").await;
    assert_eq!(got["payload"]["channel"], "whisper");
    assert_eq!(got["payload"]["from"], "c1");
    assert_eq!(got["payload"]["name"], "Ким");
    assert_eq!(got["payload"]["text"], "привет!");
}

/// Глобальный чат слышат все подключённые клиенты (broadcast).
#[tokio::test]
async fn m8_global_chat_broadcasts_to_others() {
    let url = start_server(Chaos::disabled()).await;
    let mut a = Client::connect(&url).await;
    let mut b = Client::connect(&url).await;
    a.login().await;
    b.login_as("demo2", "demo123").await;
    a.select().await;
    b.select_char("c2").await;

    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "всем привет" }),
        )
        .await;
    a.ack(seq).await;
    a.wait(|v| v["type"] == "evt/chat/message").await; // эхо A
    let got = b.wait(|v| v["type"] == "evt/chat/message").await;
    assert_eq!(got["payload"]["text"], "всем привет");
}

/// Resume по refresh-токену восстанавливает персонажа после обрыва (§4.5):
/// состояние аккаунта переживает новое соединение.
#[tokio::test]
async fn m8_resume_restores_character_after_disconnect() {
    let url = start_server(Chaos::disabled()).await;
    let start = {
        let mut c = Client::connect(&url).await;
        let start = c.login().await;
        c.select().await;
        // Обрыв: клиент уходит, Drop сессии сохраняет состояние аккаунта.
        c.sink.send(Message::Close(None)).await.expect("close");
        drop(c);
        start
    };
    // Даём серверу обработать закрытие и drop сессии до нового подключения.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let refresh = start["session"]["refresh"]["token"]
        .as_str()
        .expect("refresh-токен из session.start");

    // Новое соединение: resume → персонаж на месте.
    let mut c2 = Client::connect(&url).await;
    let seq = c2
        .send("cmd/auth.resume", json!({ "refresh_token": refresh }))
        .await;
    let ack = c2.ack(seq).await;
    assert_eq!(ack["result"], "ok", "resume: {ack}");
    c2.wait(|v| v["type"] == "evt/session.start").await;
    let seq = c2
        .send(
            "cmd/state.resync",
            json!({ "last_state_version": ack["state_version"] }),
        )
        .await;
    c2.ack(seq).await;
    let snap = c2.wait(|v| v["type"] == "evt/state/snapshot").await;
    assert_eq!(
        snap["payload"]["char"]["name"], "Ким",
        "resume вернул персонажа без повторного char.select"
    );
}

/// Группа: A создаёт и приглашает B → B принимает → оба видят состав (ФР-14).
#[tokio::test]
async fn m8_party_invite_accept_two_clients() {
    let url = start_server(Chaos::disabled()).await;
    let mut a = Client::connect(&url).await;
    let mut b = Client::connect(&url).await;
    a.login().await;
    b.login_as("demo2", "demo123").await;
    a.select().await;
    b.select_char("c2").await;

    // A создаёт группу.
    let seq = a.send("cmd/party.create", json!({})).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "create: {ack}");
    assert_eq!(ack["payload"]["party"]["leader"], "c1");
    let ev = a.wait(|v| v["type"] == "evt/party/updated").await;
    assert_eq!(ev["payload"]["party"]["members"][0]["id"], "c1");

    // A приглашает B (по char_id) → B получает приглашение.
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": "c2" }))
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "invite: {ack}");
    let inv = b
        .wait(|v| v["type"] == "evt/party/updated" && v["payload"]["invite"].is_object())
        .await;
    let invite_id = inv["payload"]["invite"]["invite_id"]
        .as_str()
        .expect("invite_id")
        .to_string();
    assert_eq!(inv["payload"]["invite"]["from_name"], "Ким");

    // B принимает → ack {party} + событие обеим сторонам.
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "accept: {ack}");
    let members = |v: &Value| -> Vec<String> {
        v["payload"]["party"]["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| m["id"].as_str().expect("id").to_string())
            .collect()
    };
    let mine = b.wait(|v| v["type"] == "evt/party/updated").await;
    assert_eq!(members(&mine), vec!["c1", "c2"], "B видит состав");
    let theirs = a.wait(|v| v["type"] == "evt/party/updated").await;
    assert_eq!(members(&theirs), vec!["c1", "c2"], "A видит состав");
}

/// Сделка между двумя клиентами: зеркало you/them, общая версия, обмен (ФР-15).
#[tokio::test]
async fn m8_trade_mirrored_versioned_exchange_two_clients() {
    let url = start_server(Chaos::disabled()).await;
    let mut a = Client::connect(&url).await;
    let mut b = Client::connect(&url).await;
    a.login().await;
    b.login_as("demo2", "demo123").await;
    a.select().await;
    b.select_char("c2").await;

    // A открывает сделку с B.
    let seq = a.send("cmd/trade.open", json!({ "player_id": "c2" })).await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "open: {ack}");
    let offer_id = ack["payload"]["offer_id"]
        .as_str()
        .expect("offer_id")
        .to_string();
    let own = a.wait(|v| v["type"] == "evt/trade/state").await;
    assert_eq!(own["payload"]["version"], 1);
    // B тоже получил открытие сделки (зеркало).
    let theirs = b.wait(|v| v["type"] == "evt/trade/state").await;
    assert_eq!(theirs["payload"]["offer_id"], offer_id);
    assert_eq!(theirs["payload"]["version"], 1);

    // A кладёт предмет (слиток m_1) → версия растёт, B видит в `them`.
    let seq = a
        .send(
            "cmd/trade.add_item",
            json!({ "offer_id": offer_id, "item_uid": "m_1" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "add_item: {ack}");
    let mine = a.wait(|v| v["type"] == "evt/trade/state").await;
    assert_eq!(mine["payload"]["you"][0]["uid"], "m_1");
    assert_eq!(mine["payload"]["you"].as_array().unwrap().len(), 1);
    let seen_by_b = b.wait(|v| v["type"] == "evt/trade/state").await;
    assert_eq!(
        seen_by_b["payload"]["them"][0]["uid"], "m_1",
        "зеркало: у B предмет A в `them`"
    );
    assert!(seen_by_b["payload"]["you"].as_array().unwrap().is_empty());
    let version = seen_by_b["payload"]["version"].as_u64().expect("version");

    // Сверка версии: устаревшая версия → E_TRADE_STALE (§9).
    let seq = a
        .send(
            "cmd/trade.propose",
            json!({ "offer_id": offer_id, "version": 1 }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "error");
    assert_eq!(ack["error"]["code"], "E_TRADE_STALE");

    // Обе стороны фиксируют актуальную версию → done + обмен предметами.
    let seq = a
        .send(
            "cmd/trade.propose",
            json!({ "offer_id": offer_id, "version": version }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "propose: {ack}");
    a.wait(|v| v["type"] == "evt/trade/state").await; // фиксация A

    let seq = b
        .send(
            "cmd/trade.accept",
            json!({ "offer_id": offer_id, "version": version + 1 }),
        )
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["result"], "ok", "accept: {ack}");

    let closed_a = a.wait(|v| v["type"] == "evt/trade/closed").await;
    assert_eq!(closed_a["payload"]["reason"], "done");
    let closed_b = b.wait(|v| v["type"] == "evt/trade/closed").await;
    assert_eq!(closed_b["payload"]["reason"], "done");

    // Обмен: B получил слиток m_1, A — потеряв его, имеет событие removed.
    let added_b = b.wait(|v| v["type"] == "evt/item/added").await;
    assert_eq!(added_b["payload"]["item"]["uid"], "m_1");
    let removed_a = a.wait(|v| v["type"] == "evt/item/removed").await;
    assert_eq!(removed_a["payload"]["uid"], "m_1");
    assert_eq!(removed_a["payload"]["reason"], "traded", "§8 ItemRemoved");
}
