//! Редьюсеры крафта, квестов и криминала (RF4h).

use ld2_protocol::names;

use super::super::*;

impl AppStore {
    /// Крафт/квесты/криминал: `evt/craft/*`, `evt/quest/*`, `evt/crime/*` (RF4h).
    pub(crate) fn reduce_craft(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            names::EVT_QUEST_PROGRESS | names::EVT_QUEST_UPDATED => {
                #[derive(serde::Deserialize)]
                struct WithQuest {
                    quest: QuestState,
                    #[serde(default)]
                    pool_left: Option<i64>,
                }
                let Ok(w) = serde_json::from_value::<WithQuest>(payload) else {
                    return vec![];
                };
                // ФР-12: замена квеста в снапшоте + пул от сдачи (§7).
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(pool) = w.pool_left {
                        snap.char.xp_pool = pool;
                    }
                    if let Some(q) = snap.char.quests.iter_mut().find(|q| q.id == w.quest.id) {
                        *q = w.quest.clone();
                    } else {
                        snap.char.quests.push(w.quest.clone());
                    }
                }
                // M18/F35: квест взят/сдан — убирается из открытого диалога
                // NPC (реплики и кнопки не врут).
                if let Some(dialog) = self.npc_dialog.as_mut() {
                    if !matches!(w.quest.state, ld2_protocol::types::QuestPhase::Available) {
                        dialog.quests.retain(|q| q.id != w.quest.id);
                    }
                }
                let title = w.quest.title.unwrap_or(w.quest.id);
                self.push_log(
                    LogKind::Quest,
                    format!("Квест «{title}»: {:?}", w.quest.state),
                );
                vec![]
            }
            names::EVT_CRAFT_RESULT => {
                let Ok(result) = serde_json::from_value::<ld2_protocol::evt::CraftResult>(payload)
                else {
                    return vec![];
                };
                if result.ok {
                    let made: Vec<String> = result
                        .made
                        .iter()
                        .map(|m| format!("{}×{}", m.uid, m.qty))
                        .collect();
                    self.push_log(
                        LogKind::Loot,
                        format!("Крафт готов: {} → {}", result.recipe_id, made.join(", ")),
                    );
                } else {
                    self.push_log(
                        LogKind::Loot,
                        format!("Крафт провален: {}", result.recipe_id),
                    );
                }
                vec![]
            }
            names::EVT_CRAFT_PROFESSION => {
                #[derive(serde::Deserialize)]
                struct WithProfession {
                    profession: ld2_protocol::types::ProfessionSnapshot,
                    #[serde(default)]
                    name: Option<String>,
                    #[serde(default)]
                    gain: Option<i64>,
                }
                let Ok(w) = serde_json::from_value::<WithProfession>(payload) else {
                    return vec![];
                };
                let old = self.snapshot.as_ref().and_then(|s| {
                    s.char
                        .professions
                        .iter()
                        .find(|p| p.id == w.profession.id)
                        .map(|p| (p.level, p.rank))
                });
                if let Some(snap) = self.snapshot.as_mut() {
                    if let Some(p) = snap
                        .char
                        .professions
                        .iter_mut()
                        .find(|p| p.id == w.profession.id)
                    {
                        *p = w.profession.clone();
                    } else {
                        snap.char.professions.push(w.profession.clone());
                    }
                }
                // M22: журнал на русском (имя из контента) — ранг
                // только при смене, опыт и уровень профессии отдельно.
                let name = w.name.clone().unwrap_or_else(|| w.profession.id.clone());
                if old.is_none_or(|(_, rank)| rank != w.profession.rank) {
                    self.push_log(
                        LogKind::Loot,
                        format!("Профессия «{name}»: ранг {}", w.profession.rank),
                    );
                }
                if let Some(gain) = w.gain.filter(|g| *g > 0) {
                    self.push_log(LogKind::Xp, format!("{name}: опыт +{gain}"));
                }
                // Условие устойчиво: если записи в снапшоте ещё не
                // было (old=None) — сравниваем с0, иначе повышение
                // терялось (фикс: строка «+N» приходила, а об уровне —
                // нет).
                if w.profession.level > old.map(|(level, _)| level).unwrap_or(0) {
                    self.push_log(
                        LogKind::Xp,
                        format!("Уровень профессии {name}: {}", w.profession.level),
                    );
                }
                vec![]
            }
            names::EVT_CRIME_STATUS => {
                let Ok(update) = serde_json::from_value::<CrimeStatusUpdate>(payload) else {
                    return vec![];
                };
                // ФР-11: полная замена списка — клиент НЕ пересчитывает blend (§4.10).
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.crime.statuses = update.statuses.clone();
                }
                let text = update.log_hint.unwrap_or_else(|| {
                    let kinds: Vec<&str> =
                        update.statuses.iter().map(|s| s.kind.as_str()).collect();
                    if kinds.is_empty() {
                        "Статусы преступления сняты".to_string()
                    } else {
                        format!("Статусы: {}", kinds.join(", "))
                    }
                });
                self.push_log(LogKind::Crime, text);
                vec![]
            }
            names::EVT_CRIME_ADDED => {
                let Ok(added) = serde_json::from_value::<CrimeAdded>(payload) else {
                    return vec![];
                };
                // Журнал + обновление бейджей (досерверный срок — как есть).
                self.crime_log.push(added.log_entry.clone());
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char
                        .crime
                        .statuses
                        .retain(|s| s.kind != added.status.kind);
                    snap.char.crime.statuses.push(added.status);
                }
                self.push_log(
                    LogKind::Crime,
                    format!(
                        "{}: +{}с (итого {}с)",
                        added.log_entry.kind, added.log_entry.delta_s, added.log_entry.total_s
                    ),
                );
                vec![]
            }
            names::EVT_CRIME_REMOVED => {
                let Ok(removed) = serde_json::from_value::<CrimeRemoved>(payload) else {
                    return vec![];
                };
                if let Some(snap) = self.snapshot.as_mut() {
                    snap.char.crime.statuses.retain(|s| s.kind != removed.kind);
                }
                self.push_log(LogKind::Crime, format!("Статус «{}» снят", removed.kind));
                vec![]
            }
            _ => vec![],
        }
    }
}
