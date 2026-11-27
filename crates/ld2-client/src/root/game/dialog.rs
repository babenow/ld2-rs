//! Модалки: диалоги NPC, смерть, осмотр, лут (RF3m).

use super::*;

const MS_PER_SECOND: i64 = 1000;
const SECONDS_PER_MINUTE: u64 = 60;

impl Root {
    /// M11.2: игровые всплывающие диалоги (ФР-03/05/06): death/examine/loot.
    /// Приоритет — прежний диспетчер замены контента (смерть главнее).
    pub(crate) fn game_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(death) = self.store.death.clone() {
            let title = t(self.lang, Key::DeathTitle).to_string();
            let body = self.death_view(cx, pal, &death).into_any_element();
            // ФР-06: закрытие только подтверждением (акр сервера) — Escape и
            // клик по скриму ничего не делают: on_close пуст.
            return Some(
                Dialog::new("ld2-death", title, |_window, _cx| {})
                    .child(body)
                    .into_any_element(),
            );
        }
        // M25: стан — всплывающее окно с прогрессбаром времени
        // (закрывается само по истечении — tick чистит store.stun).
        if let Some(stun) = self.store.stun.clone() {
            let total = (stun.until_ms - stun.started_ms).max(1);
            let left = (stun.until_ms - self.now_ms()).max(0);
            let frac = (1.0 - left as f32 / total as f32).clamp(0.0, 1.0);
            let body = div()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(240.0))
                        .h(px(10.0))
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(frac))
                                .rounded_md()
                                .bg(rgb(pal.danger)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_SM))
                        .text_color(rgb(pal.muted))
                        .child(format_duration(self.lang, left / MS_PER_SECOND)),
                )
                .into_any_element();
            return Some(
                Dialog::new("ld2-stun", stun.title.clone(), |_window, _cx| {})
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(detail) = self.store.entity_detail.clone() {
            let name = detail["entity"]["name"].as_str().unwrap_or("?").to_string();
            let body = self.examine_view(cx, pal, detail).into_any_element();
            let close = self.dismiss(cx, |r| r.store.entity_detail = None);
            return Some(
                Dialog::new("ld2-examine", name, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some((uid, name)) = self.store.loot_confirm.clone() {
            let title = t(self.lang, Key::TakeLoot).to_string();
            let body = self.loot_view(cx, pal, uid, name).into_any_element();
            let close = self.dismiss(cx, |r| r.store.clear_loot_confirm());
            return Some(
                Dialog::new("ld2-loot", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        // M18/F38: диалог NPC — попап-окно (см. npc_window), не модалка.
        None
    }

    /// M18/F38: **окно диалога NPC как окно персонажа** — попап-шелл
    /// (`popup_window`: drag/resize, общий scrim, Esc/×; порядок слоёв —
    /// верхний в группе). Гейт — наличие `store.npc_dialog`; закрытие
    /// (Esc/scrim/×/терминальный ответ) чистит состояние в store.
    pub(crate) fn npc_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        // M18/F46: окно закрыто — сбрасываем длину ленты, чтобы при
        // следующем открытии stick-to-bottom отработал заново.
        let Some(dialog) = self.store.npc_dialog.clone() else {
            self.npc_seen = 0;
            return None;
        };
        let lang = self.lang;
        // M18/F39/F44: открытый магазин или панель обучения заменяет
        // транскрипт (иконка «назад» возвращает к диалогу).
        let body = if dialog.shop.is_some() {
            self.shop_view(cx, pal, &dialog)
        } else if dialog.training_open {
            self.training_view(cx, pal, &dialog)
        } else if dialog.history_open {
            // M22: полная лента — по кнопке «История» в статусбаре.
            self.dialog_history_view(pal, &dialog)
        } else {
            self.npc_dialog_view(cx, pal, &dialog)
        };

        // M18/F41: контекстные кнопки диалога — иконки слева в шапке
        // окна (статус-бар убран; «Квесты»/«Обучение» убраны по решению
        // пользователя). Тултипы — тексты l10n, что и были у кнопок.
        let mut leading = div().flex().flex_row().gap_1().items_center();
        // M22: история диалога — кнопка в статусбаре окна; в самом
        // диалоге показывается только текущая реплика.
        leading = leading.child(
            IconButton::new("ld2-npc-history", IconName::BookOpen)
                .tooltip(t(lang, Key::DialogHistory))
                .on_click(cx.listener(|root, _, _, cx| {
                    if let Some(view) = root.store.npc_dialog.as_mut() {
                        view.history_open = !view.history_open;
                    }
                    cx.notify();
                })),
        );
        if dialog.shop.is_some() || dialog.training_open {
            // Открыт магазин/обучение — возврат к транскрипту (иконка
            // «назад»); чистим обе панели (M18/F44).
            leading = leading.child(
                IconButton::new("ld2-npc-back", IconName::ArrowLeft)
                    .tooltip(t(lang, Key::ShopBack))
                    .on_click(cx.listener(|root, _, _, cx| {
                        if let Some(view) = root.store.npc_dialog.as_mut() {
                            view.shop = None;
                            view.shop_currency = None;
                            view.training_open = false;
                            view.history_open = false;
                        }
                        cx.notify();
                    })),
            );
        } else if dialog.actions.iter().any(|action| action == "shop") {
            let entity_id = dialog.id.clone();
            leading = leading.child(
                IconButton::new("ld2-npc-shop", IconName::DollarSign)
                    .tooltip(t(lang, Key::BtnShop))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        let cmd = ld2_protocol::cmd::ShopList {
                            entity_id: entity_id.clone(),
                        };
                        root.send_cmd(&cmd, Act::ShopList);
                        cx.notify();
                    })),
            );
        }

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Npc,
            dialog.name.clone(),
            Some(leading),
            body,
            None,
        ))
    }

    /// M18/F35: окно диалога NPC: приветствие, реплики (текст готов —
    /// `{name}` подставляет сервер) и секция «Квесты» с кнопкой
    /// «Принять» (`cmd/quest.accept`; принятый исчезает по
    /// `evt/quest/updated` — store чистит `npc_dialog.quests`).
    pub(crate) fn npc_dialog_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &NpcDialogView,
    ) -> Div {
        let lang = self.lang;
        let now_ms = self.now_ms();

        // M22: верхняя строка (30 %) — только ТЕКУЩАЯ реплика
        // (полная лента — кнопка «История» в статусбаре); контент
        // длиннее рамки — прокрутка.
        let top = div()
            .flex()
            .flex_col()
            .gap_1()
            .min_h_0()
            .child(match dialog.transcript.last() {
                Some(line) => {
                    let text = if line.me {
                        format!("{}: {}", t(lang, Key::DialogYou), line.text)
                    } else {
                        line.text.clone()
                    };
                    div()
                        .text_size(px(crate::ui_tokens::FONT_XL))
                        .font_weight(if line.me {
                            gpui::FontWeight::BOLD
                        } else {
                            gpui::FontWeight::NORMAL
                        })
                        .text_color(rgb(if line.me { pal.accent } else { pal.text }))
                        .child(text)
                }
                None => div(),
            });

        // ── Зона 2: реплики персонажа и предложения NPC (внизу) ──
        let actions = div().flex().flex_col().gap_1();
        let actions = self.npc_reply_buttons(actions, cx, pal, lang, dialog, now_ms);
        let actions = self.npc_quest_cards(actions, cx, pal, lang, dialog, now_ms);

        // M22: две горизонтальные строки —30 % текущая реплика
        // сверху,70 % реплики персонажа/контент внизу; каждая с
        // прокруткой при выходе за рамки (указание заказчика).
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .child(
                div().h(gpui::relative(0.3)).min_h_0().child(
                    ScrollArea::new("ld2-npc-current")
                        .vertical()
                        .flex_grow_1()
                        .min_h_0()
                        .child(top),
                ),
            )
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(
                div().h(gpui::relative(0.7)).min_h_0().child(
                    ScrollArea::new("ld2-npc-replies")
                        .vertical()
                        .flex_grow_1()
                        .min_h_0()
                        .child(actions),
                ),
            )
    }

    /// M22: история диалога — полная лента (кнопка «История» в
    /// статусбаре окна); stick-to-bottom как была у ленты.
    pub(crate) fn dialog_history_view(&mut self, pal: &Palette, dialog: &NpcDialogView) -> Div {
        let lang = self.lang;
        let len = dialog.transcript.len();
        if len != self.npc_seen {
            self.npc_seen = len;
            if len > 0 {
                self.npc_scroll.scroll_to_bottom();
            }
        }
        let mut history = div().flex().flex_col().gap_2().justify_start();
        for line in &dialog.transcript {
            let text = if line.me {
                format!("{}: {}", t(lang, Key::DialogYou), line.text)
            } else {
                line.text.clone()
            };
            history = history.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_XL))
                    .font_weight(if line.me {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(rgb(if line.me { pal.accent } else { pal.text }))
                    .child(text),
            );
        }
        div().flex().flex_col().gap_2().size_full().child(
            ScrollArea::new("ld2-npc-history")
                .vertical()
                .flex_grow_1()
                .min_h_0()
                .child(history),
        )
    }

    /// M18/F44: панель обучения внутри окна диалога (паритет магазина,
    /// ADR-009): список навыков тренера с ценой; клик шлёт
    /// `cmd/skill.train` — если обучение невозможно, причину сообщает
    /// сервер (`E_SKILL_NOT_READY`, `E_XP_POOL`, `E_INGREDIENTS`…) тостом.
    pub(crate) fn training_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &NpcDialogView,
    ) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_1().size_full();
        col = col.child(
            div()
                .text_size(px(crate::ui_tokens::FONT_SM))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::BtnTraining)),
        );
        for entry in &dialog.training {
            let skill = self
                .store
                .snapshot
                .as_ref()
                .and_then(|snap| snap.char.skills.iter().find(|s| s.id == entry.skill))
                .cloned();
            let (name, scale) = match &skill {
                Some(skill) => (
                    skill.name.clone().unwrap_or_else(|| skill.id.clone()),
                    format!("{}/{}", skill.xp, skill.xp_to_next),
                ),
                None => (entry.skill.clone(), "—".to_string()),
            };
            // Плата: очки опыта (обязательно) + золото + предметы.
            let mut cost = t(lang, Key::TrainingPoints).replace("{n}", &entry.points.to_string());
            if entry.gold > 0 {
                cost.push_str(" · ");
                cost.push_str(&t(lang, Key::TrainingGold).replace("{n}", &entry.gold.to_string()));
            }
            for item in &entry.items {
                cost.push_str(&format!(" · {} ×{}", item.def_id, item.qty));
            }
            let entity_id = dialog.id.clone();
            let skill_id = entry.skill.clone();
            // Кнопка активна всегда — почему нельзя, отвечает сервер.
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(
                        div()
                            .flex_grow_1()
                            .min_w_0()
                            .child(format!("{name} · {scale}")),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_tokens::FONT_XS))
                            .text_color(rgb(pal.muted))
                            .child(cost),
                    )
                    .child(act_btn(
                        cx,
                        t(lang, Key::BtnTrain),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = ld2_protocol::cmd::SkillTrain {
                                entity_id: entity_id.clone(),
                                skill_id: skill_id.clone(),
                            };
                            this.send_cmd(&cmd, Act::Invest);
                            let _ = cx;
                        },
                    )),
            );
        }
        // M21: профессии этого NPC (изучение — cmd/craft.learn); уже
        // изученные не показываем — снапшот обновляется по ack.
        let learned: Vec<String> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.professions.iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default();
        let teachable: Vec<&NpcProfession> = dialog
            .professions
            .iter()
            .filter(|p| !learned.contains(&p.id))
            .collect();
        if !teachable.is_empty() {
            col = col.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ProfessionsTab)),
            );
            for entry in teachable {
                let name = crate::root::professions::prof_label(lang, &entry.id);
                let cost = t(lang, Key::TrainingGold).replace("{n}", &entry.gold.to_string());
                let profession_id = entry.id.clone();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().flex_grow_1().min_w_0().child(name))
                        .child(
                            div()
                                .text_size(px(crate::ui_tokens::FONT_XS))
                                .text_color(rgb(pal.muted))
                                .child(cost),
                        )
                        .child(act_btn(
                            cx,
                            t(lang, Key::LearnLabel),
                            pal.accent,
                            pal.accent_text,
                            move |this, cx| {
                                let cmd = ld2_protocol::cmd::CraftLearn {
                                    profession_id: profession_id.clone(),
                                };
                                this.send_cmd(&cmd, Act::Learn);
                                let _ = cx;
                            },
                        )),
                );
            }
        }
        // «К диалогу» — внизу слева (паритет панели магазина F41).
        col = col.child(div().flex_grow_1().min_h_0());
        col = col.child(
            div()
                .flex()
                .flex_row()
                .justify_start()
                .pt_1()
                .child(act_btn_hover(
                    cx,
                    t(lang, Key::ShopBack),
                    pal.panel,
                    pal.selection,
                    pal.text,
                    |this, cx| {
                        if let Some(view) = this.store.npc_dialog.as_mut() {
                            view.training_open = false;
                        }
                        let _ = cx;
                    },
                )),
        );
        // M22: прокрутка контента панели обучения (навыки +
        // профессии) — кнопка «К диалогу» остаётся под скроллом.
        scrollable_tab("ld2-training-scroll", &self.training_scroll, col)
    }

    // ── модалки (паритет egui-окон) ───────────────────────────────────────

    pub(crate) fn death_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        death: &ld2_protocol::evt::CharDeath,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::DeathLost)),
            )
            // M18: список утраченного со скроллом (много предметов).
            .child(
                ScrollArea::new("ld2-death-scroll")
                    .vertical()
                    .max_h(px(300.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(death.lost_items.iter().map(|item| {
                                // M16/F29e: редкость — на языке игрока.
                                let rarity = item
                                    .rarity
                                    .as_deref()
                                    .map(|r| rarity_label(lang, r))
                                    .unwrap_or_else(|| "-".to_string());
                                div().child(format!("• {} ({})", item.name, rarity))
                            })),
                    ),
            )
            .child(
                btn(
                    cx,
                    t(lang, Key::DeathConfirm).to_string(),
                    pal.danger,
                    0xffffff,
                    Click::ConfirmDeath,
                )
                .mt(px(crate::ui_tokens::SPACE_10)),
            )
    }

    pub(crate) fn examine_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        let lang = self.lang;
        let kind = detail["entity"]["kind"].as_str().unwrap_or("?").to_string();
        let was_player = detail["was_player"] == true;
        let looted = detail["looted"] == true;
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_color(rgb(pal.muted)).child(kind));
        if let Some(hp) = detail["entity"]["hp"].as_object() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("HP: {}/{}", hp["cur"], hp["max"])),
            );
        }
        if was_player {
            col = col.child(
                div()
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::CorpsePlayerHint)),
            );
        }
        if let Some(loot) = detail["loot"].as_array() {
            if !loot.is_empty() {
                col = col.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::LootLabel)),
                );
                for item in loot {
                    let uid = item["uid"].as_str().unwrap_or_default().to_string();
                    let iname = item["name"].as_str().unwrap_or("?").to_string();
                    let itype = item["type"].as_str().unwrap_or("?");
                    let mut line = div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(format!("• {iname} ({itype})"));
                    if !looted {
                        let uid2 = uid.clone();
                        let iname2 = iname.clone();
                        // Лут трупа ИГРОКА — подтверждение о статусе (ФР-11).
                        let player_corpse = was_player;
                        line = line.child(btn(
                            cx,
                            t(lang, Key::TakeLoot).to_string(),
                            pal.panel,
                            pal.accent,
                            if player_corpse {
                                Click::LootTakeMarker(uid2, iname2)
                            } else {
                                Click::UseItem(uid2)
                            },
                        ));
                    }
                    col = col.child(line);
                }
            }
        }
        // Снятие шкур (M22): лут освежевания — отдельный блок
        // «Внутри» (мясо/потроха), взятие как у обычного лута.
        if let Some(inner) = detail["inner"].as_array() {
            if !inner.is_empty() {
                col = col.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::InnerLabel)),
                );
                for item in inner {
                    let uid = item["uid"].as_str().unwrap_or_default().to_string();
                    let iname = item["name"].as_str().unwrap_or("?").to_string();
                    let itype = item["type"].as_str().unwrap_or("?");
                    let color = crate::root::sheet::quality_color(item["def_id"].as_str())
                        .map(rgb)
                        .unwrap_or_else(|| rgb(pal.text));
                    let mut line = div().flex().flex_row().gap_2().items_center().child(
                        div()
                            .text_color(color)
                            .child(format!("• {iname} ({itype})")),
                    );
                    if !looted {
                        let uid2 = uid.clone();
                        line = line.child(btn(
                            cx,
                            t(lang, Key::TakeLoot).to_string(),
                            pal.panel,
                            pal.accent,
                            Click::UseItem(uid2),
                        ));
                    }
                    col = col.child(line);
                }
            }
        }
        if let Some(err) = self.store.world_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err));
        }
        col.child(btn(
            cx,
            t(lang, Key::Exit).to_string(),
            pal.panel,
            pal.text,
            Click::CloseDetail,
        ))
    }

    pub(crate) fn loot_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        _uid: String,
        name: String,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::LootPlayerWarn)),
            )
            .child(name)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .mt(px(crate::ui_tokens::SPACE_8))
                    .child(btn(
                        cx,
                        t(lang, Key::Exit).to_string(),
                        pal.panel,
                        pal.text,
                        Click::LootCancel,
                    ))
                    .child(btn(
                        cx,
                        t(lang, Key::TakeLoot).to_string(),
                        pal.danger,
                        0xffffff,
                        Click::LootTake,
                    )),
            )
    }

    /// Ответы текущего узла диалога — шаги ветки (RF3m: часть
    /// `npc_dialog_view`).
    fn npc_reply_buttons(
        &mut self,
        mut actions: gpui::Div,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        _lang: ld2_core::l10n::Lang,
        dialog: &NpcDialogView,
        now_ms: i64,
    ) -> gpui::Div {
        // Ответы текущего узла — шаги ветки (моя реплика в ленту сразу,
        // сервер отвечает новым узлом; без ответов диалог закончен).
        if let Some(node) = &dialog.node {
            for reply in &node.replies {
                // M18/F44: реплика-действие — открываем панель обучения
                // (паритет магазина), npc.reply не шлётся.
                if reply.action.as_deref() == Some("train") {
                    actions = actions.child(act_btn_hover(
                        cx,
                        reply.text.clone(),
                        pal.panel,
                        pal.selection,
                        pal.text,
                        |this, cx| {
                            if let Some(view) = this.store.npc_dialog.as_mut() {
                                view.training_open = true;
                            }
                            let _ = cx;
                        },
                    ));
                    continue;
                }
                // M18/F43: реплика-действие — открываем магазин вместо
                // перехода по ветке (тот же флоу, что иконка в шапке).
                if reply.action.as_deref() == Some("shop") {
                    let entity_id = dialog.id.clone();
                    actions = actions.child(act_btn_hover(
                        cx,
                        reply.text.clone(),
                        pal.panel,
                        pal.selection,
                        pal.text,
                        move |this, cx| {
                            let cmd = ld2_protocol::cmd::ShopList {
                                entity_id: entity_id.clone(),
                            };
                            this.send_cmd(&cmd, Act::ShopList);
                            let _ = cx;
                        },
                    ));
                    continue;
                }
                // M18/F42: живой отсчёт таймера (ADR-008) — остаток по
                // абсолютному дедлайну; ноль → окно истекло, реплика
                // снова активна.
                let wait_left = reply
                    .wait_s
                    .map(|wait_s| {
                        reply
                            .wait_until_ms
                            .map(|until| ((until - now_ms).max(0) / MS_PER_SECOND) as u64)
                            .unwrap_or(wait_s)
                    })
                    .filter(|left| *left > 0);
                if let Some(wait_left) = wait_left {
                    actions = actions.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(pal.panel))
                            .text_size(px(crate::ui_tokens::FONT_MD))
                            .text_color(rgb(pal.muted))
                            .child(format!(
                                "{} ({:02}:{:02})",
                                reply.text,
                                wait_left / SECONDS_PER_MINUTE,
                                wait_left % SECONDS_PER_MINUTE
                            )),
                    );
                    continue;
                }
                // M18/F46: дефис перед репликой убран.
                let label = reply.text.clone();
                let entity_id = dialog.id.clone();
                let node_id = node.id.clone();
                let reply_id = reply.id.clone();
                let text = reply.text.clone();
                // M18/F38: реплики персонажа подсвечиваются при наведении.
                actions = actions.child(act_btn_hover(
                    cx,
                    label,
                    pal.panel,
                    pal.selection,
                    pal.text,
                    move |this, cx| {
                        if let Some(view) = this.store.npc_dialog.as_mut() {
                            view.transcript.push(ld2_core::store::NpcTranscriptLine {
                                me: true,
                                text: text.clone(),
                            });
                        }
                        let cmd = ld2_protocol::cmd::NpcReply {
                            entity_id: entity_id.clone(),
                            node_id: node_id.clone(),
                            reply_id: reply_id.clone(),
                        };
                        this.send_cmd(&cmd, Act::NpcReply);
                        let _ = cx;
                    },
                ));
            }
        }
        actions
    }

    /// Карточки квестов NPC в диалоге (RF3m: часть `npc_dialog_view`).
    fn npc_quest_cards(
        &mut self,
        mut actions: gpui::Div,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        lang: ld2_core::l10n::Lang,
        dialog: &NpcDialogView,
        _now_ms: i64,
    ) -> gpui::Div {
        if !dialog.quests.is_empty() {
            actions = actions.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::QuestsTab)),
            );
            for quest in &dialog.quests {
                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .border_1()
                    .border_color(rgb(pal.border))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(quest.title.clone().unwrap_or_else(|| quest.id.clone())),
                    );
                if !quest.goals.is_empty() {
                    card = card.child(
                        div()
                            .text_size(px(crate::ui_tokens::FONT_SM))
                            .text_color(rgb(pal.muted))
                            .child(format!("• {}", goals_summary(quest))),
                    );
                }
                if let Some(reward) = &quest.reward_preview {
                    if let Some(xp) = reward.xp {
                        card = card.child(
                            div()
                                .text_size(px(crate::ui_tokens::FONT_SM))
                                .text_color(rgb(pal.muted))
                                .child(format!("{}: {xp} XP", t(lang, Key::RewardLabel))),
                        );
                    }
                }
                let qid = quest.id.clone();
                card = card.child(act_btn(
                    cx,
                    t(lang, Key::BtnAccept),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = ld2_protocol::cmd::QuestAccept {
                            quest_id: qid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Quest);
                        let _ = cx;
                    },
                ));
                actions = actions.child(card);
            }
        }
        actions
    }
}
