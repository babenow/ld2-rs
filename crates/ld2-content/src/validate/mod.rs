//! Структурная валидация контента (дубликаты id, висячие выходы).
//!
//! Общая для сервера (сид/hot reload) и редактора: дубликаты id —
//! фатальны (upsert молча перетёр бы записи в БД), висячие выходы —
//! предупреждение (мерж карт может оставлять их временно).

use crate::model::GameContent;
use crate::ContentError;
use ld2_protocol::types::EntityKind;
mod items;
mod npc;
mod refs;
mod report;

pub use items::*;
pub use npc::*;
pub use refs::*;
pub use report::*;

/// Валидация для сервера: `Err(Invalid)` при фатальных проблемах,
/// предупреждения — в лог.
pub fn validate(content: &GameContent) -> Result<(), ContentError> {
    let report = validate_report(content);
    for warning in &report.warnings {
        tracing::warn!("контент (не критично): {warning}");
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(ContentError::Invalid(report.errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentDialogs, ContentShops};
    use crate::{ContentDialogLine, ContentDialogReply};
    use std::path::Path;

    fn repo_content() -> GameContent {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        crate::load(&dir).expect("контент репозитория загружается")
    }

    /// E5: контент репозитория не имеет висячих ссылок.
    #[test]
    fn repository_references_are_clean() {
        let warnings = validate_references(&repo_content());
        assert!(warnings.is_empty(), "висячие ссылки: {warnings:?}");
    }

    /// E5: битая ссылка в квесте — предупреждение (не ошибка).
    #[test]
    fn broken_quest_references_are_warnings() {
        let mut content = repo_content();
        content.quests[0].giver = Some("npc_no_such".into());
        content.quests[0].goals[0].target = Some("m_no_such".into());
        content.quests[0].reward.rank_up = Some("no_profession".into());
        let warnings = validate_references(&content);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("npc_no_such")));
        assert!(warnings.iter().any(|w| w.contains("m_no_such")));
        assert!(warnings.iter().any(|w| w.contains("no_profession")));
    }

    /// E5: ингредиент по `type` (flask) и по `id` (herb) — оба валидны;
    /// неизвестный предмет — предупреждение.
    #[test]
    fn recipe_inputs_accept_ids_and_types() {
        let mut content = repo_content();
        let index = content
            .recipes
            .iter()
            .position(|recipe| recipe.id == "rec_potion_heal")
            .expect("рецепт");
        content.recipes[index].inputs[0].item_type = "herb".into();
        content.recipes[index].output.item_type = "flask".into();
        assert!(
            validate_references(&content).is_empty(),
            "id и type валидны"
        );

        content.recipes[index].inputs[0].item_type = "no_such_item".into();
        let warnings = validate_references(&content);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no_such_item"));
    }

    fn repo_npc() -> (ContentDialogs, ContentShops, GameContent) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let raw = crate::RawContent::load(&dir).expect("сырой контент");
        let dialogs = raw.typed_dialogs().expect("dialogs.json");
        let shops = raw.typed_shops().expect("shops.json");
        (dialogs, shops, repo_content())
    }

    /// E5.2: диалоги и магазины репозитория проходят ссылочную валидацию.
    #[test]
    fn repository_npc_content_is_clean() {
        let (dialogs, shops, content) = repo_npc();
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert!(report.errors.is_empty(), "ошибки: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "предупреждения: {:?}",
            report.warnings
        );
    }

    /// E5.2: битые ссылки диалогов/магазинов — предупреждения (не ошибки).
    #[test]
    fn broken_npc_references_are_warnings() {
        let (mut dialogs, mut shops, content) = repo_npc();
        dialogs.npcs[0].root = Some("l_missing".into());
        dialogs.npcs[0].lines[0].quest = Some("q_missing".into());
        if let Some(reply) = dialogs.npcs[0].lines[0].replies.first_mut() {
            reply.next = Some("l_missing".into());
        }
        shops.npcs[0].currencies[0]
            .items
            .push("no_such_item".into());
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let warnings = report.warnings.join("; ");
        for needle in ["l_missing", "q_missing", "no_such_item"] {
            assert!(warnings.contains(needle), "нет {needle}: {warnings}");
        }
    }

    /// E5.2: дубликаты строк и реплик в диалоге — фатальные ошибки.
    #[test]
    fn duplicate_dialog_ids_are_errors() {
        let (mut dialogs, shops, content) = repo_npc();
        let line_id = dialogs.npcs[0].lines[0].id.clone();
        let line_text = dialogs.npcs[0].lines[0].text.clone();
        dialogs.npcs[0].lines.push(ContentDialogLine {
            id: line_id,
            text: line_text,
            ..Default::default()
        });
        let reply_id = dialogs.npcs[0].lines[0]
            .replies
            .first()
            .map(|reply| reply.id.clone())
            .expect("у строки есть реплики");
        dialogs.npcs[0].lines[0].replies.push(ContentDialogReply {
            id: reply_id,
            ..Default::default()
        });
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(report.errors.iter().all(|error| error.contains("дубликат")));
    }

    /// E6.2: предметы репозитория проходят структурную валидацию.
    #[test]
    fn repository_items_are_clean() {
        let warnings = validate_items(&repo_content());
        assert!(warnings.is_empty(), "предметы: {warnings:?}");
    }

    /// E6.2: битые бонусы/статы/руны — предупреждения (не ошибки).
    #[test]
    fn broken_item_rules_are_warnings() {
        let mut content = repo_content();
        let axe = content
            .items
            .iter()
            .position(|item| item.id == "drakkar_axe")
            .expect("drakkar_axe");
        content.items[axe].rarity = "mythic".into();
        content.items[axe].bonuses[0].kind = "talent".into();
        content.items[axe].bonuses[1].value = 0;
        content.items[axe].bonuses[1].pct = true;
        let rune = content
            .items
            .iter()
            .position(|item| item.item_type == "rune")
            .expect("руна");
        content.items[rune].stats.remove("glyph");
        content.items[rune].stats.insert(
            "effect".into(),
            serde_json::json!({"kind": "burn", "duration_s": 6}),
        );
        let warnings = validate_items(&content);
        let joined = warnings.join("; ");
        for needle in ["mythic", "talent", "0", "glyph", "burn"] {
            assert!(joined.contains(needle), "нет {needle}: {joined}");
        }
    }
}
