//! Квесты персонажа (ФР-12, M12.6): журнал, принятие/сдача, прогресс целей.

use sqlx::PgPool;

/// Контент-квест `(id, title, giver, goals, reward, reward_label)`.
pub type QuestDefRow = (
    String,
    String,
    Option<String>,
    serde_json::Value,
    serde_json::Value,
    Option<String>,
);

/// Прогресс `(quest_id, state, progress)`; state: active|turn_in|done.
pub type QuestProgressRow = (String, String, serde_json::Value);

/// Все квесты контента.
pub async fn defs(pool: &PgPool) -> Result<Vec<QuestDefRow>, sqlx::Error> {
    sqlx::query_as::<_, QuestDefRow>(
        "SELECT id, title, giver, goals, reward, reward_label FROM quests ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

/// Прогресс всех квестов персонажа.
pub async fn progress(pool: &PgPool, char_id: &str) -> Result<Vec<QuestProgressRow>, sqlx::Error> {
    sqlx::query_as::<_, QuestProgressRow>(
        "SELECT quest_id, state, progress FROM character_quests \
         WHERE character_id = $1 ORDER BY quest_id",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Принять квест (state = active, нулевой прогресс); `false` — уже взят.
pub async fn accept(
    pool: &PgPool,
    char_id: &str,
    quest_id: &str,
    goal_count: usize,
) -> Result<bool, sqlx::Error> {
    let zero: Vec<serde_json::Value> = (0..goal_count)
        .map(|_| serde_json::json!({ "cur": 0 }))
        .collect();
    let result = sqlx::query(
        "INSERT INTO character_quests (character_id, quest_id, state, progress) \
         VALUES ($1, $2, 'active', $3) ON CONFLICT DO NOTHING",
    )
    .bind(char_id)
    .bind(quest_id)
    .bind(serde_json::Value::Array(zero))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Смена состояния (active → turn_in → done); `false` — квеста нет.
pub async fn set_state(
    pool: &PgPool,
    char_id: &str,
    quest_id: &str,
    state: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE character_quests SET state = $3 \
         WHERE character_id = $1 AND quest_id = $2",
    )
    .bind(char_id)
    .bind(quest_id)
    .bind(state)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Записать прогресс целей (JSON-массив `[{cur}]`).
pub async fn set_progress(
    pool: &PgPool,
    char_id: &str,
    quest_id: &str,
    progress: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE character_quests SET progress = $3 \
         WHERE character_id = $1 AND quest_id = $2",
    )
    .bind(char_id)
    .bind(quest_id)
    .bind(progress)
    .execute(pool)
    .await?;
    Ok(())
}

/// Бросить квест (удаление прогресса; контент-квест снова «доступен»).
pub async fn abandon(pool: &PgPool, char_id: &str, quest_id: &str) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM character_quests WHERE character_id = $1 AND quest_id = $2")
            .bind(char_id)
            .bind(quest_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() == 1)
}

/// Собрать `QuestState` (§6) из контента + прогресса персонажа.
/// `progress`/`state` из БД; без строки — квест «доступен».
pub fn build_state(
    def: &QuestDefRow,
    row: Option<&QuestProgressRow>,
) -> ld2_protocol::types::QuestState {
    use ld2_protocol::types::{QuestGoal, QuestPhase, QuestState, RewardPreview};

    let (id, title, _, goals_json, reward_json, _) = def;
    let content_goals: Vec<serde_json::Value> = goals_json.as_array().cloned().unwrap_or_default();
    let progress: Vec<serde_json::Value> = row
        .map(|(_, _, p)| p.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let phase = match row.map(|(_, state, _)| state.as_str()) {
        Some("active") => QuestPhase::Active,
        Some("turn_in") => QuestPhase::TurnIn,
        Some("done") => QuestPhase::Done,
        _ => QuestPhase::Available,
    };
    let goals = content_goals
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let cur = progress.get(i).and_then(|p| p["cur"].as_i64()).unwrap_or(0);
            QuestGoal {
                text: g.get("text").and_then(|t| t.as_str()).unwrap_or("").into(),
                cur: Some(cur),
                need: g.get("need").and_then(|n| n.as_i64()),
            }
        })
        .collect();
    let reward_preview = Some(RewardPreview {
        xp: reward_json.get("xp").and_then(|x| x.as_i64()),
        items: reward_json
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default(),
    });
    QuestState {
        id: id.clone(),
        title: Some(title.clone()),
        state: phase,
        goals,
        reward_preview,
        extra: Default::default(),
    }
}

/// Продвинуть цели типа `kill <target>` на 1; → `(новый прогресс, все
/// цели выполнены)`. Чистая функция (юнит-тесты, ФР-12).
pub fn advance_kill(
    def_goals: &serde_json::Value,
    progress: &serde_json::Value,
    target: &str,
) -> (serde_json::Value, bool) {
    let goals = def_goals.as_array().cloned().unwrap_or_default();
    let mut cur: Vec<i64> = progress
        .as_array()
        .map(|a| a.iter().map(|p| p["cur"].as_i64().unwrap_or(0)).collect())
        .unwrap_or_default();
    cur.resize(goals.len(), 0);
    let mut all_done = !goals.is_empty();
    for (i, g) in goals.iter().enumerate() {
        let is_kill = g.get("type").and_then(|t| t.as_str()) == Some("kill");
        let matches = g.get("target").and_then(|t| t.as_str()) == Some(target);
        let need = g.get("need").and_then(|n| n.as_i64()).unwrap_or(1);
        if is_kill && matches && cur[i] < need {
            cur[i] += 1;
        }
        if cur[i] < need {
            all_done = false;
        }
    }
    let new_progress = serde_json::Value::Array(
        cur.into_iter()
            .map(|c| serde_json::json!({ "cur": c }))
            .collect(),
    );
    (new_progress, all_done)
}
