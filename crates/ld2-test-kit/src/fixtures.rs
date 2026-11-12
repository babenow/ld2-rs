//! Фикстуры контракта — эталонные тестовые данные из канонических примеров:
//! `docs/protocol.md` §5 (session.start, дословно), §6 (типы),
//! `docs/tz.md` §4.8.2 (снапшот), `docs/skills.md` §9 (рецепты/профессии).

use std::collections::BTreeMap;

use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::features::{
    ChatConfig, CrimeConfig, CrimeKind, InventoryConfig, MagicConfig, ManaConfig, RefreshToken,
    ServerInfo, SessionConfig, SessionTokens, UpgradeConfig,
};
use ld2_protocol::types::{
    CharacterState, CharacterSummary, CrimeLogEntry, CrimeState, CrimeStatus, Entity, Exit,
    GuildState, Hp, ItemSnapshot, LocationSnapshot, Mp, ProfessionSnapshot, QuestGoal, QuestState,
    Severity, SkillSnapshot, Socket,
};
use serde_json::{json, Value};

pub const LOGIN: &str = "demo";
pub const PASSWORD: &str = "demo123";
pub const REFRESH_TOKEN: &str = "rt_mock_refresh";
/// Второй фикстурный аккаунт (DoD M8: «2 клиента») — свой персонаж `c2`.
pub const LOGIN2: &str = "demo2";
/// Пароль тот же — тестам удобнее, логин разный.
pub const PASSWORD2: &str = "demo123";

/// Стоимость каста руны в мане (D4: руна тратит ману + заряд).
pub const RUNE_MP_COST: i64 = 10;
/// Лимит первичных профессий (D10, §5).
pub const PROFESSION_LIMIT: u32 = 2;
/// Лимит персонажей по умолчанию; переопределяется CLI (`--char-slots`) и,
/// в реальном сервере, полем конфига (D6, DoD M2 — «без правки кода»).
pub const CHAR_SLOT_LIMIT: u32 = 8;
pub const UPGRADE_MAX: u32 = 9;

/// Профили персонажей (D3, ADR-003).
pub const PROFILES: &[&str] = &["warrior", "archer", "mage"];

/// `evt/session.start` — дословно по protocol.md §5 (лимит слотов — параметр).
pub fn session_config(char_slot_limit: u32) -> SessionConfig {
    session_config_for(LOGIN, char_slot_limit)
}

/// Конфиг сессии для конкретного аккаунта: список персонажей и refresh-токен
/// свои (DoD M8 — второй клиент со своим персонажем `c2`).
pub fn session_config_for(login: &str, char_slot_limit: u32) -> SessionConfig {
    let mut cfg = session_config_impl(char_slot_limit);
    cfg.characters = chars_for(login);
    cfg.session.refresh.token = refresh_token_for(login);
    cfg
}

/// Refresh-токен фикстурного аккаунта: `demo` — свой, остальные — `rt_{login}`.
fn refresh_token_for(login: &str) -> String {
    if login == LOGIN {
        REFRESH_TOKEN.to_string()
    } else {
        format!("rt_{login}")
    }
}

/// Персонажи аккаунта (список слотов §5 `characters[]`).
pub fn chars_for(login: &str) -> Vec<CharacterSummary> {
    if login == LOGIN2 {
        vec![character_summary2()]
    } else {
        vec![character_summary()]
    }
}

/// Проверка учётных данных фикстурных аккаунтов.
pub fn account(login: &str, password: &str) -> bool {
    (login == LOGIN && password == PASSWORD) || (login == LOGIN2 && password == PASSWORD2)
}

/// Есть ли логин среди фикстур (зарегистрированные учитываются отдельно).
pub fn account_exists(login: &str) -> bool {
    login == LOGIN || login == LOGIN2
}

/// Лимиты `cmd/auth.register` в тестах — дефолты шаблона `server.example.toml`
/// (`[auth]`); у реального сервера — настройки конфига (ADR-006, D25).
pub const MIN_LOGIN_LEN: usize = 3;
pub const MIN_PASSWORD_LEN: usize = 8;

