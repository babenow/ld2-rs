//! M18/F35: динамические диалоги NPC — контент `content/dialogs.json`
//! (реплики с условиями: пол/уровень/атрибуты/навыки/профессии/предметы,
//! тайминг `every_s`, связь с квестом), загрузка в память при старте
//! (без БД — в отличие от кулдаунов `every_s`, см. `db::dialogs`).
//!
//! Шаблоны: `{name}` → имя персонажа (сервер подставляет, клиенту уходит
//! готовый текст). Условия `when` — AND; строки с `quest` видны, пока
//! квест доступен.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::content::ContentError;

/// Корень `content/dialogs.json` — по образцу прочих файлов каталога.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DialogsRoot {
    #[serde(default)]
    pub npcs: Vec<DialogNpc>,
}

/// Диалог одного NPC: корень ветки, строки с условиями и обучение
/// (M18/F42: поле `greeting` убрано — ADR-008; M18/F44: `training` —
/// ADR-009).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DialogNpc {
    pub id: String,
    /// M18/F37: корень ветки диалога; без поля — первая строка списка.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default)]
    pub lines: Vec<DialogLine>,
    /// M18/F44: чему этот NPC учит и по какой цене (ADR-009); пусто —
    /// не тренер по навыкам.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub training: Vec<TrainingEntry>,
}

/// M18/F44: обучение навыку у NPC (ADR-009) — повышение ранга стоит
/// очков опыта (пул) + платы, указанной здесь.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrainingEntry {
    /// id навыка из `skills.json`.
    pub skill: String,
    /// Плата очками опыта (пул квестов), > 0.
    pub points: i64,
    /// Плата золотом (монеты); 0 — без золота.
    #[serde(default)]
    pub gold: i64,
    /// Плата предметами (def_id → qty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<TrainingItem>,
}

/// M18/F44: предмет платы за обучение.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrainingItem {
    pub def_id: String,
    #[serde(default = "training_qty")]
    pub qty: i64,
}

fn training_qty() -> i64 {
    1
}

/// Строка диалога.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DialogLine {
    /// Стабильный id — в т.ч. ключ кулдауна `every_s` в БД.
    pub id: String,
    pub text: String,
    /// Условия видимости (все выполняются — AND).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<DialogWhen>,
    /// Квест, который предлагает строка; видна, пока квест доступен.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// Тайминг-реплика: раз в N секунд на персонажа (кулдаун в БД).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_s: Option<u64>,
    /// M18/F37: реплики персонажа — ответы, связывающие строки в
    /// ветку диалога (`next` — id целевой строки, `None` — конец).
    #[serde(default)]
    pub replies: Vec<DialogReply>,
}

/// M18/F37: реплика персонажа (ответ) — переход на строку `next`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DialogReply {
    pub id: String,
    pub text: String,
    /// Целевая строка; `None` — диалог заканчивается.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Условия видимости ответа (AND) — как у строк.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<DialogWhen>,
    /// M18/F43: действие вместо перехода по ветке (`"shop"` — открыть
    /// магазин NPC); контент валидируется тестом (у NPC есть магазин).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// Условия видимости строки (AND; незаданные ключи не проверяются).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DialogWhen {
    /// Пол персонажа (`male`/`female` — M18/F34).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<LevelMin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<AttrMin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<SkillMin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profession: Option<SkillMin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemQty>,
}

/// Минимальный уровень персонажа.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelMin {
    pub min: i64,
}

/// Минимальное значение характеристики (`str`/`dex`/…).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttrMin {
    pub name: String,
    pub min: i64,
}

/// Минимальный ранг навыка/профессии.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillMin {
    pub id: String,
    pub min_rank: i64,
}

/// Предмет в рюкзаке: `def_id` ≥ `qty` (дефолт 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemQty {
    pub def_id: String,
    #[serde(default = "default_qty")]
    pub qty: i64,
}

