//! M18/F36: **окно квестов** — всплывающее окно (паритет рюкзака) с
//! двумя вкладками: «Текущие» (взятые: active/turn_in) и «Выполненные»
//! (done). Слева — список с бейджем состояния и целями, справа —
//! детали выбранного квеста (цели с прогрессом, награда, действия
//! «Сдать»/«Бросить»/«Отслеживать»), внизу — статус-бар: текущие
//! `N / M` (лимит из `SessionConfig.quests.max_active`, §5 — конфиг
//! сервера, не хардкод), выполненные — просто `N`.
//!
//! Вкладка «Квесты» окна персонажа убрана (правило дубликатов):
//! принятие доступных квестов — в диалогах NPC (F35).

use ely::forms::Choice;
use ely::navigation::Tabs;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, tf, Key, Lang};
use ld2_core::store::{Act, Screen};
use ld2_protocol::types::{QuestPhase, QuestState};

use crate::palette::Palette;

use super::{act_btn, scrollable_tab, PopupKind, Root};

/// Вкладки окна квестов (открытие всегда на «Текущих» — паритет окон).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum QuestsTab {
    #[default]
    Current,
    Completed,
}

impl QuestsTab {
    pub(crate) fn all() -> [(QuestsTab, Key); 2] {
        [
            (QuestsTab::Current, Key::QuestsTabCurrent),
            (QuestsTab::Completed, Key::QuestsTabDone),
        ]
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            QuestsTab::Current => "current",
            QuestsTab::Completed => "completed",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "completed" => QuestsTab::Completed,
            _ => QuestsTab::Current,
        }
    }
}

/// M18/F36: в какую вкладку попадает квест: взятые (active/turn_in) —
/// «Текущие», завершённые — «Выполненные»; доступные (не взятые) в
/// окне не показываются — их принимают в диалогах NPC (F35).
pub(crate) fn quest_tab_of(phase: QuestPhase) -> Option<QuestsTab> {
    match phase {
        QuestPhase::Active | QuestPhase::TurnIn => Some(QuestsTab::Current),
        QuestPhase::Done => Some(QuestsTab::Completed),
        QuestPhase::Available => None,
    }
}

/// Бейдж состояния (ключ + цвет) — палитра бывшей вкладки «Квесты».
fn state_badge(phase: QuestPhase) -> (Key, u32) {
    match phase {
        QuestPhase::Available => (Key::StateAvailable, 0x98_c3_79),
        QuestPhase::Active => (Key::StateActive, 0xab_b2_bf),
        QuestPhase::TurnIn => (Key::StateTurnIn, 0xd1_9a_66),
        QuestPhase::Done => (Key::StateDone, 0x61_af_ef),
    }
}

