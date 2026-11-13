//! Golden-тесты: сверка `docs/protocol.md` ↔ типы `ld2-protocol` (PLAN-M1 T1).
//!
//! Примеры JSON здесь — дословные выдержки из §5 (session.start), §6 (типы),
//! §12 (диалоги) и ТЗ §4.8.2 (снапшот). Правка документа без правки типов
//! (или наоборот) ломает эти тесты.

use ld2_protocol::cmd::{
    AuthLogin, CharCreate, ChatSend, GuildRankSet, LocationMove, MagicRuneCast, Ping, SkillTrain,
    StateResync, TradeAddItem,
};
use ld2_protocol::evt::{
    ChatMessage, CombatHit, CrimeStatusUpdate, Event, MagicCast, Pong, StateDelta, StateSnapshot,
    TradeState,
};
use ld2_protocol::features::SessionConfig;
use ld2_protocol::types::{ItemSnapshot, Mp, Severity};
use ld2_protocol::{Ack, Command, Envelope, ErrorCode};

const TS: &str = "2026-11-03T12:00:00Z";

// ─── §5: session.start (дословно) ────────────────────────────────────────────

const SESSION_START_JSON: &str = r#"{
  "server": { "name": "ld2-dev", "region": "ru", "min_client": null },
  "session": { "access_token": "…", "expires_in_s": 900,
               "refresh": { "token": "…", "expires_in_s": 2592000 } },
  "char_slot_limit": 8,
  "profession_limit": 2,
  "role": "player",
  "features": {
    "loot.roll": false, "respawn.inn": true, "guild.bank": false,
    "upgrade.rules": "break_on_fail", "trade.value_hint": true, "chat.slash": true
  },
  "crime": {
    "t_max_s": 604800, "blend_k": 0.5,
    "base": { "murderer_s": 21600, "marauder_s": 7200 },
    "kinds": [
      { "kind": "murderer", "title": "Убийца", "severity": "high" },
      { "kind": "marauder", "title": "Мародёр", "severity": "medium" },
      { "kind": "thief", "title": "Вор", "severity": "medium" }
    ]
  },
  "chat": { "server_history": 200,
            "channels": ["system","location","global","party","guild","whisper"],
            "max_len": 500, "cooldown_s": 1.5 },
  "inventory": { "slots": 40, "equip_slots": ["head","shoulders","bracers","gloves","body","legs","feet","cloak","ring1","ring2","amulet","main_hand","off_hand","artifact1","artifact2"] },
  "upgrade": { "max_level": 9, "preview": true, "shows_chance": true },
  "magic": { "rune_recharge": "item", "scroll_requires_target": true,
             "mana": { "regen_out_s": 45, "regen_in_pct_s": 1.0, "rune_cost": 10 } },
  "quests": { "max_active": 10 },
  "characters": [
    { "id": "c1", "name": "Ким", "location": "Черный лес",
      "summary": "Навыки: меч 3, аркана 1", "crime": ["murderer"], "online": true }
  ]
}"#;