fn session_config_impl(char_slot_limit: u32) -> SessionConfig {
    let mut features = serde_json::Map::new();
    features.insert("loot.roll".into(), json!(false));
    features.insert("respawn.inn".into(), json!(true));
    features.insert("guild.bank".into(), json!(false));
    features.insert("upgrade.rules".into(), json!("break_on_fail"));
    features.insert("trade.value_hint".into(), json!(true));
    features.insert("chat.slash".into(), json!(true));

    let mut base = BTreeMap::new();
    base.insert("murderer_s".to_string(), 21600);
    base.insert("marauder_s".to_string(), 7200);

    SessionConfig {
        server: ServerInfo {
            name: "ld2-dev".into(),
            region: Some("ru".into()),
            min_client: None,
        },
        session: SessionTokens {
            access_token: "at_mock".into(),
            expires_in_s: 900,
            refresh: RefreshToken {
                token: REFRESH_TOKEN.into(),
                expires_in_s: 2_592_000,
            },
        },
        char_slot_limit,
        profession_limit: PROFESSION_LIMIT,
        role: "player".into(),
        features,
        crime: CrimeConfig {
            t_max_s: 604_800,
            blend_k: 0.5,
            base,
            kinds: vec![
                CrimeKind {
                    kind: "murderer".into(),
                    title: "Убийца".into(),
                    severity: Severity::High,
                    extra: Default::default(),
                },
                CrimeKind {
                    kind: "marauder".into(),
                    title: "Мародёр".into(),
                    severity: Severity::Medium,
                    extra: Default::default(),
                },
                CrimeKind {
                    kind: "thief".into(),
                    title: "Вор".into(),
                    severity: Severity::Medium,
                    extra: Default::default(),
                },
            ],
        },
        chat: ChatConfig {
            server_history: 200,
            channels: vec![
                "system".into(),
                "location".into(),
                "global".into(),
                "party".into(),
                "guild".into(),
                "whisper".into(),
            ],
            max_len: 500,
            cooldown_s: 1.5,
        },
        inventory: InventoryConfig {
            slots: 40,
            equip_slots: vec![
                "head".into(),
                "shoulders".into(),
                "bracers".into(),
                "gloves".into(),
                "body".into(),
                "legs".into(),
                "feet".into(),
                "cloak".into(),
                "ring1".into(),
                "ring2".into(),
                "amulet".into(),
                "main_hand".into(),
                "off_hand".into(),
                "artifact1".into(),
                "artifact2".into(),
            ],
        },
        upgrade: UpgradeConfig {
            max_level: UPGRADE_MAX,
            preview: true,
            shows_chance: true,
        },
        magic: MagicConfig {
            rune_recharge: "item".into(),
            scroll_requires_target: true,
            mana: ManaConfig {
                regen_out_s: 45,
                regen_in_pct_s: 1.0,
                rune_cost: 10,
            },
        },
        characters: vec![character_summary()],
        extra: Default::default(),
    }
}

pub fn character_summary() -> CharacterSummary {
    CharacterSummary {
        id: "c1".into(),
        name: "Ким".into(),
        profile: Some("warrior".into()),
        location: "Черный лес".into(),
        summary: Some("Навыки: меч 5, аркана 1".into()),
        crime: vec!["murderer".into()],
        online: true,
        extra: Default::default(),
    }
}

/// Персонаж второго аккаунта (DoD M8: второй клиент, адресат whisper/сделки).
pub fn character_summary2() -> CharacterSummary {
    CharacterSummary {
        id: "c2".into(),
        name: "Астра".into(),
        profile: Some("mage".into()),
        location: "Черный лес".into(),
        summary: Some("Навыки: лук 3".into()),
        crime: vec![],
        online: true,
        extra: Default::default(),
    }
}

// ─── Локации ────────────────────────────────────────────────────────────────

pub fn village() -> LocationSnapshot {
    LocationSnapshot {
        id: "l_village".into(),
        name: "Деревня Изба".into(),
        flags: vec!["safe".into()],
        text: "Тесная деревня на опушке. Дым из труб, крики петухов. \
               На север уходит тропа в лес."
            .into(),
        exits: vec![Exit {
            id: "e_north".into(),
            label: "тропа на север".into(),
            dir: Some("north".into()),
        }],
        entities: vec![Entity {
            id: "npc_elder".into(),
            kind: ld2_protocol::types::EntityKind::Npc,
            name: "Староста".into(),
            hp: None,
            tags: None,
            was_player: None,
            aggro: None,
            extra: Default::default(),
        }],
        extra: Default::default(),
    }
}