/// Сводка целей квеста: «Волков убито 4/5» или текст цели
/// (переехала из вкладки «Квесты» персонажа, M18/F36; используют
/// это окно и трекер/диалог NPC).
pub(crate) fn goals_summary(q: &QuestState) -> String {
    q.goals
        .iter()
        .map(|g| match (g.cur, g.need) {
            (Some(cur), Some(need)) => format!("{} {cur}/{need}", g.text),
            _ => g.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Текст статус-бара (чистая функция — юнит-тест): текущие — `N / M`
/// (лимит §5), выполненные — просто `N`.
pub(crate) fn quest_status_text(
    lang: Lang,
    tab: QuestsTab,
    cur: usize,
    done: usize,
    max: u32,
) -> String {
    match tab {
        QuestsTab::Current => tf(
            lang,
            Key::QuestsStatusCurrent,
            &[("cur", &cur.to_string()), ("max", &max.to_string())],
        ),
        QuestsTab::Completed => tf(lang, Key::QuestsStatusDone, &[("done", &done.to_string())]),
    }
}

/// Детали квеста в правой колонке: цели с прогрессом, награда
/// (`reward_label` из контента либо XP), действия по фазе.
fn quest_details(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    quest: &QuestState,
    tracked: bool,
) -> Div {
    let (badge_key, badge_color) = state_badge(quest.state);
    let mut col = div().flex().flex_col().gap_2().pr(px(8.0));
    col = col.child(
        div()
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(badge_color))
                    .child(t(lang, badge_key)),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(quest.title.clone().unwrap_or_else(|| quest.id.clone())),
            ),
    );
    for goal in &quest.goals {
        let line = match (goal.cur, goal.need) {
            (Some(cur), Some(need)) => format!("• {} ({cur}/{need})", goal.text),
            _ => format!("• {}", goal.text),
        };
        col = col.child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(pal.text))
                .child(line),
        );
    }
    // Награда: человекочитаемая строка контента (M18/F36), фолбэк — XP.
    if let Some(label) = &quest.reward_label {
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(label.clone()),
        );
    } else if let Some(xp) = quest.reward_preview.as_ref().and_then(|r| r.xp) {
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!("{}: {xp} XP", t(lang, Key::RewardLabel))),
        );
    }

    // Действия (логика перенесена из вкладки «Квесты» персонажа, F36).
    if matches!(quest.state, QuestPhase::Active | QuestPhase::TurnIn) {
        let mut actions = div().flex().flex_row().gap_1().mt(px(4.0));
        if quest.state == QuestPhase::TurnIn {
            let qid = quest.id.clone();
            actions = actions.child(act_btn(
                cx,
                t(lang, Key::BtnTurnIn),
                pal.accent,
                pal.accent_text,
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::QuestTurnIn {
                        quest_id: qid.clone(),
                    };
                    this.send_cmd(&cmd, Act::Quest);
                    let _ = cx;
                },
            ));
        }
        let qid = quest.id.clone();
        actions = actions.child(act_btn(
            cx,
            t(lang, Key::BtnAbandon),
            pal.panel,
            pal.danger,
            move |this, cx| {
                let cmd = ld2_protocol::cmd::QuestAbandon {
                    quest_id: qid.clone(),
                };
                this.send_cmd(&cmd, Act::Quest);
                let _ = cx;
            },
        ));
        let qid = quest.id.clone();
        actions = actions.child(act_btn(
            cx,
            if tracked {
                t(lang, Key::TrackedBtn)
            } else {
                t(lang, Key::TrackBtn)
            },
            if tracked { pal.accent } else { pal.panel },
            pal.text,
            move |this, cx| {
                this.store.toggle_quest_tracker(&qid);
                let _ = cx;
            },
        ));
        col = col.child(actions);
    }
    col
}

impl Root {
    /// M18/F36: окно квестов — вкладки, две колонки, статус-бар.
    pub(crate) fn quests_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.quests_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        let lang = self.lang;
        let tab = self.quests_tab;