#[test]
fn session_start_matches_doc_example() {
    let cfg: SessionConfig = serde_json::from_str(SESSION_START_JSON).expect("§5 парсится");
    assert_eq!(cfg.server.name, "ld2-dev");
    assert_eq!(cfg.server.min_client, None);
    assert_eq!(cfg.char_slot_limit, 8, "D6: слоты — из конфига");
    assert_eq!(cfg.profession_limit, 2, "D10");
    // M14/F19: роль аккаунта; отсутствие поля (старый сервер) → player.
    assert_eq!(cfg.role, "player");
    // M18/F36: лимит текущих квестов §5.
    assert_eq!(cfg.quests.max_active, 10, "quests.max_active §5");
    let legacy = SESSION_START_JSON
        .replace(r#""role": "player","#, "")
        .replace(r#""quests": { "max_active": 10 },"#, "");
    let old_cfg: SessionConfig = serde_json::from_str(&legacy).expect("legacy §5");
    assert_eq!(old_cfg.role, "player", "serde(default) для старых серверов");
    assert_eq!(
        old_cfg.quests.max_active, 10,
        "M18/F36: старый сервер без quests → дефолт10 (§11)"
    );
    assert_eq!(cfg.crime.t_max_s, 604800);
    assert!((cfg.crime.blend_k - 0.5).abs() < f64::EPSILON);
    assert_eq!(cfg.crime.kinds.len(), 3, "D5: Убийца + Мародёр + Вор");
    assert_eq!(cfg.crime.kinds[2].kind, "thief");
    assert_eq!(cfg.crime.kinds[0].severity, Severity::High);
    assert_eq!(cfg.chat.channels.len(), 6);
    assert!((cfg.chat.cooldown_s - 1.5).abs() < f64::EPSILON);
    assert_eq!(cfg.magic.mana.regen_out_s, 45, "D4: конфиг маны");
    assert_eq!(
        cfg.magic.mana.rune_cost, 10,
        "M5: стоимость каста в мане (ФР-09)"
    );
    assert_eq!(cfg.characters[0].id, "c1");
    assert_eq!(cfg.characters[0].crime, vec!["murderer".to_string()]);
    // Capability-доступ и незнакомые ключи
    assert_eq!(cfg.feature_bool("loot.roll"), Some(false));
    assert_eq!(cfg.feature_bool("respawn.inn"), Some(true));
    assert_eq!(cfg.feature("nonexistent.key"), None);

    // Round-trip: сериализация → десериализация → равенство структур
    let text = serde_json::to_string(&cfg).expect("сериализация");
    let parsed: SessionConfig = serde_json::from_str(&text).expect("round-trip");
    assert_eq!(cfg, parsed);

    // Как событие: wire-тип из §8
    assert_eq!(SessionConfig::TYPE, "evt/session.start");
}

// ─── §6 + ТЗ §4.8.2: снапшот состояния ──────────────────────────────────────

const SNAPSHOT_JSON: &str = r#"{
  "state_version": 1337,
  "char": {
    "id": "c1", "name": "Ким", "gender": "male",
    "attrs": { "str": 14, "int": 9 },
    "skills": [ { "id": "sword", "group": "weapon",
                  "rank": 3, "xp": 612, "xp_to_next": 480 } ],
    "professions": [ { "id": "scribe", "rank": 2, "xp": 140, "xp_to_next": 200 } ],
    "hp": { "cur": 42, "max": 60 },
    "mp": { "cur": 45, "max": 60 },
    "xp_pool": 120,
    "level": 7,
    "total_xp": 5734,
    "xp_in_level": 134,
    "xp_to_next": 2800,
    "derived": { "accuracy": 80, "damage_min": 11, "damage_max": 11,
                 "crit_chance": 8, "crit_damage": 52, "armor": 0,
                 "dodge": 7, "parry_min": 5, "parry_max": 5,
                 "parry_chance": 6, "block_min": 0, "block_max": 0,
                 "block_chance": 0, "magic_defense": 10, "magic_dodge": 3,
                 "magic_resist": 5, "magic_dispel": 5,
                 "magic_crit_damage": 52, "magic_crit_chance": 8 },
    "profile": "mage",
    "equip": { "main_hand": "i_17", "off_hand": null },
    "inventory": [ { "uid": "i_17", "type": "staff", "name": "Ясеневый посох",
                     "rarity": "rare", "upgrade": 2, "stats": {},
                     "sockets": [ { "index": 0, "rune": { "name": "Руна огня",
                       "spell": "fire_dart", "charges": 7, "max": 10, "damage": 12 } },
                                  { "index": 1, "rune": null } ] } ],
    "crime": { "statuses": [ { "kind": "murderer", "severity": "high",
                               "expires_at": 1762150000 } ] },
    "quests": [ { "id": "q_boars", "state": "active",
                  "goals": [ { "text": "Кабанов убито", "cur": 2, "need": 5 } ] } ]
  },
  "location": { "id": "l_forest", "name": "Черный лес", "flags": ["pvp"],
                "text": "Сосны давят тьмою…",
                "exits": [ { "id": "e_s", "label": "тропа на юг", "dir": "south" } ],
                "entities": [
                  { "id": "p_77", "kind": "player", "name": "Марк",
                    "hp": { "cur": 30, "max": 55 }, "tags": ["marauder"] },
                  { "id": "m_3", "kind": "monster", "name": "Волк",
                    "hp": { "cur": 18, "max": 18 }, "aggro": false },
                  { "id": "cor_9", "kind": "corpse", "name": "Труп: Ким-младший",
                    "was_player": true } ] },
  "party": null,
  "guild": null,
  "server_ts": 1762140311
}"#;

#[test]
fn snapshot_matches_doc_example() {
    let snap: StateSnapshot = serde_json::from_str(SNAPSHOT_JSON).expect("снапшот парсится");
    assert_eq!(snap.state_version, 1337);
    assert_eq!(snap.char.hp.cur, 42);
    assert_eq!(
        snap.char.gender, "male",
        "M18/F34: пол в снапшоте (без поля старого сервера → male)"
    );
    assert_eq!(
        snap.char.mp,
        Mp { cur: 45, max: 60 },
        "D4: mp — объект, не null"
    );
    assert_eq!(snap.char.xp_pool, 120, "пул опыта квестов");
    assert_eq!(snap.char.level, Some(7), "M15/F24: уровень");
    assert_eq!(snap.char.total_xp, Some(5734), "M15/F24: общий опыт");
    assert_eq!(snap.char.xp_in_level, Some(134), "M15/F24: опыт в уровне");
    assert_eq!(snap.char.xp_to_next, Some(2800), "M15/F24: до уровня");
    let der = snap.char.derived.as_ref().expect("M15/F26: derived");
    assert_eq!(der.accuracy, 80, "M15/F26: точность стартового");
    assert_eq!((der.damage_min, der.damage_max), (11, 11), "урон 6+10/2");
    assert_eq!(der.crit_chance, 8, "крит-шанс стартового");
    assert_eq!(
        snap.char.skills[0].description, None,
        "M14/F17: сервер без description → None (обратная совместимость)"
    );
    assert_eq!(
        snap.char.skills[0].group.as_deref(),
        Some("weapon"),
        "M15/F28: группа навыка из контента"
    );
    assert_eq!(
        snap.char.profile.as_deref(),
        Some("mage"),
        "M17/F30b: класс"
    );
    assert_eq!(
        snap.char.equip.get("off_hand"),
        Some(&None),
        "пустой слот = null"
    );
    assert_eq!(snap.char.inventory[0].item_type, "staff");
    assert_eq!(snap.char.inventory[0].upgrade, 2);
    let staff = &snap.char.inventory[0];
    let sockets = staff.sockets.as_ref().expect("сокеты посоха");
    assert_eq!(sockets.len(), 2);
    assert_eq!(sockets[0].rune.as_ref().expect("руна").charges, 7);
    assert_eq!(sockets[1].rune, None, "свободный сокет ◈◈○");
    assert_eq!(snap.char.crime.statuses[0].expires_at, 1762150000);
    assert_eq!(snap.char.quests[0].goals[0].need, Some(5));
    assert_eq!(snap.location.entities.len(), 3);
    assert_eq!(
        snap.location.entities[2].was_player,
        Some(true),
        "труп игрока"
    );
    assert!(snap.party.is_none() && snap.guild.is_none());

    // Round-trip
    let text = serde_json::to_string(&snap).expect("сериализация");
    let parsed: StateSnapshot = serde_json::from_str(&text).expect("round-trip");
    assert_eq!(snap, parsed);
    assert_eq!(StateSnapshot::TYPE, "evt/state/snapshot");
}

// ─── §7: команды через конверт ───────────────────────────────────────────────

#[test]
fn command_envelope_round_trip_per_domain() {
    // Представители всех доменов §7: конверт → payload → тип → равенство.
    let commands: Vec<(String, Envelope)> = vec![
        (
            "auth".into(),
            Envelope::command(
                1,
                TS,
                &AuthLogin {
                    login: "demo".into(),
                    password: "demo123".into(),
                },
            )
            .unwrap(),
        ),
        (
            "char".into(),
            Envelope::command(
                2,
                TS,
                &CharCreate {
                    name: "Ким".into(),
                    profile: "warrior".into(),
                    gender: None,
                },
            )
            .unwrap(),
        ),
        (
            "location".into(),
            Envelope::command(
                3,
                TS,
                &LocationMove {
                    exit_id: "e_north".into(),
                },
            )
            .unwrap(),
        ),
        (
            "chat".into(),
            Envelope::command(
                4,
                TS,
                &ChatSend {
                    channel: "location".into(),
                    text: "привет".into(),
                    to: None,
                },
            )
            .unwrap(),
        ),
        (
            "magic".into(),
            Envelope::command(
                5,
                TS,
                &MagicRuneCast {
                    staff_uid: "i_17".into(),
                    socket_index: 0,
                    target_id: Some("m_3".into()),
                },
            )
            .unwrap(),
        ),
        (
            "trade".into(),
            Envelope::command(
                6,
                TS,
                &TradeAddItem {
                    offer_id: "o1".into(),
                    item_uid: "i_9".into(),
                    qty: Some(2),
                },
            )
            .unwrap(),
        ),
        (
            "guild".into(),
            Envelope::command(
                7,
                TS,
                &GuildRankSet {
                    player_id: "p_77".into(),
                    rank: "officer".into(),
                },
            )
            .unwrap(),
        ),
        (
            "state".into(),
            Envelope::command(
                8,
                TS,
                &StateResync {
                    last_state_version: 1345,
                },
            )
            .unwrap(),
        ),
        (
            "invest".into(),
            Envelope::command(
                9,
                TS,
                &SkillTrain {
                    entity_id: "npc_elder".into(),
                    skill_id: "sword".into(),
                },
            )
            .unwrap(),
        ),
        ("ping".into(), Envelope::command(10, TS, &Ping {}).unwrap()),
    ];

    for (domain, env) in &commands {
        assert_eq!(env.v, 1, "{domain}");
        assert!(
            env.msg_type.starts_with("cmd/"),
            "{domain}: {}",
            env.msg_type
        );
        let json = env.to_json_limited().expect("лимит 64 KiB");
        let parsed: Envelope = serde_json::from_str(&json).expect("конверт парсится");
        assert_eq!(env, &parsed, "{domain}");
        assert!(parsed.payload.is_some(), "{domain}: payload обязателен");
    }

    // Точная сверка payload'а ключевых команд
    let env = Envelope::command(
        1,
        TS,
        &AuthLogin {
            login: "demo".into(),
            password: "demo123".into(),
        },
    )
    .unwrap();
    let payload: AuthLogin = env.payload_as().expect("payload");
    assert_eq!(payload.login, "demo");
    assert_eq!(AuthLogin::TYPE, "cmd/auth.login");
    // §7: ретраи только идемпотентных (подробно — в cmd::tests).
    const { assert!(AuthLogin::IDEMPOTENT) };
    const { assert!(!LocationMove::IDEMPOTENT) };
    const { assert!(!ChatSend::IDEMPOTENT) };
}

// ─── §8: события ─────────────────────────────────────────────────────────────

#[test]
fn event_payloads_round_trip() {
    // Бой
    let hit = CombatHit {
        source: "c1".into(),
        source_name: "Ким".into(),
        target: "m_3".into(),
        target_name: "Волк".into(),
        kind: ld2_protocol::evt::CombatKind::Melee,
        amount: 7,
        crit: false,
        miss: false,
        text: "Вы бьёте Волк руками **7**".into(),
        extra: Default::default(),
    };
    let v = serde_json::to_value(&hit).unwrap();
    let back: CombatHit = serde_json::from_value(v).unwrap();
    assert_eq!(hit, back);
    assert_eq!(CombatHit::TYPE, "evt/combat/hit");

    // Магия: D4 — заряды И мана
    let cast = MagicCast {
        by: "c1".into(),
        by_name: "Ким".into(),
        spell: "fire_dart".into(),
        what: ld2_protocol::evt::CastSource::Rune,
        target: Some("m_3".into()),
        charges_left: Some(6),
        mp_left: Some(45),
        ok: true,
        extra: Default::default(),
    };
    let back: MagicCast = serde_json::from_value(serde_json::to_value(&cast).unwrap()).unwrap();
    assert_eq!(cast, back);
    assert_eq!(cast.charges_left, Some(6));
    assert_eq!(cast.mp_left, Some(45));

    // Дельта состояния (§3)
    let delta = StateDelta {
        from_version: 1337,
        to_version: 1340,
        patch: vec![
            ld2_protocol::evt::PatchOp {
                op: ld2_protocol::evt::PatchOpKind::Replace,
                path: "/char/hp/cur".into(),
                value: Some(serde_json::json!(31)),
            },
            ld2_protocol::evt::PatchOp {
                op: ld2_protocol::evt::PatchOpKind::Remove,
                path: "/location/entities/m_3".into(),
                value: None,
            },
        ],
    };
    let back: StateDelta = serde_json::from_value(serde_json::to_value(&delta).unwrap()).unwrap();
    assert_eq!(delta, back);
    assert_eq!(back.patch[1].value, None, "remove без value");

    // Преступность (ФР-11): полная замена статусов
    let crime = CrimeStatusUpdate {
        statuses: vec![
            ld2_protocol::types::CrimeStatus {
                kind: "murderer".into(),
                severity: Severity::High,
                expires_at: 1762150000,
            },
            ld2_protocol::types::CrimeStatus {
                kind: "thief".into(),
                severity: Severity::Medium,
                expires_at: 1762147511,
            },
        ],
        log_hint: Some("За мародёрство: +2ч 00м".into()),
    };
    let back: CrimeStatusUpdate =
        serde_json::from_value(serde_json::to_value(&crime).unwrap()).unwrap();
    assert_eq!(crime, back);
    assert_eq!(CrimeStatusUpdate::TYPE, "evt/crime/status");

    // Чат (ФР-13)
    let msg = ChatMessage {
        id: Some(42),
        channel: "location".into(),
        from: "p_77".into(),
        name: "Марк".into(),
        text: "кто в пати?".into(),
        tags: Some(vec!["marauder".into()]),
        sent_at: TS.into(),
        extra: Default::default(),
    };
    let back: ChatMessage = serde_json::from_value(serde_json::to_value(&msg).unwrap()).unwrap();
    assert_eq!(msg, back);

    // Сделка (ФР-15): версионирование
    let trade = TradeState {
        offer_id: "o1".into(),
        version: 3,
        you: vec![],
        them: vec![],
        you_gold: 100,
        them_gold: 0,
        you_locked: true,
        them_locked: false,
        value_hint: Some(250),
        extra: Default::default(),
    };
    let back: TradeState = serde_json::from_value(serde_json::to_value(&trade).unwrap()).unwrap();
    assert_eq!(trade, back);
    assert_eq!(back.version, 3);

    // Пинг
    let pong = Pong {
        ts: TS.into(),
        rtt_ms: 42,
    };
    let back: Pong = serde_json::from_value(serde_json::to_value(&pong).unwrap()).unwrap();
    assert_eq!(pong, back);
    assert_eq!(Pong::TYPE, "evt/pong");
}

// ─── §2.2: ack ───────────────────────────────────────────────────────────────

#[test]
fn ack_ok_and_error_round_trip() {
    let ok = Ack::ok(42, Some(1337));
    let json = serde_json::to_string(&ok).unwrap();
    assert!(json.contains("\"type\":\"ack\""));
    let back: Ack = serde_json::from_str(&json).unwrap();
    assert_eq!(ok, back);
    assert!(back.is_ok());
    assert_eq!(back.error_code(), None);
    assert_eq!(back.seq, 42);

    let err = Ack::error(43, Some(1338), &ErrorCode::NotEnoughMp, "Не хватает маны");
    let back: Ack = serde_json::from_str(&serde_json::to_string(&err).unwrap()).unwrap();
    assert!(!back.is_ok());
    assert_eq!(back.error_code(), Some(ErrorCode::NotEnoughMp));
    assert_eq!(back.error.as_ref().unwrap().message, "Не хватает маны");

    // Неизвестный код сервера не ломает клиента (§11)
    let future = r#"{"v":1,"type":"ack","seq":1,"result":"error",
        "error":{"code":"E_FUTURE","message":"новый код"}}"#;
    let parsed: Ack = serde_json::from_str(future).unwrap();
    assert_eq!(
        parsed.error_code(),
        Some(ErrorCode::Unknown("E_FUTURE".into()))
    );
}

