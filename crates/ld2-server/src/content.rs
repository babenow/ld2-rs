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

use ld2_protocol::types::{Entity, Exit, ItemBonus, Requires};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Строка content-файла: выход хранит целевую локацию (`to`) — на проводе
/// её быть не должно (§6 [`Exit`] без `to`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentExit {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub dir: Option<String>,
    /// Целевая локация (только БД/контент, в снапшот не попадает).
    pub to: String,
}

impl ContentExit {
    /// Проводной вид выхода (§6): без `to`.
    pub fn to_wire(&self) -> Exit {
        Exit {
            id: self.id.clone(),
            label: self.label.clone(),
            dir: self.dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentLocation {
    pub id: String,
    pub name: String,
    /// Единственная спавн-точка новых персонажей (→ `meta.spawn_location`).
    #[serde(default)]
    pub spawn: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    pub text: String,
    #[serde(default)]
    pub exits: Vec<ContentExit>,
    #[serde(default)]
    pub entities: Vec<Entity>,
}

/// Навык из `skills.json` (skills.md §3: id + имя; группа — для UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSkill {
    pub id: String,
    pub name: String,
    pub group: String,
    /// M14/F17: описание эффекта для UI (тултип/карточка навыка).
    /// `default` — старый skills.json без поля продолжает грузиться.
    #[serde(default)]
    pub description: String,
}

/// Определение предмета из `items.json` (§6 `ItemSnapshot`-ядро).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentItemDef {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    pub rarity: String,
    #[serde(default)]
    pub upgrade: i32,
    #[serde(default)]
    pub stats: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub requires: Option<Requires>,
    #[serde(default)]
    pub gold_value: i64,
    /// Стартовое количество стака при взятии/выдаче.
    #[serde(default)]
    pub qty: Option<i64>,
    /// Навык, который качается убийством этим оружием (skills.md §2.3);
    /// `None` → `unarmed`.
    #[serde(default)]
    pub skill: Option<String>,
    /// Количество сокетов (посохи, ADR-002) — создаются при вставке.
    #[serde(default)]
    pub sockets: u32,
    /// M17/F30g: кулдаун использования/скорость атаки, с
    /// (дефолт 1.0 — «по умолчанию 1 сек»).
    #[serde(default)]
    pub cooldown_s: Option<f64>,
    /// M17/F30b: слот, куда предмет надевается (main_hand/off_hand/
    /// head/…/псевдослоты пар ring|artifact); `None` — не экипируется.
    #[serde(default)]
    pub slot: Option<String>,
    /// M17/F30b: руки (2 — двуручное: занимает обе руки и блокирует
    /// левую); `None` → 1.
    #[serde(default)]
    pub hands: Option<i32>,
    /// M18/F47: бонусы предмета (ADR-010) — атрибут/параметр/навык.
    #[serde(default)]
    pub bonuses: Vec<ItemBonus>,
}

/// Профессия из `professions.json` (skills.md §5.1: первичная/вторичная).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentProfession {
    pub id: String,
    pub name: String,
    /// `primary` — занимает слот `profession_limit`; `secondary` — нет.
    #[serde(rename = "type", default = "default_primary")]
    pub prof_type: String,
}

fn default_primary() -> String {
    "primary".into()
}

/// Инструмент рецепта: предмет с этим `item_type` в инвентаре (skills.md §5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTool {
    #[serde(rename = "type")]
    pub item_type: String,
}

/// Требования рецепта (открытый объект §6: attr/skill — сервер читает).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentRequires {
    #[serde(default)]
    pub attr: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub skill: std::collections::BTreeMap<String, i64>,
}

/// Рецепт из `recipes.json` (skills.md §5.4 — payload `cmd/craft.recipes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRecipe {
    pub id: String,
    pub name: String,
    pub profession: String,
    #[serde(default = "one")]
    pub rank: i32,
    /// Станок, который должен быть в локации; `None` — без станка.
    #[serde(default)]
    pub station: Option<String>,
    #[serde(default)]
    pub tool: Option<ContentTool>,
    #[serde(default)]
    pub requires: ContentRequires,
    #[serde(default)]
    pub inputs: Vec<ContentInput>,
    #[serde(default = "one")]
    pub time_s: i32,
    #[serde(default)]
    pub xp: ContentRecipeXp,
    pub output: ContentOutput,
    #[serde(default)]
    pub station_label: Option<String>,
}

fn one() -> i32 {
    1
}

/// Ингредиент: `qty` штук с `item_type` в инвентаре.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentInput {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default = "one_i64")]
    pub qty: i64,
}

fn one_i64() -> i64 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentRecipeXp {
    #[serde(default)]
    pub profession: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentOutput {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default = "one_i64")]
    pub qty: i64,
}

/// Квест из `quests.json` (ФР-12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentQuest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub giver: Option<String>,
    #[serde(default)]
    pub goals: Vec<ContentGoal>,
    #[serde(default)]
    pub reward: ContentReward,
    #[serde(default)]
    pub reward_label: Option<String>,
}

/// Цель квеста: `kill <target>` — убийства сущности с данным id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentGoal {
    #[serde(rename = "type")]
    pub goal_type: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "one_i64")]
    pub need: i64,
    #[serde(default)]
    pub text: String,
}

/// Награда квеста: XP в пул + предметы.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentReward {
    #[serde(default)]
    pub xp: i64,
    #[serde(default)]
    pub items: Vec<ContentInput>,
}

#[derive(Debug, Deserialize)]
struct LocationsRoot {
    locations: Vec<ContentLocation>,
}

#[derive(Debug, Deserialize)]
struct SkillsRoot {
    skills: Vec<ContentSkill>,
}

