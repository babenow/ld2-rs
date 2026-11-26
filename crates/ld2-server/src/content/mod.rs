//! Контент-сид (D26, M12.3/M12.4): статический мир из `world.content_dir`
//! (JSON, версионируется в git) → PostgreSQL **идемпотентно**.
//!
//! Файлы каталога: `locations.json`, `skills.json`, `items.json` — все
//! обязательны. Сверка `meta.content_version` = sha256 по каноническому
//! дереву **всего** контента: версия не изменилась → БД не трогается
//! вообще (ручные правки переживают перезапуск — критерий M12.3);
//! изменилась → upsert локаций/навыков/предметов и обновление версии.
//! Удалённые из контента сущности из БД **не** дропаются (на них могут
//! ссылаться персонажи).

use std::path::{Path, PathBuf};

use sqlx::PgPool;

mod seed;

pub use seed::{fingerprint, load, reload, seed, validate};

// E1 (M27): схема контента — общая с редактором (`ld2-content`).
pub use ld2_content::{
    ContentExit, ContentGoal, ContentInput, ContentItemDef, ContentLocation, ContentOutput,
    ContentProfession, ContentQuest, ContentRecipe, ContentRecipeXp, ContentRequires, ContentSkill,
    ContentTool, GameContent, CONTENT_FILES,
};

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("контент: не удалось прочитать {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("контент: некорректный JSON {0}: {1}")]
    Json(PathBuf, serde_json::Error),
    #[error("контент: {0}")]
    Db(#[from] sqlx::Error),
    /// E1: ошибка общей схемы `ld2-content` (чтение/валидация/версия).
    #[error("контент: {0}")]
    Load(#[from] ld2_content::ContentError),
    #[error("контент: в {0} нет ни одной локации")]
    Empty(PathBuf),
    /// M27 (hot reload): структурная валидация перед сидом — одна строка
    /// со списком проблем (дубликаты id, висячие выходы), чтобы битый
    /// контент не залился в БД работающего сервера.
    #[error("контент не прошёл валидацию: {0}")]
    Invalid(String),
}

/// Результат сида: что показать в логе старта.
#[derive(Debug)]
pub struct SeedReport {
    /// sha256-версия содержимого каталога.
    pub version: String,
    /// `false` — версия не изменилась, БД не трогалась.
    pub applied: bool,
}

/// Применить **уже загруженный** контент (идемпотентно): версия совпала
/// с `meta.content_version` → БД не трогается вовсе (M12.3).
async fn seed_content(
    pool: &PgPool,
    content: &GameContent,
    version: String,
) -> Result<SeedReport, ContentError> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'content_version'")
            .fetch_optional(pool)
            .await?;
    if current.as_deref() == Some(version.as_str()) {
        return Ok(SeedReport {
            version,
            applied: false,
        });
    }

    let spawn = content
        .locations
        .iter()
        .find(|l| l.spawn)
        .map(|l| l.id.clone());

    let mut tx = pool.begin().await?;
    for loc in &content.locations {
        let exits = serde_json::to_value(&loc.exits).unwrap_or_default();
        let entities = serde_json::to_value(&loc.entities).unwrap_or_default();
        sqlx::query(
            "INSERT INTO locations (id, name, flags, text, exits, entities) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO UPDATE SET \
               name = EXCLUDED.name, flags = EXCLUDED.flags, text = EXCLUDED.text, \
               exits = EXCLUDED.exits, entities = EXCLUDED.entities",
        )
        .bind(&loc.id)
        .bind(&loc.name)
        .bind(&loc.flags)
        .bind(&loc.text)
        .bind(exits)
        .bind(entities)
        .execute(&mut *tx)
        .await?;
    }
    for skill in &content.skills {
        sqlx::query(
            "INSERT INTO skills (id, name, skill_group, description) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
               skill_group = EXCLUDED.skill_group, description = EXCLUDED.description",
        )
        .bind(&skill.id)
        .bind(&skill.name)
        .bind(&skill.group)
        .bind(&skill.description)
        .execute(&mut *tx)
        .await?;
    }
    for item in &content.items {
        let stats = serde_json::Value::Object(item.stats.clone());
        let requires = serde_json::to_value(&item.requires).unwrap_or(serde_json::Value::Null);
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty, skill, sockets, rune_sockets, slot, hands, cooldown_s, bonuses, bag_slots, bag_types, bag_kind) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             ON CONFLICT (id) DO UPDATE SET \
               item_type = EXCLUDED.item_type, name = EXCLUDED.name, rarity = EXCLUDED.rarity, \
               upgrade = EXCLUDED.upgrade, stats = EXCLUDED.stats, requires = EXCLUDED.requires, \
               gold_value = EXCLUDED.gold_value, qty = EXCLUDED.qty, skill = EXCLUDED.skill, \
               sockets = EXCLUDED.sockets, rune_sockets = EXCLUDED.rune_sockets, \
               slot = EXCLUDED.slot, hands = EXCLUDED.hands, \
               cooldown_s = EXCLUDED.cooldown_s, bonuses = EXCLUDED.bonuses, \
               bag_slots = EXCLUDED.bag_slots, bag_types = EXCLUDED.bag_types, \
               bag_kind = EXCLUDED.bag_kind",
        )
        .bind(&item.id)
        .bind(&item.item_type)
        .bind(&item.name)
        .bind(&item.rarity)
        .bind(item.upgrade)
        .bind(stats)
        .bind(requires)
        .bind(item.gold_value)
        .bind(item.qty.unwrap_or(1))
        .bind(&item.skill)
        .bind(i64::from(item.sockets))
        .bind(item.rune_sockets)
        .bind(&item.slot)
        .bind(item.hands.unwrap_or(1))
        .bind(item.cooldown_s.unwrap_or(1.0))
        .bind(serde_json::to_value(&item.bonuses).unwrap_or(serde_json::Value::Null))
        .bind(item.bag_slots.map(i64::from))
        .bind(item.bag_types.as_deref())
        .bind(item.bag_kind.as_deref())
        .execute(&mut *tx)
        .await?;
    }
    for prof in &content.professions {
        sqlx::query(
            "INSERT INTO professions (id, name, prof_type) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
               prof_type = EXCLUDED.prof_type",
        )
        .bind(&prof.id)
        .bind(&prof.name)
        .bind(&prof.prof_type)
        .execute(&mut *tx)
        .await?;
    }
    for recipe in &content.recipes {
        let requires = serde_json::to_value(&recipe.requires).unwrap_or_default();
        let inputs = serde_json::to_value(&recipe.inputs).unwrap_or_default();
        sqlx::query(
            "INSERT INTO recipes (id, name, profession, rank, station, tool_type, requires, \
               inputs, time_s, xp_profession, output_type, output_qty, station_label) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (id) DO UPDATE SET \
               name = EXCLUDED.name, profession = EXCLUDED.profession, rank = EXCLUDED.rank, \
               station = EXCLUDED.station, tool_type = EXCLUDED.tool_type, \
               requires = EXCLUDED.requires, inputs = EXCLUDED.inputs, \
               time_s = EXCLUDED.time_s, xp_profession = EXCLUDED.xp_profession, \
               output_type = EXCLUDED.output_type, output_qty = EXCLUDED.output_qty, \
               station_label = EXCLUDED.station_label",
        )
        .bind(&recipe.id)
        .bind(&recipe.name)
        .bind(&recipe.profession)
        .bind(recipe.rank)
        .bind(&recipe.station)
        .bind(recipe.tool.as_ref().map(|t| t.item_type.as_str()))
        .bind(requires)
        .bind(inputs)
        .bind(recipe.time_s)
        .bind(recipe.xp.profession)
        .bind(&recipe.output.item_type)
        .bind(recipe.output.qty)
        .bind(&recipe.station_label)
        .execute(&mut *tx)
        .await?;
    }
    for quest in &content.quests {
        let goals = serde_json::to_value(&quest.goals).unwrap_or_default();
        let reward = serde_json::to_value(&quest.reward).unwrap_or_default();
        sqlx::query(
            "INSERT INTO quests (id, title, giver, goals, reward, reward_label) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, giver = EXCLUDED.giver, \
               goals = EXCLUDED.goals, reward = EXCLUDED.reward, \
               reward_label = EXCLUDED.reward_label",
        )
        .bind(&quest.id)
        .bind(&quest.title)
        .bind(&quest.giver)
        .bind(goals)
        .bind(reward)
        .bind(&quest.reward_label)
        .execute(&mut *tx)
        .await?;
    }
    if let Some(spawn) = &spawn {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES ('spawn_location', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(spawn)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES ('content_version', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(&version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if spawn.is_none() {
        tracing::warn!("в контенте нет локации со spawn: true — новые персонажи без спавна");
    }
    Ok(SeedReport {
        version,
        applied: true,
    })
}

/// Результат полной горячей перезагрузки контента (M27 фаза 1).
pub struct ReloadReport {
    /// sha256-версия контента.
    pub version: String,
    /// `true` — версия изменилась, сид применён; `false` — БД не трогалась.
    pub applied: bool,
    pub locations: usize,
    pub items: usize,
    /// Диалоги/магазины читаются из файлов (не из БД) — заменяются в памяти.
    pub dialogs: std::collections::HashMap<String, crate::dialogs::DialogNpc>,
    pub shops: std::collections::HashMap<String, crate::shops::ShopNpc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// M14/F17: у каждого навыка контента есть описание для UI
    /// (тултип/карточка) — пропуск сломает ожидание клиента.
    #[test]
    fn every_skill_has_description() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let content = load(&dir).expect("контент контента загружается");
        assert!(!content.skills.is_empty(), "навыки есть");
        for skill in &content.skills {
            assert!(
                !skill.description.trim().is_empty(),
                "нет описания у навыка {}",
                skill.id
            );
        }
    }

    /// M15/F28: у каждого навыка контента есть группа (skills.md §3).
    #[test]
    fn every_skill_has_group() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let content = load(&dir).expect("контент контента загружается");
        assert!(!content.skills.is_empty(), "навыки есть");
        for skill in &content.skills {
            assert!(
                !skill.group.trim().is_empty(),
                "нет группы у навыка {}",
                skill.id
            );
        }
    }

    /// M18/F35: контент диалогов валиден — у каждого `giver` есть запись
    /// диалога; id строк уникальны; ссылки (`quest`/`item`/`skill`/
    /// `profession`) указывают на существующий контент; `every_s > 0`;
    /// пол — `male`/`female`.
    #[test]
    fn dialogs_content_is_valid() {
        use std::collections::HashSet;

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let content = load(&dir).expect("контент контента загружается");
        let dialogs = crate::dialogs::load(&dir).expect("dialogs.json парсится");
        let shops = crate::shops::load(&dir).expect("shops.json парсится");
        assert!(!dialogs.is_empty(), "диалоги NPC есть");

        // У каждого выдающего квеста NPC — запись диалога.
        for quest in &content.quests {
            if let Some(giver) = &quest.giver {
                assert!(
                    dialogs.contains_key(giver),
                    "у квеста {} giver={giver} нет диалога",
                    quest.id
                );
            }
        }

        let item_ids: HashSet<&str> = content.items.iter().map(|i| i.id.as_str()).collect();
        let skill_ids: HashSet<&str> = content.skills.iter().map(|s| s.id.as_str()).collect();
        let prof_ids: HashSet<&str> = content.professions.iter().map(|p| p.id.as_str()).collect();
        let quest_ids: HashSet<&str> = content.quests.iter().map(|q| q.id.as_str()).collect();

        for (npc_id, npc) in &dialogs {
            let mut seen = HashSet::new();
            for line in &npc.lines {
                assert!(seen.insert(&line.id), "id строки дублируется у {npc_id}");
                if let Some(every) = line.every_s {
                    assert!(every > 0, "every_s должен быть > 0: {npc_id}/{}", line.id);
                }
                if let Some(quest) = &line.quest {
                    assert!(
                        quest_ids.contains(quest.as_str()),
                        "неизвестный quest {} у {npc_id}",
                        line.id
                    );
                }
                if let Some(when) = &line.when {
                    if let Some(item) = &when.item {
                        assert!(
                            item_ids.contains(item.def_id.as_str()),
                            "неизвестный item {} у {npc_id}",
                            line.id
                        );
                    }
                    if let Some(skill) = &when.skill {
                        assert!(
                            skill_ids.contains(skill.id.as_str()),
                            "неизвестный skill {} у {npc_id}",
                            line.id
                        );
                    }
                    if let Some(prof) = &when.profession {
                        assert!(
                            prof_ids.contains(prof.id.as_str()),
                            "неизвестная profession {} у {npc_id}",
                            line.id
                        );
                    }
                    if let Some(gender) = &when.gender {
                        assert!(
                            matches!(gender.as_str(), "male" | "female"),
                            "неизвестный пол {} у {npc_id}",
                            line.id
                        );
                    }
                }
            }

            // M18/F44: обучение навыкам (ADR-009) — навык из контента,
            // очки/золото положительные, предметы существуют, навык
            // уникален у NPC, и NPC обязательно тренер по этому навыку
            // (иначе запись недостижима — сервер отдаёт E_NO_TRAINER).
            let mut trained = HashSet::new();
            for entry in &npc.training {
                assert!(
                    skill_ids.contains(entry.skill.as_str()),
                    "неизвестный skill {} в training у {npc_id}",
                    entry.skill
                );
                assert!(
                    trained.insert(entry.skill.as_str()),
                    "skill {} дублируется в training у {npc_id}",
                    entry.skill
                );
                assert!(
                    entry.points > 0,
                    "training.points должен быть > 0: {npc_id}"
                );
                assert!(entry.gold >= 0, "training.gold должен быть ≥ 0: {npc_id}");
                for item in &entry.items {
                    assert!(
                        item_ids.contains(item.def_id.as_str()),
                        "неизвестный item {} в training у {npc_id}",
                        item.def_id
                    );
                    assert!(item.qty > 0, "training.qty должен быть > 0: {npc_id}");
                }
                let marker_trains = content
                    .locations
                    .iter()
                    .flat_map(|loc| loc.entities.iter())
                    .find(|e| e.id == *npc_id)
                    .and_then(|e| e.extra.get("trainer"))
                    .and_then(|t| t.get("skills"))
                    .and_then(|s| s.as_array())
                    .is_some_and(|arr| {
                        arr.iter().any(|v| v.as_str() == Some(entry.skill.as_str()))
                    });
                assert!(
                    marker_trains,
                    "у {npc_id} есть training.{}, но нет маркера trainer.skills в locations.json",
                    entry.skill
                );
            }

            // M18/F37/F38: дерево диалога — корень существует (любой
            // диалог начинается со стартовой реплики), ответы ведут
            // на существующие строки, id ответов уникальны (в пределах
            // NPC); переходы — произвольный граф: циклы-возвраты к
            // развилкам разрешены (уточнение заказчика M19: «в диалогах
            // можно всегда вернуться к развилке репликой»).
            assert!(
                npc.root.is_some(),
                "у {npc_id} нет root — диалог без стартовой реплики"
            );
            let root = npc
                .root
                .as_deref()
                .or_else(|| npc.lines.first().map(|l| l.id.as_str()))
                .expect("в диалоге есть строки");
            assert!(
                npc.lines.iter().any(|l| l.id == root),
                "корень {root} отсутствует у {npc_id}"
            );
            let mut reply_ids = HashSet::new();
            let mut edges: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            for line in &npc.lines {
                for reply in &line.replies {
                    assert!(
                        reply_ids.insert(reply.id.as_str()),
                        "id ответа дублируется у {npc_id}"
                    );
                    // M18/F43/F44: действие ответа — известное и только
                    // там, где у NPC оно реально есть: shop → магазин,
                    // train → список обучения в диалоге.
                    if let Some(action) = &reply.action {
                        match action.as_str() {
                            "shop" => assert!(
                                shops.contains_key(npc_id),
                                "ответ {} с action=shop, но у {npc_id} нет магазина",
                                reply.id
                            ),
                            "train" => assert!(
                                !npc.training.is_empty(),
                                "ответ {} с action=train, но у {npc_id} нет training",
                                reply.id
                            ),
                            other => panic!(
                                "неизвестное действие {other} у ответа {} ({npc_id})",
                                reply.id
                            ),
                        }
                    }
                    if let Some(next) = &reply.next {
                        assert!(
                            npc.lines.iter().any(|l| &l.id == next),
                            "ответ {} у {npc_id} ведёт на несуществующую {next}",
                            reply.id
                        );
                        edges.entry(line.id.as_str()).or_default().push(next);
                    }
                    if let Some(when) = &reply.when {
                        if let Some(gender) = &when.gender {
                            assert!(
                                matches!(gender.as_str(), "male" | "female"),
                                "неизвестный пол у ответа {}",
                                reply.id
                            );
                        }
                        if let Some(item) = &when.item {
                            assert!(
                                item_ids.contains(item.def_id.as_str()),
                                "неизвестный item у ответа {}",
                                reply.id
                            );
                        }
                    }
                }
            }
            // M19 (уточнение заказчика): ацикличность больше не
            // проверяется — возвраты к развилкам (циклы через root)
            // легальны; остаются проверки id/next/action/when выше.
            assert!(!edges.is_empty() || npc.lines.len() <= 1, "нет переходов");
        }
    }

    /// M18/F39: `shops.json` валиден — NPC существует (entity kind=npc
    /// в локациях), валюты/товары из контента предметов, markup/rate > 0,
    /// валюты уникальны, вкладки непусты.
    #[test]
    fn shops_content_is_valid() {
        use std::collections::HashSet;

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let content = load(&dir).expect("контент контента загружается");
        let shops = crate::shops::load(&dir).expect("shops.json парсится");
        assert!(!shops.is_empty(), "магазины NPC есть");

        let item_ids: HashSet<&str> = content.items.iter().map(|i| i.id.as_str()).collect();
        let mut npc_ids = HashSet::new();
        for location in &content.locations {
            for entity in &location.entities {
                if entity.kind == ld2_protocol::types::EntityKind::Npc {
                    npc_ids.insert(entity.id.as_str());
                }
            }
        }

        for (npc_id, shop) in &shops {
            assert!(
                npc_ids.contains(npc_id.as_str()),
                "нет NPC {npc_id} в локациях"
            );
            assert!(shop.markup > 0.0, "markup > 0 у {npc_id}");
            assert!(!shop.currencies.is_empty(), "валютные вкладки у {npc_id}");
            let mut currencies = HashSet::new();
            for tab in &shop.currencies {
                assert!(
                    currencies.insert(tab.currency.as_str()),
                    "валюта дубль у {npc_id}"
                );
                assert!(
                    item_ids.contains(tab.currency.as_str()),
                    "нет предмета-валюты {} у {npc_id}",
                    tab.currency
                );
                assert!(tab.rate > 0.0, "rate > 0 у {npc_id}");
                assert!(
                    !tab.items.is_empty(),
                    "пустая вкладка {} у {npc_id}",
                    tab.currency
                );
                for item in &tab.items {
                    assert!(
                        item_ids.contains(item.as_str()),
                        "нет товара {item} у {npc_id}"
                    );
                }
            }
        }
    }

    /// M18/F47: `items.json` с бонусами валиден (ADR-010): kind ∈
    /// attr|param|skill, id существует (атрибут/параметр/навык), value ≠ 0,
    /// `pct` только у param, id уникален в пределах предмета.
    #[test]
    fn items_content_is_valid() {
        use std::collections::HashSet;

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let content = load(&dir).expect("контент контента загружается");
        let skill_ids: HashSet<&str> = content.skills.iter().map(|s| s.id.as_str()).collect();
        const ATTRS: [&str; 5] = ["str", "dex", "vit", "int", "spir"];
        const PARAMS: [&str; 20] = [
            "accuracy",
            "damage",
            "damage_min",
            "damage_max",
            "crit_chance",
            "crit_damage",
            "armor",
            "dodge",
            "parry_min",
            "parry_max",
            "parry_chance",
            "block_min",
            "block_max",
            "block_chance",
            "magic_defense",
            "magic_dodge",
            "magic_resist",
            "magic_dispel",
            "magic_crit_damage",
            "magic_crit_chance",
        ];
        for item in &content.items {
            let mut seen = HashSet::new();
            for bonus in &item.bonuses {
                assert!(
                    seen.insert(bonus.id.as_str()),
                    "дубль бонуса {} у {}",
                    bonus.id,
                    item.id
                );
                assert_ne!(bonus.value, 0, "value должен быть ≠ 0: {}", item.id);
                match bonus.kind.as_str() {
                    "attr" => assert!(
                        ATTRS.contains(&bonus.id.as_str()),
                        "неизвестный атрибут {} у {}",
                        bonus.id,
                        item.id
                    ),
                    "param" => assert!(
                        PARAMS.contains(&bonus.id.as_str()),
                        "неизвестный параметр {} у {}",
                        bonus.id,
                        item.id
                    ),
                    "skill" => assert!(
                        skill_ids.contains(bonus.id.as_str()),
                        "неизвестный навык {} у {}",
                        bonus.id,
                        item.id
                    ),
                    other => panic!("неизвестный kind {other} у {}", item.id),
                }
                if bonus.pct {
                    assert_eq!(
                        bonus.kind, "param",
                        "pct допустим только у param: {}",
                        item.id
                    );
                }
            }

            // M18/F58f: у оружия/посохов со слотами `rune_sockets`
            // обязан быть указан явно (указание заказчика): деление
            // слотов на рунные/самоцветы — из контента, не из дефолта.
            if matches!(item.item_type.as_str(), "weapon" | "staff")
                && item.sockets > 0
                && item.rune_sockets.is_none()
            {
                panic!(
                    "нет явного rune_sockets у оружия/посоха {} (sockets: {})",
                    item.id, item.sockets
                );
            }

            // M18/F58d: бафф авто-приёма — param из известных, срок и
            // лимит стаков положительные (ADR-012 §6).
            if let Some(buff) = item.stats.get("buff") {
                let b = buff.as_object().expect("stats.buff — объект");
                let param = b.get("param").and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    PARAMS.contains(&param),
                    "неизвестный param {param:?} в stats.buff у {}",
                    item.id
                );
                assert_ne!(
                    b.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
                    0,
                    "value ≠ 0 в stats.buff у {}",
                    item.id
                );
                assert!(
                    b.get("duration_s").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
                    "duration_s > 0 в stats.buff у {}",
                    item.id
                );
                assert!(
                    b.get("max_stacks").and_then(|v| v.as_i64()).unwrap_or(0) >= 1,
                    "max_stacks ≥ 1 в stats.buff у {}",
                    item.id
                );
            }
            // M18/F58d: эффект на моба — только известные kind
            // (стан/кровотечение/корни/горение); bleed/burn требуют value.
            if let Some(effect) = item.stats.get("effect") {
                let e = effect.as_object().expect("stats.effect — объект");
                let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    matches!(kind, "stun" | "root" | "bleed" | "burn"),
                    "неизвестный effect.kind {kind:?} у {}",
                    item.id
                );
                assert!(
                    e.get("duration_s").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
                    "duration_s > 0 в stats.effect у {}",
                    item.id
                );
                if kind == "bleed" || kind == "burn" {
                    assert!(
                        e.get("value").and_then(|v| v.as_i64()).unwrap_or(0) >= 1,
                        "{kind} требует value ≥ 1 (урон за тик) у {}",
                        item.id
                    );
                }
                if kind == "burn" {
                    assert!(
                        e.get("period_s").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 0.5,
                        "burn требует period_s ≥ 0.5 (период тика) у {}",
                        item.id
                    );
                }
            }

            // M18/F58f: знак руны — поле `glyph` (латинские буквы —
            // глифы шрифта Dwarven Runes; заказчик 2026-11-20: допускается
            // несколько символов — рендерятся рядом); без него клиент
            // фолбэчит хешем имени, но контент рун обязан задавать знак
            // явно.
            if item.item_type == "rune" {
                let glyph = item
                    .stats
                    .get("glyph")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("нет stats.glyph у руны {}", item.id));
                assert!(
                    !glyph.is_empty() && glyph.chars().all(|c| c.is_ascii_alphabetic()),
                    "stats.glyph — латинские буквы (1..N, шрифт Dwarven Runes), получено {glyph:?} у {}",
                    item.id
                );
            }

            // M18/F47: раздел stats.param — любой параметр прямо в
            // статах: число (плоское) или {value, pct}.
            if let Some(params) = item.stats.get("param").and_then(|v| v.as_object()) {
                for (id, value) in params {
                    assert!(
                        PARAMS.contains(&id.as_str()),
                        "неизвестный параметр {id} в stats.param у {}",
                        item.id
                    );
                    let value = match value {
                        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
                        serde_json::Value::Object(map) => {
                            map.get("value").and_then(|v| v.as_i64()).unwrap_or(0)
                        }
                        other => panic!(
                            "некорректное значение {other} в stats.param ({id}) у {}",
                            item.id
                        ),
                    };
                    assert_ne!(
                        value, 0,
                        "value должен быть ≠ 0 в stats.param ({id}) у {}",
                        item.id
                    );
                }
            }
        }
    }

    /// M27: валидация ловит дубликаты id (защита БД от битого мержа).
    #[test]
    fn validate_catches_duplicate_location_id() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let mut content = load(&dir).expect("контент загружается");
        assert!(validate(&content).is_ok(), "репозиторный контент валиден");

        let dup = content.locations[0].clone();
        content.locations.push(dup);
        assert!(validate(&content).is_err(), "дубликат локации ловится");
    }

    /// M27: висячий выход — предупреждение, а не отказ (мерж карт может
    /// оставлять их временно; карта/переходы их игнорируют).
    #[test]
    fn dangling_exit_is_tolerated_with_warning() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let mut content = load(&dir).expect("контент загружается");
        content.locations[0].exits.push(ContentExit {
            id: "probe_exit".into(),
            label: "Проба".into(),
            dir: None,
            to: "no_such_location".into(),
            extra: Default::default(),
        });
        assert!(
            validate(&content).is_ok(),
            "висячий выход не должен валить сид"
        );
    }

    /// M27: отпечаток каталога стабилен, пока файлы не менялись.
    #[test]
    fn fingerprint_is_stable() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let first = fingerprint(&dir).expect("каталог читается");
        let second = fingerprint(&dir).expect("каталог читается");
        assert_eq!(first, second, "без изменений отпечаток тот же");
    }
}