fn default_qty() -> i64 {
    1
}

/// M18/F37: контекст персонажа для диалогов (owned — собирается из
/// БД один раз и живёт в обеих командах диалога).
pub struct DialogContext {
    pub name: String,
    pub gender: String,
    pub level: i64,
    pub attrs: BTreeMap<String, i64>,
    /// Навыки: id → ранг.
    pub skills: HashMap<String, i64>,
    /// Профессии: id → ранг.
    pub professions: HashMap<String, i64>,
    /// Рюкзак: def_id → суммарное количество.
    pub items: HashMap<String, i64>,
}

impl DialogContext {
    /// Borrowed-контекст для чистых функций видимости.
    pub fn line_ctx<'a>(&'a self, available: &'a HashSet<String>) -> LineCtx<'a> {
        LineCtx {
            name: &self.name,
            gender: &self.gender,
            level: self.level,
            attrs: &self.attrs,
            skills: &self.skills,
            professions: &self.professions,
            items: &self.items,
            available_quests: available,
        }
    }
}

/// Контекст персонажа для условий (M18/F35).
pub struct LineCtx<'a> {
    pub name: &'a str,
    pub gender: &'a str,
    pub level: i64,
    pub attrs: &'a BTreeMap<String, i64>,
    /// Навыки: id → ранг.
    pub skills: &'a HashMap<String, i64>,
    /// Профессии: id → ранг.
    pub professions: &'a HashMap<String, i64>,
    /// Рюкзак: def_id → суммарное количество.
    pub items: &'a HashMap<String, i64>,
    /// Квесты NPC в фазе available — гейт строк с `quest`.
    pub available_quests: &'a HashSet<String>,
}

/// Видимая строка: готовый текст + признак «отметить показ» (начало окна
/// `every_s` — сервер пишет `shown_at = now`).
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleLine {
    pub id: String,
    pub text: String,
    pub quest: Option<String>,
    pub mark_shown: bool,
}

/// Рендер шаблона: `{name}` → имя персонажа.
pub fn render(text: &str, name: &str) -> String {
    text.replace("{name}", name)
}

/// M18/F42: жёсткие условия видимости строки (гейт `quest` → условия
/// `when`) — без учёта кулдауна `every_s`. Скрытая условием строка
/// недостижима в любом случае; кулдаун же влияет только на активность
/// ответа (ADR-008).
fn conditions_ok(line: &DialogLine, ctx: &LineCtx) -> bool {
    if let Some(quest) = &line.quest {
        if !ctx.available_quests.contains(quest) {
            return false;
        }
    }
    if let Some(when) = &line.when {
        if !when_ok(when, ctx) {
            return false;
        }
    }
    true
}

/// M18/F42: остаток окна `every_s` строки в секундах — `Some`, пока
/// строка скрыта кулдауном (ответ на неё показывается неактивным с
/// таймером, ADR-008).
pub fn cooldown_left(line: &DialogLine, now: i64, shown: &HashMap<String, i64>) -> Option<u64> {
    let every = line.every_s? as i64;
    let at = *shown.get(&line.id)?;
    let left = every.saturating_sub(now.saturating_sub(at));
    (left > 0).then_some(left as u64)
}

/// Чистая функция видимости одной строки: `None` — скрыта (гейт
/// `quest` → условия `when` → окно `every_s`), `Some(true)` — показать
/// и **отметить показ** (открытие окна `every_s` — только при входе в
/// узел; пик видимости ответов отметку не ставит).
pub fn line_visible(
    line: &DialogLine,
    ctx: &LineCtx,
    now: i64,
    shown: &HashMap<String, i64>,
) -> Option<bool> {
    if !conditions_ok(line, ctx) {
        return None;
    }
    if let Some(every) = line.every_s {
        match shown.get(&line.id) {
            Some(at) if now.saturating_sub(*at) < every as i64 => return None,
            // Окно истекло либо показа не было — показ и новая метка.
            _ => return Some(true),
        }
    }
    Some(false)
}