pub fn forest() -> LocationSnapshot {
    LocationSnapshot {
        id: "l_forest".into(),
        name: "Черный лес".into(),
        flags: vec!["pvp".into()],
        text: "Сосны давят тьмою… Тропа вьётся на юг, к деревне.".into(),
        exits: vec![Exit {
            id: "e_south".into(),
            label: "тропа на юг".into(),
            dir: Some("south".into()),
        }],
        entities: vec![
            Entity {
                id: "m_3".into(),
                kind: ld2_protocol::types::EntityKind::Monster,
                name: "Волк".into(),
                hp: Some(Hp { cur: 18, max: 18 }),
                tags: Some(vec!["aggro".into()]),
                was_player: None,
                aggro: Some(true),
                extra: Default::default(),
            },
            Entity {
                id: "p_77".into(),
                kind: ld2_protocol::types::EntityKind::Player,
                name: "Марк".into(),
                hp: Some(Hp { cur: 30, max: 55 }),
                tags: Some(vec!["marauder".into()]),
                was_player: None,
                aggro: None,
                extra: Default::default(),
            },
            Entity {
                id: "cor_9".into(),
                kind: ld2_protocol::types::EntityKind::Corpse,
                name: "Труп: Ким-младший".into(),
                hp: None,
                tags: None,
                was_player: Some(true),
                aggro: None,
                extra: Default::default(),
            },
        ],
        extra: Default::default(),
    }
}

/// Куда ведёт выход из текущей локации (для `cmd/location.move`).
pub fn exit_target(location_id: &str, exit_id: &str) -> Option<LocationSnapshot> {
    match (location_id, exit_id) {
        ("l_village", "e_north") => Some(forest()),
        ("l_forest", "e_south") => Some(village()),
        _ => None,
    }
}

// ─── Персонаж и снапшот ─────────────────────────────────────────────────────

pub fn staff_item() -> ItemSnapshot {
    let mut stats = serde_json::Map::new();
    stats.insert("spell".into(), json!("fire_dart"));
    stats.insert("dmg".into(), json!("3..5"));
    ItemSnapshot {
        uid: "i_17".into(),
        item_type: "staff".into(),
        name: "Ясеневый посох".into(),
        rarity: "rare".into(),
        upgrade: 2,
        stats,
        requires: None,
        slot: Some("main_hand".into()),
        hands: Some(2),
        sockets: Some(vec![
            Socket {
                index: 0,
                rune: Some(ld2_protocol::types::RuneInfo {
                    name: "Руна огня".into(),
                    spell: "fire_dart".into(),
                    charges: 7,
                    max: 10,
                }),
            },
            Socket {
                index: 1,
                rune: None,
            },
        ]),
        qty: None,
        gold_value: Some(120),
        extra: Default::default(),
    }
}

pub fn scroll_item() -> ItemSnapshot {
    let mut stats = serde_json::Map::new();
    stats.insert("spell".into(), json!("fireball"));
    ItemSnapshot {
        uid: "s_1".into(),
        item_type: "scroll".into(),
        name: "Свиток огненного шара".into(),
        rarity: "common".into(),
        upgrade: 0,
        stats,
        requires: None,
        slot: None,
        hands: None,
        sockets: None,
        qty: Some(1),
        gold_value: Some(30),
        extra: Default::default(),
    }
}

/// Руна-предмет (M5: инкрустация/тесты) — `uid` параметризован.
pub fn rune_item(uid: &str) -> ItemSnapshot {
    let mut stats = serde_json::Map::new();
    stats.insert("spell".into(), json!("ice_shard"));
    stats.insert("charges".into(), json!(5));
    stats.insert("max".into(), json!(5));
    ItemSnapshot {
        uid: uid.to_string(),
        item_type: "rune".into(),
        name: "Руна льда".into(),
        rarity: "common".into(),
        upgrade: 0,
        stats,
        requires: None,
        slot: None,
        hands: None,
        sockets: None,
        qty: Some(1),
        gold_value: Some(20),
        extra: Default::default(),
    }
}

/// Ингредиенты для рецепта `rec_sword_iron` (skills.md §5.4).
pub fn ingredients() -> Vec<ItemSnapshot> {
    ["ingot_iron", "wood_oak"]
        .iter()
        .enumerate()
        .map(|(i, ty)| ItemSnapshot {
            uid: format!("m_{}", i + 1),
            item_type: (*ty).into(),
            name: if *ty == "ingot_iron" {
                "Железный слиток"
            } else {
                "Дубовая доска"
            }
            .into(),
            rarity: "common".into(),
            upgrade: 0,
            stats: Default::default(),
            requires: None,
            slot: None,
            hands: None,
            sockets: None,
            qty: Some(3),
            gold_value: Some(5),
            extra: Default::default(),
        })
        .collect()
}