#[derive(Debug, Deserialize)]
struct ItemsRoot {
    items: Vec<ContentItemDef>,
}

#[derive(Debug, Deserialize)]
struct ProfessionsRoot {
    professions: Vec<ContentProfession>,
}

#[derive(Debug, Deserialize)]
struct RecipesRoot {
    recipes: Vec<ContentRecipe>,
}

#[derive(Debug, Deserialize)]
struct QuestsRoot {
    quests: Vec<ContentQuest>,
}

/// Весь контент каталога (для версии и сида).
#[derive(Debug, Serialize)]
pub struct GameContent {
    locations: Vec<ContentLocation>,
    skills: Vec<ContentSkill>,
    items: Vec<ContentItemDef>,
    professions: Vec<ContentProfession>,
    recipes: Vec<ContentRecipe>,
    quests: Vec<ContentQuest>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("контент: не удалось прочитать {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("контент: некорректный JSON {0}: {1}")]
    Json(PathBuf, serde_json::Error),
    #[error("контент: {0}")]
    Db(#[from] sqlx::Error),
    #[error("контент: в {0} нет ни одной локации")]
    Empty(PathBuf),
}

/// Результат сида: что показать в логе старта.
#[derive(Debug)]
pub struct SeedReport {
    /// sha256-версия содержимого каталога.
    pub version: String,
    /// `false` — версия не изменилась, БД не трогалась.
    pub applied: bool,
}

/// Сид контента в PostgreSQL (D26). Вызывается при старте сервера.
pub async fn seed(pool: &PgPool, dir: impl AsRef<Path>) -> Result<SeedReport, ContentError> {
    let content = load(dir.as_ref())?;
    let version = content_version(&content)?;

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
            "INSERT INTO item_defs (id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty, skill, sockets, slot, hands, cooldown_s, bonuses) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
             ON CONFLICT (id) DO UPDATE SET \
               item_type = EXCLUDED.item_type, name = EXCLUDED.name, rarity = EXCLUDED.rarity, \
               upgrade = EXCLUDED.upgrade, stats = EXCLUDED.stats, requires = EXCLUDED.requires, \
               gold_value = EXCLUDED.gold_value, qty = EXCLUDED.qty, skill = EXCLUDED.skill, \
               sockets = EXCLUDED.sockets, slot = EXCLUDED.slot, hands = EXCLUDED.hands, \
               cooldown_s = EXCLUDED.cooldown_s, bonuses = EXCLUDED.bonuses",
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
        .bind(&item.slot)
        .bind(item.hands.unwrap_or(1))
        .bind(item.cooldown_s.unwrap_or(1.0))
        .bind(serde_json::to_value(&item.bonuses).unwrap_or(serde_json::Value::Null))
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

/// Загрузка трёх обязательных файлов каталога.
fn load(dir: &Path) -> Result<GameContent, ContentError> {
    let read = |name: &str| -> Result<String, ContentError> {
        let path = dir.join(name);
        std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path, e))
    };

    let locations: LocationsRoot = serde_json::from_str(&read("locations.json")?)
        .map_err(|e| ContentError::Json(dir.join("locations.json"), e))?;
    let skills: SkillsRoot = serde_json::from_str(&read("skills.json")?)
        .map_err(|e| ContentError::Json(dir.join("skills.json"), e))?;
    let items: ItemsRoot = serde_json::from_str(&read("items.json")?)
        .map_err(|e| ContentError::Json(dir.join("items.json"), e))?;
    let professions: ProfessionsRoot = serde_json::from_str(&read("professions.json")?)
        .map_err(|e| ContentError::Json(dir.join("professions.json"), e))?;
    let recipes: RecipesRoot = serde_json::from_str(&read("recipes.json")?)
        .map_err(|e| ContentError::Json(dir.join("recipes.json"), e))?;
    let quests: QuestsRoot = serde_json::from_str(&read("quests.json")?)
        .map_err(|e| ContentError::Json(dir.join("quests.json"), e))?;

    if locations.locations.is_empty() {
        return Err(ContentError::Empty(dir.to_path_buf()));
    }
    Ok(GameContent {
        locations: locations.locations,
        skills: skills.skills,
        items: items.items,
        professions: professions.professions,
        recipes: recipes.recipes,
        quests: quests.quests,
    })
}

/// Версия контента: sha256 от канонического дерева **всего** контента
/// (неизменное содержимое → неизменная версия независимо от имён файлов).
fn content_version(content: &GameContent) -> Result<String, ContentError> {
    let canonical = serde_json::to_string(content)
        .map_err(|e| ContentError::Json(PathBuf::from("<canonical>"), e))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
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
            // NPC), ветки ацикличны (DFS от корня).
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
            // Ацикличность: тройной обход (граф маленький).
            fn has_cycle<'a>(
                node: &'a str,
                edges: &std::collections::HashMap<&'a str, Vec<&'a str>>,
                visiting: &mut std::collections::HashSet<&'a str>,
                done: &mut std::collections::HashSet<&'a str>,
            ) -> bool {
                if done.contains(node) {
                    return false;
                }
                if !visiting.insert(node) {
                    return true;
                }
                let cyclic = edges
                    .get(node)
                    .is_some_and(|nexts| nexts.iter().any(|n| has_cycle(n, edges, visiting, done)));
                visiting.remove(node);
                done.insert(node);
                cyclic
            }
            let mut visiting = std::collections::HashSet::new();
            let mut done = std::collections::HashSet::new();
            assert!(
                !has_cycle(root, &edges, &mut visiting, &mut done),
                "цикл в ветках диалога {npc_id}"
            );
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
        const ATTRS: [&str; 4] = ["str", "dex", "vit", "int"];
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
}