/// Чистая функция видимости строк (юнит-тесты покрывают каждое условие):
/// условия AND + `every_s` + гейт `quest`. `now`/`shown` — epoch-секунды.
pub fn visible_lines(
    npc: &DialogNpc,
    ctx: &LineCtx,
    now: i64,
    shown: &HashMap<String, i64>,
) -> Vec<VisibleLine> {
    let mut out = Vec::new();
    for line in &npc.lines {
        let Some(mark) = line_visible(line, ctx, now, shown) else {
            continue;
        };
        out.push(VisibleLine {
            id: line.id.clone(),
            text: render(&line.text, ctx.name),
            quest: line.quest.clone(),
            mark_shown: mark,
        });
    }
    out
}

/// M18/F37: разрешён ли ответ в текущем контексте (условия ответа).
pub fn reply_allowed(reply: &DialogReply, ctx: &LineCtx) -> bool {
    reply.when.as_ref().is_none_or(|when| when_ok(when, ctx))
}

/// M18/F37: видимый узел диалога — строка + её видимые ответы (тексты
/// уже отрендерены сервером).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeView {
    pub id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    pub replies: Vec<ReplyView>,
}

/// M18/F37: ответ, который видит персонаж (id — для `cmd/npc.reply`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplyView {
    pub id: String,
    pub text: String,
    /// M18/F42: секунды до конца кулдауна целевой строки — ответ
    /// показывается неактивным с таймером (ADR-008); `None` — активен.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<u64>,
    /// M18/F43: действие ответа (`"shop"`) — клиент шлёт `shop.list`
    /// вместо `npc.reply`; `None` — обычный переход по ветке.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// M18/F37: вход в узел (корень из `npc.talk` либо переход по ответу):
/// `None` — строка скрыта; `Some((узел, mark))` — показать, `mark` —
/// отметить открытие окна `every_s` в БД (вызывающий пишет).
pub fn enter_node(
    npc: &DialogNpc,
    line_id: &str,
    ctx: &LineCtx,
    now: i64,
    shown: &HashMap<String, i64>,
) -> Option<(NodeView, bool)> {
    let line = npc.lines.iter().find(|l| l.id == line_id)?;
    let mark = line_visible(line, ctx, now, shown)?;
    let replies = visible_replies(line, npc, ctx, now, shown)
        .into_iter()
        .map(|visible| ReplyView {
            id: visible.reply.id.clone(),
            text: render(&visible.reply.text, ctx.name),
            wait_s: visible.wait_s,
            action: visible.reply.action.clone(),
        })
        .collect();
    Some((
        NodeView {
            id: line.id.clone(),
            text: render(&line.text, ctx.name),
            quest: line.quest.clone(),
            replies,
        },
        mark,
    ))
}

/// M18/F42: ответ, видимый персонажу: сама реплика + остаток кулдауна
/// её целевой строки (`wait_s`, ADR-008).
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleReply<'a> {
    pub reply: &'a DialogReply,
    /// Секунды до конца кулдауна; `None` — ответ активен.
    pub wait_s: Option<u64>,
}

/// Видимые ответы строки — условия ответа (AND) **и** судьба целевой
/// строки: скрытая **условием/квестом** — ответ не показывается вовсе;
/// скрытая **кулдауном `every_s`** — показывается с `wait_s` (неактивной,
/// ADR-008). Пик видимости отметку `every_s` не ставит (в узел ещё не
/// входили — метка ставится только при входе). Ответ без `next` (конец
/// диалога) разрешён всегда.
pub fn visible_replies<'a>(
    line: &'a DialogLine,
    npc: &DialogNpc,
    ctx: &LineCtx,
    now: i64,
    shown: &HashMap<String, i64>,
) -> Vec<VisibleReply<'a>> {
    line.replies
        .iter()
        .filter_map(|reply| {
            if !reply_allowed(reply, ctx) {
                return None;
            }
            let Some(next_id) = &reply.next else {
                return Some(VisibleReply {
                    reply,
                    wait_s: None,
                });
            };
            let target = npc.lines.iter().find(|target| &target.id == next_id)?;
            if !conditions_ok(target, ctx) {
                return None;
            }
            if line_visible(target, ctx, now, shown).is_some() {
                Some(VisibleReply {
                    reply,
                    wait_s: None,
                })
            } else {
                cooldown_left(target, now, shown).map(|wait_s| VisibleReply {
                    reply,
                    wait_s: Some(wait_s),
                })
            }
        })
        .collect()
}