        // Вкладки: текущие/выполненные (иконок нет — Choice без иконки).
        let choices: Vec<Choice> = QuestsTab::all()
            .iter()
            .map(|(candidate, key)| Choice::new(candidate.as_str(), t(lang, *key)))
            .collect();
        let this = cx.entity();
        let tabs = Tabs::new("ld2-quests-tabs", choices, tab.as_str()).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.quests_tab = QuestsTab::parse(&value);
                    cx.notify();
                });
            },
        );

        let Some(snap) = self.store.snapshot.clone() else {
            return Some(
                self.popup_window(
                    window,
                    cx,
                    pal,
                    PopupKind::Quests,
                    t(lang, Key::QuestsTab),
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::Loading)),
                    None,
                ),
            );
        };

        // Разбиение по вкладкам + счётчики статус-бара.
        let mut cur = 0usize;
        let mut done = 0usize;
        let mut items: Vec<&QuestState> = Vec::new();
        for quest in &snap.char.quests {
            match quest_tab_of(quest.state) {
                Some(QuestsTab::Current) => {
                    cur += 1;
                    if tab == QuestsTab::Current {
                        items.push(quest);
                    }
                }
                Some(QuestsTab::Completed) => {
                    done += 1;
                    if tab == QuestsTab::Completed {
                        items.push(quest);
                    }
                }
                // Доступные (не взятые) — принимаются в диалогах NPC.
                None => {}
            }
        }
        // Лимит — из конфига сервера (§5; старый сервер → serde-default).
        let max = self
            .store
            .session
            .as_ref()
            .map(|s| s.quests.max_active)
            .unwrap_or(10);

        // Левая колонка: список с выделением по клику.
        let mut list = div().flex().flex_col().gap_1().w(px(300.0));
        if items.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoQuests)),
            );
        }
        for quest in &items {
            let qid = quest.id.clone();
            let selected = self.selected_quest.as_deref() == Some(quest.id.as_str());
            let (badge_key, badge_color) = state_badge(quest.state);
            let mut card = div()
                .id(format!("ld2-quest-{qid}"))
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(if selected { pal.selection } else { pal.panel }))
                .border_1()
                .border_color(rgb(pal.border))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(pal.selection)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_quest = Some(qid.clone());
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(badge_color))
                                .child(t(lang, badge_key)),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(quest.title.clone().unwrap_or_else(|| quest.id.clone())),
                        ),
                );
            if !quest.goals.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", goals_summary(quest))),
                );
            }
            list = list.child(card);
        }

        // Правая колонка: выбранный квест либо первая строка вкладки.
        let effective = self
            .selected_quest
            .as_deref()
            .and_then(|id| items.iter().find(|q| q.id == id))
            .copied()
            .or_else(|| items.first().copied());
        let tracked = &self.store.quest_tracker;
        let right = match effective {
            Some(quest) => quest_details(cx, pal, lang, quest, tracked.contains(&quest.id)),
            None => div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::QuestsPick)),
        };

        let body = div()
            .flex()
            .flex_row()
            .gap_3()
            .child(list)
            .child(div().w(px(1.0)).self_stretch().bg(rgb(pal.border)))
            .child(div().flex().flex_col().flex_grow_1().min_w_0().child(right));

        // Тело в скролле (длинные списки целей/квестов) под вкладками.
        let content = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_h_0()
            .child(tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(scrollable_tab(
                "ld2-quests-scroll",
                &self.quests_scroll,
                body,
            ));

        // Статус-бар: текущие — N / M (лимит §5), выполненные — N.
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(pal.selection))
            .border_t_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.text))
                    .child(quest_status_text(lang, tab, cur, done, max)),
            );

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Quests,
            t(lang, Key::QuestsTab),
            content,
            Some(footer),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F36: классификация по вкладкам — взятые/сданные/доступные.
    #[test]
    fn quest_tabs_split_by_phase() {
        assert_eq!(quest_tab_of(QuestPhase::Active), Some(QuestsTab::Current));
        assert_eq!(quest_tab_of(QuestPhase::TurnIn), Some(QuestsTab::Current));
        assert_eq!(quest_tab_of(QuestPhase::Done), Some(QuestsTab::Completed));
        assert_eq!(
            quest_tab_of(QuestPhase::Available),
            None,
            "недоступные не показываются — приём в диалогах NPC (F35)"
        );
        assert_eq!(QuestsTab::parse("completed"), QuestsTab::Completed);
        assert_eq!(QuestsTab::parse("current"), QuestsTab::Current);
        assert_eq!(QuestsTab::parse(""), QuestsTab::Current);
    }

    /// M18/F36: статус-бар — текущие N / M (лимит §5), выполненные N.
    #[test]
    fn status_bar_counts_per_tab() {
        let cur = quest_status_text(Lang::Ru, QuestsTab::Current, 2, 5, 10);
        assert!(cur.contains('2') && cur.contains("10"), "ru: {cur}");
        let done = quest_status_text(Lang::Ru, QuestsTab::Completed, 2, 5, 10);
        assert!(done.contains('5'), "счётчик выполненных: {done}");
        assert!(!done.contains("10"), "выполненным лимит не нужен: {done}");
        let en = quest_status_text(Lang::En, QuestsTab::Current, 3, 0, 10);
        assert!(en.contains('3') && en.contains("10"), "en: {en}");
    }
}
