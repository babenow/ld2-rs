//! Дерево диалога: видимость строк/ответов и переходы по узлам.

use super::*;

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