// ─── предмет (§6): открытые поля и сокеты ────────────────────────────────────

#[test]
fn item_snapshot_open_fields_survive() {
    let json = r#"{
      "uid": "i_44", "type": "weapon", "name": "Кинжал", "rarity": "mythic",
      "upgrade": 5, "stats": { "dmg": "3..5" },
      "requires": { "attr": { "dex": 14 }, "profession": null, "level": null },
      "qty": 1, "gold_value": 120, "brand_new_field": "сервер добавил"
    }"#;
    let item: ItemSnapshot = serde_json::from_str(json).expect("незнакомые поля не ломают");
    assert_eq!(item.rarity, "mythic", "открытая строка rarity");
    assert_eq!(item.requires.as_ref().unwrap().attr["dex"], 14);
    assert_eq!(
        item.extra.get("brand_new_field"),
        Some(&serde_json::json!("сервер добавил")),
        "flatten extra сохраняет новое поле"
    );
    // Round-trip сохраняет extra
    let back: ItemSnapshot = serde_json::from_value(serde_json::to_value(&item).unwrap()).unwrap();
    assert_eq!(item, back);
}

/// M18/F34: снапшот старого сервера без `gender` парсится → `male` (§11);
/// поле в JSON присутствует при сериализации.
#[test]
fn gender_defaults_when_absent() {
    let json = r#"{ "id": "c1", "name": "Ким",
        "hp": { "cur": 100, "max": 100 }, "mp": { "cur": 60, "max": 60 } }"#;
    let state: ld2_protocol::types::CharacterState =
        serde_json::from_str(json).expect("снапшот без gender парсится");
    assert_eq!(state.gender, "male", "фолбэк старого сервера");
    let encoded = serde_json::to_value(&state).expect("сериализация");
    assert_eq!(encoded["gender"], "male", "поле в JSON");
}