fn when_ok(when: &DialogWhen, ctx: &LineCtx) -> bool {
    if let Some(gender) = &when.gender {
        if gender != ctx.gender {
            return false;
        }
    }
    if let Some(level) = &when.level {
        if ctx.level < level.min {
            return false;
        }
    }
    if let Some(attr) = &when.attr {
        if ctx.attrs.get(&attr.name).copied().unwrap_or(0) < attr.min {
            return false;
        }
    }
    if let Some(skill) = &when.skill {
        if ctx.skills.get(&skill.id).copied().unwrap_or(0) < skill.min_rank {
            return false;
        }
    }
    if let Some(prof) = &when.profession {
        if ctx.professions.get(&prof.id).copied().unwrap_or(0) < prof.min_rank {
            return false;
        }
    }
    if let Some(item) = &when.item {
        if ctx.items.get(&item.def_id).copied().unwrap_or(0) < item.qty {
            return false;
        }
    }
    true
}

/// Загрузка `dialogs.json`: файла нет → пусто (диалоги опциональны),
/// сломанный JSON → ошибка (в `AppState::new` — warn + пусто; валидность
/// контента ловят контент-тесты в CI).
pub fn load(dir: &Path) -> Result<HashMap<String, DialogNpc>, ContentError> {
    let path = dir.join("dialogs.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path.clone(), e))?;
    let root: DialogsRoot = serde_json::from_str(&raw).map_err(|e| ContentError::Json(path, e))?;
    Ok(root
        .npcs
        .into_iter()
        .map(|npc| (npc.id.clone(), npc))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npc() -> DialogNpc {
        serde_json::from_value(serde_json::json!({
            "id": "npc_elder",
            "lines": [
                { "id": "l_plain", "text": "Просто реплика" },
                { "id": "l_female", "text": "Для женщин", "when": { "gender": "female" } },
                { "id": "l_level", "text": "Для пятого уровня", "when": { "level": { "min": 5 } } },
                { "id": "l_attr", "text": "Сильным", "when": { "attr": { "name": "str", "min": 12 } } },
                { "id": "l_skill", "text": "Мечникам", "when": { "skill": { "id": "sword", "min_rank": 3 } } },
                { "id": "l_prof", "text": "Кузнецам", "when": { "profession": { "id": "blacksmith", "min_rank": 1 } } },
                { "id": "l_item", "text": "С зельем", "when": { "item": { "def_id": "potion_heal" } } },
                { "id": "l_timed", "text": "Раз в полчаса", "every_s": 1800 },
                { "id": "l_quest", "text": "Просьба", "quest": "q_wolf_hunt" },
                { "id": "l_combo", "text": "Женщина и зелье",
                  "when": { "gender": "female", "item": { "def_id": "potion_heal", "qty": 2 } } }
            ]
        }))
        .expect("диалог парсится")
    }

    fn mk_ctx<'a>(
        name: &'a str,
        gender: &'a str,
        attrs: &'a BTreeMap<String, i64>,
        skills: &'a HashMap<String, i64>,
        professions: &'a HashMap<String, i64>,
        items: &'a HashMap<String, i64>,
        quests: &'a HashSet<String>,
    ) -> LineCtx<'a> {
        LineCtx {
            name,
            gender,
            level: 1,
            attrs,
            skills,
            professions,
            items,
            available_quests: quests,
        }
    }

    /// Базовый контекст персонажа-мужчины 1-го уровня без всего.
    /// (attrs, навыки, профессии, предметы) базового персонажа.
    type Base = (
        BTreeMap<String, i64>,
        HashMap<String, i64>,
        HashMap<String, i64>,
        HashMap<String, i64>,
    );

    fn base() -> Base {
        (
            BTreeMap::from([("str".to_string(), 10i64)]),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn ids(lines: &[VisibleLine]) -> Vec<&str> {
        lines.iter().map(|l| l.id.as_str()).collect()
    }

    /// M18/F35: `{name}` подставляется в приветствие и строки.
    #[test]
    fn render_substitutes_name() {
        assert_eq!(render("Привет, {name}!", "Ким"), "Привет, Ким!");
        assert_eq!(render("Без шаблона", "Ким"), "Без шаблона");
    }

    /// M18/F35: каждое условие по отдельности открывает/закрывает строку.
    #[test]
    fn every_condition_gates_its_line() {
        let n = npc();
        let (attrs, skills, profs, items) = base();
        let empty_q = HashSet::new();
        // Мужчина 1 ур., без навыков/предметов: только plain и timed.
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        let now_shown = HashMap::new();
        let lines = visible_lines(&n, &ctx, 1_000_000, &now_shown);
        let got = ids(&lines);
        assert!(got.contains(&"l_plain"), "безусловная строка есть: {got:?}");
        assert!(!got.contains(&"l_female"), "пол не подходит");
        assert!(!got.contains(&"l_level"), "уровень 1 < 5");
        assert!(!got.contains(&"l_attr"), "str 10 < 12");
        assert!(!got.contains(&"l_skill"), "нет навыка");
        assert!(!got.contains(&"l_prof"), "нет профессии");
        assert!(!got.contains(&"l_item"), "нет предмета");
        assert!(!got.contains(&"l_quest"), "квест недоступен");
        assert!(!got.contains(&"l_combo"), "комбо не выполняется");
        assert!(got.contains(&"l_timed"), "первый показ тайминг-строки");

        // Женщина → l_female открывается.
        let ctx = mk_ctx("Мира", "female", &attrs, &skills, &profs, &items, &empty_q);
        let lines = visible_lines(&n, &ctx, 1_000_000, &now_shown);
        let got = ids(&lines);
        assert!(got.contains(&"l_female"), "пол подходит: {got:?}");

        // Уровень 5 → l_level.
        let mut ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        ctx.level = 5;
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_level"));

        // str 12 → l_attr.
        let attrs = BTreeMap::from([("str".to_string(), 12i64)]);
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_attr"));

        // sword 3 → l_skill.
        let skills = HashMap::from([("sword".to_string(), 3i64)]);
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_skill"));

        // blacksmith 1 → l_prof.
        let profs = HashMap::from([("blacksmith".to_string(), 1i64)]);
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_prof"));

        // 1 зелье (qty-дефолт 1) → l_item; combo нужно 2 → не открывается.
        let items = HashMap::from([("potion_heal".to_string(), 1i64)]);
        let ctx = mk_ctx("Мира", "female", &attrs, &skills, &profs, &items, &empty_q);
        let lines = visible_lines(&n, &ctx, 1_000_000, &now_shown);
        let got = ids(&lines);
        assert!(got.contains(&"l_item"), "зелье есть: {got:?}");
        assert!(!got.contains(&"l_combo"), "нужно 2 зелья");
        let items = HashMap::from([("potion_heal".to_string(), 2i64)]);
        let ctx = mk_ctx("Мира", "female", &attrs, &skills, &profs, &items, &empty_q);
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_combo"));

        // Доступный квест → l_quest.
        let quests = HashSet::from(["q_wolf_hunt".to_string()]);
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &quests);
        assert!(ids(&visible_lines(&n, &ctx, 1_000_000, &now_shown)).contains(&"l_quest"));
    }

    /// M18/F35: `every_s` — показ один раз за период; по истечении окна
    /// строка видна снова (и метится заново).
    #[test]
    fn timed_line_opens_once_per_period() {
        let n = npc();
        let (attrs, skills, profs, items) = base();
        let empty_q = HashSet::new();
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty_q);
        let now = 1_000_000;

        // Никогда не показана → видна + метка.
        let mut shown = HashMap::new();
        let lines = visible_lines(&n, &ctx, now, &shown);
        let timed = lines.iter().find(|l| l.id == "l_timed").expect("видна");
        assert!(timed.mark_shown, "первый показ метится");

        // Показ 100 с назад (окно 1800) → скрыта.
        shown.insert("l_timed".into(), now - 100);
        assert!(
            !ids(&visible_lines(&n, &ctx, now, &shown)).contains(&"l_timed"),
            "внутри окна скрыта"
        );

        // Показ 2000 с назад (окно истекло) → видна + новая метка.
        shown.insert("l_timed".into(), now - 2000);
        let lines = visible_lines(&n, &ctx, now, &shown);
        let timed = lines.iter().find(|l| l.id == "l_timed").expect("видна");
        assert!(timed.mark_shown, "после окна метится заново");

        // Строка без every_s меток не ставит.
        let plain = lines.iter().find(|l| l.id == "l_plain").expect("plain");
        assert!(!plain.mark_shown, "обычная строка не метится");
    }

    /// M18/F37: узел + ответы: фильтр условий ответа, гейт цели
    /// (скрытая строка прячет ветку), конец ветки без `next`, пик не
    /// метит `every_s` — метка только при входе (`mark`).
    #[test]
    fn node_and_replies_walk_the_tree() {
        let npc: DialogNpc = serde_json::from_value(serde_json::json!({
            "id": "npc_elder",
            "root": "l_hello",
            "lines": [
                { "id": "l_hello", "text": "Как дела, {name}?", "replies": [
                    { "id": "r_work", "text": "Какая работа?", "next": "l_errand" },
                    { "id": "r_pack", "text": "Про зелье", "next": "l_pack" },
                    { "id": "r_f", "text": "Опасно?", "next": "l_female" },
                    { "id": "r_bye", "text": "Пока." }
                ] },
                { "id": "l_errand", "text": "Отнеси весть!", "every_s": 1800,
                  "replies": [ { "id": "r_back", "text": "Хорошо.", "next": null } ] },
                { "id": "l_pack", "text": "Зелье с тобой.",
                  "when": { "item": { "def_id": "potion_heal" } } },
                { "id": "l_female", "text": "Береги себя.",
                  "when": { "gender": "female" } }
            ]
        }))
        .expect("диалог парсится");

        let empty = HashSet::new();
        let items = HashMap::new();
        let (attrs, skills, profs) = (BTreeMap::new(), HashMap::new(), HashMap::new());
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty);
        let now = 1_000_000;
        let shown = HashMap::new();

        // Корень: целевые ветки скрыты по условиям (нет зелья/пол),
        // конец ветки (r_bye) и рабочая ветка — видны.
        let (node, mark) = enter_node(&npc, "l_hello", &ctx, now, &shown).expect("корень виден");
        assert_eq!(node.id, "l_hello");
        assert_eq!(node.text, "Как дела, Ким?", "шаблон отрендерен");
        assert!(!mark, "корень без every_s не метится");
        let ids: Vec<&str> = node.replies.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r_work", "r_bye"], "видимые ответы: {ids:?}");

        // Женщина с зельем открывает обе условные ветки.
        let items = HashMap::from([("potion_heal".to_string(), 1i64)]);
        let ctx = mk_ctx("Мира", "female", &attrs, &skills, &profs, &items, &empty);
        let (node, _) = enter_node(&npc, "l_hello", &ctx, now, &shown).expect("корень виден");
        let ids: Vec<&str> = node.replies.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r_work", "r_pack", "r_f", "r_bye"], "{ids:?}");

        // Вход в тайминг-узел метит показ; пик (visible_replies из
        // l_hello) метки не требует и не меняет shown.
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &empty);
        let (node, mark) = enter_node(&npc, "l_errand", &ctx, now, &shown).expect("узел виден");
        assert!(mark, "первый показ every_s метится");
        assert_eq!(node.replies[0].id, "r_back");

        // Повторный вход в пределах окна → узел скрыт.
        let mut shown = HashMap::new();
        shown.insert("l_errand".to_string(), now - 100);
        assert!(
            enter_node(&npc, "l_errand", &ctx, now, &shown).is_none(),
            "в окне скрыт"
        );

        // Прямой вызов visible_replies (корень): цель l_errand в окне
        // every_s — M18/F42 ответ r_work остаётся, но неактивный
        // (wait_s > 0, ADR-008); условные ветки скрыты.
        let root = npc.lines.iter().find(|l| l.id == "l_hello").unwrap();
        let replies = visible_replies(root, &npc, &ctx, now, &shown);
        let ids: Vec<&str> = replies.iter().map(|r| r.reply.id.as_str()).collect();
        assert_eq!(
            ids,
            ["r_work", "r_pack", "r_bye"],
            "r_work виден с таймером: {ids:?}"
        );
        let wait = replies
            .iter()
            .find(|r| r.reply.id == "r_work")
            .and_then(|r| r.wait_s);
        assert!(
            wait.is_some_and(|s| s > 0 && s <= 1800),
            "остаток кулдауна: {wait:?}"
        );

        // Несуществующий узел → None.
        assert!(enter_node(&npc, "l_nope", &ctx, now, &shown).is_none());
    }

    /// M18/F35: строка с `quest` несёт его id (кнопка на клиенте).
    #[test]
    fn quest_line_carries_quest_id() {
        let n = npc();
        let (attrs, skills, profs, items) = base();
        let quests = HashSet::from(["q_wolf_hunt".to_string()]);
        let ctx = mk_ctx("Ким", "male", &attrs, &skills, &profs, &items, &quests);
        let lines = visible_lines(&n, &ctx, 1_000_000, &HashMap::new());
        let line = lines.iter().find(|l| l.id == "l_quest").expect("видна");
        assert_eq!(line.quest.as_deref(), Some("q_wolf_hunt"));
    }

    /// M18/F42/F43: `wait_s` и `action` сериализуются только когда заданы —
    /// у обычной активной реплики полей нет (совместимость §11.2).
    #[test]
    fn reply_wait_s_serializes_only_when_set() {
        let active = ReplyView {
            id: "r_a".into(),
            text: "А".into(),
            wait_s: None,
            action: None,
        };
        let cooling = ReplyView {
            id: "r_b".into(),
            text: "Б".into(),
            wait_s: Some(42),
            action: None,
        };
        let acting = ReplyView {
            id: "r_s".into(),
            text: "Покажи свои товары.".into(),
            wait_s: None,
            action: Some("shop".into()),
        };
        let active: serde_json::Value = serde_json::to_value(active).unwrap();
        let cooling: serde_json::Value = serde_json::to_value(cooling).unwrap();
        let acting: serde_json::Value = serde_json::to_value(acting).unwrap();
        assert!(active.get("wait_s").is_none(), "активная: {active}");
        assert!(active.get("action").is_none(), "обычная: {active}");
        assert_eq!(cooling["wait_s"], 42, "на кулдауне: {cooling}");
        assert_eq!(acting["action"], "shop", "действие: {acting}");
        assert!(
            acting.get("wait_s").is_none(),
            "действие без таймера: {acting}"
        );
    }
}
