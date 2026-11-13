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

/// Диалог одного NPC: приветствие и строки с условиями.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DialogNpc {
    pub id: String,
    /// Шаблон приветствия (`{name}` → имя персонажа).
    #[serde(default)]
    pub greeting: String,
    #[serde(default)]
    pub lines: Vec<DialogLine>,
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

/// Чистая функция видимости строк: гейт `quest` → условия `when` →
/// окно `every_s` (нет показа либо показан не позже окна → скрыта;
/// окно истекло → показываем и метим заново).
pub fn visible_lines(
    npc: &DialogNpc,
    ctx: &LineCtx,
    now: i64,
    shown: &HashMap<String, i64>,
) -> Vec<VisibleLine> {
    let mut out = Vec::new();
    for line in &npc.lines {
        if let Some(quest) = &line.quest {
            if !ctx.available_quests.contains(quest) {
                continue;
            }
        }
        if let Some(when) = &line.when {
            if !when_ok(when, ctx) {
                continue;
            }
        }
        let mut mark = false;
        if let Some(every) = line.every_s {
            match shown.get(&line.id) {
                Some(at) if now.saturating_sub(*at) < every as i64 => continue,
                Some(_) | None => mark = true,
            }
        }
        out.push(VisibleLine {
            id: line.id.clone(),
            text: render(&line.text, ctx.name),
            quest: line.quest.clone(),
            mark_shown: mark,
        });
    }
    out
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
            "greeting": "Привет, {name}!",
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
}