pub fn character(id: &str, name: &str) -> CharacterState {
    CharacterState {
        id: id.into(),
        name: name.into(),
        profile: Some("warrior".into()),
        attrs: BTreeMap::from([("str".to_string(), 14i64), ("int".to_string(), 9i64)]),
        skills: vec![
            SkillSnapshot {
                id: "sword".into(),
                name: Some("Владение мечом".into()),
                description: Some("+2% физ. урона мечом".into()),
                // rank 5 — открывает активку «Мощный удар» (skills.md §4);
                // порог формулы §2.2: round(120 · 1.32^5) = 1471.
                rank: 5,
                group: Some("weapon".into()),
                xp: 612,
                xp_to_next: 1471,
            },
            SkillSnapshot {
                id: "arcana".into(),
                name: Some("Аркана".into()),
                description: None,
                rank: 1,
                group: Some("magic".into()),
                xp: 40,
                xp_to_next: 480,
            },
        ],
        professions: vec![ProfessionSnapshot {
            id: "scribe".into(),
            rank: 2,
            xp: 140,
            xp_to_next: 200,
            extra: Default::default(),
        }],
        hp: Hp { cur: 42, max: 60 },
        mp: Mp { cur: 45, max: 60 },
        xp_pool: 120,
        // M15/F24: уровень и шкала опыта (образец: 1 уровень, порог 100).
        level: Some(1),
        total_xp: Some(60),
        xp_in_level: Some(60),
        xp_to_next: Some(100),
        // M15/F26: сервер без вкладки «Параметры» → None (заглушка).
        derived: None,
        equip: BTreeMap::from([("main_hand".to_string(), Some("i_17".to_string()))]),
        inventory: vec![
            staff_item(),
            scroll_item(),
            ingredients()[0].clone(),
            ingredients()[1].clone(),
        ],
        crime: CrimeState {
            statuses: vec![CrimeStatus {
                kind: "murderer".into(),
                severity: Severity::High,
                // Динамический срок (M6): фикстура 2025 уже истекла бы —
                // живой отсчёт = now + murderer_s (конфиг §5).
                expires_at: now_epoch() + 21_600,
            }],
        },
        quests: vec![quest_wolves()],
        extra: Default::default(),
    }
}

/// Квест «Волки» (M7): цель 4/5 — одно убийство доводит до «к сдаче».
/// (Golden `ld2-protocol` использует собственный JSON и от фикстур не зависит.)
pub fn quest_wolves() -> QuestState {
    QuestState {
        id: "q_wolves".into(),
        title: Some("Волки".into()),
        state: ld2_protocol::types::QuestPhase::Active,
        goals: vec![QuestGoal {
            text: "Волков убито".into(),
            cur: Some(4),
            need: Some(5),
        }],
        reward_preview: Some(ld2_protocol::types::RewardPreview {
            xp: Some(450),
            items: vec![],
        }),
        extra: Default::default(),
    }
}

pub fn snapshot(
    state_version: u64,
    char: &CharacterState,
    location: &LocationSnapshot,
) -> StateSnapshot {
    StateSnapshot {
        state_version,
        char: char.clone(),
        location: location.clone(),
        party: None,
        guild: None,
        server_ts: now_epoch(),
        extra: Default::default(),
    }
}

// ─── Рецепты (skills.md §5.4) ───────────────────────────────────────────────

pub fn recipes() -> Vec<Value> {
    vec![
        json!({
            "id": "rec_sword_iron", "name": "Железный меч",
            "profession": "blacksmith", "rank": 3,
            "station": "forge", "station_label": "Кузница (город)",
            "tool": { "item_type": "hammer" },
            "requires": { "attr": { "str": 10 } },
            "inputs": [ { "type": "ingot_iron", "qty": 3 }, { "type": "wood_oak", "qty": 1 } ],
            "time_s": 20, "xp": { "profession": 85 },
            "output": { "type": "sword_iron", "qty": 1, "quality_by": "rank" }
        }),
        json!({
            "id": "rec_potion_hp", "name": "Зелье лечения",
            "profession": "alchemist", "rank": 1,
            "station": "alchemy_table", "station_label": "Алхимический стол",
            "inputs": [ { "type": "herb", "qty": 2 } ],
            "time_s": 8, "xp": { "profession": 30 },
            "output": { "type": "potion_hp", "qty": 1 }
        }),
    ]
}

// ─── Прочее ─────────────────────────────────────────────────────────────────

pub fn crime_log() -> Vec<CrimeLogEntry> {
    vec![
        CrimeLogEntry {
            kind: "murderer".into(),
            at: 1_762_140_311,
            delta_s: 21_600,
            total_s: 21_600,
        },
        CrimeLogEntry {
            kind: "marauder".into(),
            at: 1_762_143_911,
            delta_s: 3_141,
            total_s: 24_741,
        },
    ]
}

pub fn guild_fixture() -> GuildState {
    GuildState {
        id: "g_1".into(),
        name: "Стальные волки".into(),
        members: vec![ld2_protocol::types::GuildMember {
            id: "c1".into(),
            name: "Ким".into(),
            rank: "master".into(),
            online: true,
            extra: Default::default(),
        }],
        extra: Default::default(),
    }
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
