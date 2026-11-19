//! Игровой экран gpui (M10.4) — порт `ui/world.rs` egui-клиента:
//! левая колонка (цель/ресурсы/история), центр (локация, сущности с
//! контекстным меню, hotbar, журнал), модалки смерти/осмотра/лута.
//!
//! Правая панель-лист (sheet, 7 вкладок) и соц-окна (пати/сделка) — M10.4b/M10.5.
//! Отличия от egui: модалки заменяют контент (шапка остаётся), контекстное
//! меню — инлайн под строкой (вместо попапа по курсору), тултипы не используются.

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, MouseButton, Stateful, Window};
use ld2_core::l10n::{format_duration, t, tf, Key};
use ld2_core::store::{
    crime_kind_title, filter_active_buffs, filter_active_statuses, local_epoch, Act, AppStore,
    LogKind, NpcDialogView, NpcProfession,
};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::{Entity, EntityKind, ItemSnapshot, LocationSnapshot, Severity};

use crate::fa::fa_icon;
use crate::palette::Palette;
use ely::buttons::IconButton;
use ely::forms::Choice;
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip};

use super::quests::goals_summary;
use super::sheet::rarity_label;
use super::social::show_party;
use super::{
    act_btn, act_btn_hover, btn, hex_to_u32, scrollable_tab, Click, LogTab, PopupKind, Root,
};

/// M14/F4: «Мощный удар» открывается владением мечом 5 (skills.md §4;
/// серверный гейт — конфиг `skills.active.power_strike` → `E_REQUIREMENT_ATTR`).
/// Клиентское зеркало гейта: кнопка и хоткей показываются/срабатывают
/// только когда навык открыт.
pub(crate) fn power_strike_open(skills: &[ld2_protocol::types::SkillSnapshot]) -> bool {
    skills
        .iter()
        .find(|s| s.id == "sword")
        .is_some_and(|s| s.rank >= 5)
}

/// M17/F30b: uid надетого посоха — слот `staff` удалён (посох теперь
/// двуручное оружие правой руки), ищем по типу среди надетого.
/// M24: надетая экипировка с вставленными рунами — кнопки каста на
/// панели и хоткей «руна». Раньше брался только посох (`item_type ==
/// "staff"`): руны в мече/броне (ADR-012) кнопки не получали —
/// активная руна «не активировалась» (указание заказчика).
fn equipped_rune_items(snap: &StateSnapshot) -> Vec<&ItemSnapshot> {
    let equipped: std::collections::HashSet<&str> = snap
        .char
        .equip
        .values()
        .flatten()
        .map(String::as_str)
        .collect();
    snap.char
        .inventory
        .iter()
        .filter(|i| equipped.contains(i.uid.as_str()))
        .filter(|i| {
            i.sockets
                .iter()
                .flatten()
                // M24: самоцветы живут в том же поле `rune`, но
                // заклинания не имеют — на панель каста не идут.
                .any(|s| s.rune.as_ref().is_some_and(|r| !r.spell.is_empty()))
        })
        .collect()
}

impl Root {
    /// M14/F4: первая доступная руна посоха (хоткей «руна») — заряды и
    /// мана в порядке, как у первой включённой кнопки рун на панели.
    pub(crate) fn first_rune_click(&self) -> Option<Click> {
        let snap = self.store.snapshot.as_ref()?;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);
        for staff in equipped_rune_items(snap) {
            if let Some(socket) = staff.sockets.iter().flatten().find(|socket| {
                socket.rune.as_ref().is_some_and(|r| {
                    r.charges > 0
                        // M24: самоцвет — не руна, каст не из него.
                        && !r.spell.is_empty()
                        && (rune_cost <= 0 || snap.char.mp.cur >= rune_cost)
                        // M24: класс руны — каст только своему классу.
                        && r.class
                            .as_deref()
                            .is_none_or(|c| snap.char.profile.as_deref() == Some(c))
                })
            }) {
                return Some(Click::RuneCast {
                    staff_uid: staff.uid.clone(),
                    socket_index: socket.index,
                });
            }
        }
        None
    }

    /// M14/F4: первый расходник боя (свиток/зелье) — хоткей «6».
    pub(crate) fn first_consumable_uid(&self) -> Option<String> {
        self.store
            .snapshot
            .as_ref()?
            .char
            .inventory
            .iter()
            .find(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
            .map(|i| i.uid.clone())
    }

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
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format_duration(self.lang, left / 1000)),
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

    /// M18/F39: панель магазина внутри окна диалога: вкладки валют
    /// торговца, баланс по активной валюте, товары с ценами (сервер
    /// считает наценки/скидки), покупка цветом показывает нехватку
    /// средств (сервер всё равно авторитет — `E_NOT_ENOUGH_MONEY`).
    fn shop_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &NpcDialogView,
    ) -> Div {
        let lang = self.lang;
        let Some(shop) = dialog.shop.clone() else {
            return div().size_full();
        };
        let active = dialog
            .shop_currency
            .as_deref()
            .and_then(|currency| shop.tabs.iter().find(|tab| tab.currency == currency))
            .or_else(|| shop.tabs.first());
        let Some(tab) = active else {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ShopEmpty));
        };

        // Вкладки валют торговца.
        let choices: Vec<Choice> = shop
            .tabs
            .iter()
            .map(|tab| Choice::new(tab.currency.as_str(), tab.currency_name.clone()))
            .collect();
        let this = cx.entity();
        let tabs = Tabs::new("ld2-shop-tabs", choices, tab.currency.as_str()).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    if let Some(view) = root.store.npc_dialog.as_mut() {
                        view.shop_currency = Some(value);
                    }
                    cx.notify();
                });
            },
        );

        // Баланс по активной валюте — из снапшота.
        let balance: i64 = self
            .store
            .snapshot
            .as_ref()
            .map(|snap| {
                snap.char
                    .inventory
                    .iter()
                    .filter(|item| item.def_id.as_deref() == Some(tab.currency.as_str()))
                    .map(|item| item.qty.unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);

        // M18/F41: колонка на весь бокс — гибкий отступ прижимает
        // кнопку возврата к нижнему краю панели.
        let mut col = div().flex().flex_col().gap_1().size_full();
        // M18/F41: сверху подпись, что вкладка — это валюта торговца.
        col = col.child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ShopCurrency)),
        );
        col = col.child(tabs);
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(tf(lang, Key::ShopOwned, &[("n", &balance.to_string())])),
        );
        for item in &tab.items {
            let afford = balance >= item.price;
            let mut right = div().flex().flex_row().gap_2().items_center();
            right = right.child(
                div()
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(if afford { pal.text } else { pal.danger }))
                    .child(format!("{}", item.price)),
            );
            let entity_id = dialog.id.clone();
            let currency = tab.currency.clone();
            let def_id = item.def_id.clone();
            right = right.child(act_btn_hover(
                cx,
                t(lang, Key::BtnBuy),
                pal.panel,
                pal.selection,
                if afford { pal.text } else { pal.danger },
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::ShopBuy {
                        entity_id: entity_id.clone(),
                        currency: currency.clone(),
                        def_id: def_id.clone(),
                    };
                    this.send_cmd(&cmd, Act::ShopBuy);
                    let _ = cx;
                },
            ));
            let mut left = div().flex().flex_row().gap_2().items_center();
            left = left.child(div().text_size(px(13.0)).child(item.name.clone()));
            if item.qty > 1 {
                left = left.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("×{}", item.qty)),
                );
            }
            // M18/F40: строка товара — подсветка наведения, клик
            // открывает карточку информации (тот же item_detail, что
            // «Описание» в меню предмета).
            let row_id = format!("ld2-shop-item-{}", item.def_id);
            let detail = item.item.clone();
            col = col.child(
                div()
                    .id(row_id)
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(pal.selection)))
                    .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(snapshot) = &detail {
                            this.store.close_upgrade();
                            this.upgrade_progress = None;
                            this.store.item_detail = serde_json::to_value(snapshot).ok();
                            // M18/F53: описание предмета — карточка-попап.
                            this.open_popup(PopupKind::Item);
                        }
                        cx.notify();
                    }))
                    .child(left)
                    .child(right),
            );
        }
        // M18/F41: «К диалогу» — отступ растягивает колонку, кнопка
        // прижата к низу слева (дублирует иконку в шапке окна).
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
                            view.shop = None;
                            view.shop_currency = None;
                        }
                        let _ = cx;
                    },
                )),
        );
        col
    }

    /// M18/F44: панель обучения внутри окна диалога (паритет магазина,
    /// ADR-009): список навыков тренера с ценой; клик шлёт
    /// `cmd/skill.train` — если обучение невозможно, причину сообщает
    /// сервер (`E_SKILL_NOT_READY`, `E_XP_POOL`, `E_INGREDIENTS`…) тостом.
    fn training_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        dialog: &NpcDialogView,
    ) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_1().size_full();
        col = col.child(
            div()
                .text_size(px(12.0))
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
                            .text_size(px(11.0))
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ProfessionsTab)),
            );
            for entry in teachable {
                let name = super::professions::prof_label(lang, &entry.id);
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
                                .text_size(px(11.0))
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

    pub(crate) fn game_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(t(self.lang, Key::Loading));
        };

        div()
            .flex()
            .flex_row()
            .gap_3()
            .size_full()
            .p(px(12.0))
            // M14/F18: резиновая вёрстка — min_w_0 позволяет колонкам
            // сжиматься ниже min-content (иначе правая колонка
            // вылезала за рамки окна); min_w(360) у листа остаётся
            // жёстким полом на минимуме окна (960×640).
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    // M25: левая колонка — 20 % ширины экрана
                    // (резиновая; пол 240 — не схлопывается на узких
                    // окнах), центр — гибкий, правая —20 %.
                    .w(gpui::relative(0.2))
                    .min_w(px(240.0))
                    .child(self.left_column(cx, pal, &snap)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow_1()
                    .min_w_0()
                    // Не выше остатка экрана: журнальный блок сжимается
                    // до своего min_h, а не выдавливает колонку наружу.
                    .min_h_0()
                    // M14/F2: описание локации — всегда первая строка.
                    .child(self.location_block(pal, &snap.location))
                    // Таргет — под описанием (перенос из левой колонки).
                    .child(self.target_block(cx, pal, &snap))
                    .children(self.notice_line(pal))
                    .child(self.entities_block(cx, pal, &snap.location))
                    .child(self.hotbar(cx, pal, &snap))
                    .child(self.exits_row(cx, pal, &snap.location))
                    .children((self.store.target.is_some()).then(|| {
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(self.lang, Key::AltNavHint))
                    }))
                    // Журнал/чат — последним (M14/F2/F7).
                    // M25: журнал приклеен к низу центра (заказчик).
                    .child(div().flex_grow_1().min_h_0())
                    .child(self.journal_block(cx, pal)),
            )
            // M25: правая колонка —20 %: история посещений (верх)
            // и трекер квестов (указание заказчика).
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .min_h_0()
                    .w(gpui::relative(0.2))
                    .min_w(px(180.0))
                    .child(self.history_block(pal))
                    .children(self.tracker_block(pal)),
            )
    }

    // ── левая колонка: цель, ресурсы, история ─────────────────────────────

    fn left_column(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        // M24: ширина задаётся контейнером в game_screen (30 %,
        // резиновая); колонка тянется на всю его ширину.
        let mut col = div().flex().flex_col().gap_2().w_full();

        // Имя + бейджи статусов (ФР-11): серверные сроки, now = лок + offset.
        let mut name_row = div().flex().flex_row().items_center().gap_2().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(16.0))
                .child(snap.char.name.clone()),
        );
        let now = local_epoch() + self.store.time_offset;
        for status in filter_active_statuses(&snap.char.crime.statuses, now) {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            name_row = name_row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(severity_rgb(status.severity)))
                    .child(format!("{title} · {}", format_duration(lang, left))),
            );
        }
        col = col.child(name_row);

        // Ресурсы (ФР-04/09).
        // M25: шкала ХП — виджет stat_bar (выше/шире — параметры,
        // цифры поверх бара, зелёная заливка).
        col = col.child(stat_bar(
            pal.success,
            snap.char.hp.max,
            snap.char.hp.cur,
            t(lang, Key::HpLabel),
            BAR_WIDTH,
            16.0,
            pal,
        ));
        // M18: ресурс по профилю — адреналин (красный), энергия
        // (тёмно-жёлтый), мана (акцентный); длина бара — общая
        // константа со шкалой здоровья.
        let (resource_label, resource_color) = match snap.char.profile.as_deref() {
            Some("warrior") => (t(lang, Key::RageLabel), 0xd1_3b_3b),
            Some("archer") => (t(lang, Key::EnergyLabel), 0xb8_96_1e),
            _ => (t(lang, Key::MpLabel), pal.accent),
        };
        // M25: шкала ресурса — тот же виджет (цвет по профилю).
        col = col.child(stat_bar(
            resource_color,
            snap.char.mp.max,
            snap.char.mp.cur,
            resource_label,
            BAR_WIDTH,
            16.0,
            pal,
        ));
        // M24: индикаторы вставленных рун (указание заказчика) —
        // под индикатором ресурса: глиф · прогрессбар заполнения ·
        // N/M (правее глифа бар, правее бара — заряды).
        for row in self.rune_indicator_rows(snap, pal) {
            col = col.child(row);
        }
        // M24: индикаторы бафов рун (указание заказчика): активные
        // баффы рун; за3 с до конца — моргание раз в секунду.
        for row in self.rune_buff_rows(snap, pal) {
            col = col.child(row);
        }
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!(
                    "{}: {}",
                    t(lang, Key::XpPoolLabel),
                    snap.char.xp_pool
                )),
        );
        // M24: единый прогресс-бар (разделка/инкрустация/заточка) —
        // под панелью персонажа (ХП/ресурс; плавность — быстрый тик).
        col = col.when_some(self.progress_bar(pal), |d, b| d.child(b));

        // M14/F1: «Сменить персонажа»/«Выйти» переехали в меню шапки.

        // M14/F2: окно группы — только при участниках ≥2 (инвайт сам
        // создаёт группу — F5, но до принятия она пустая).
        if self
            .store
            .party
            .as_ref()
            .is_some_and(|p| show_party(&p.members))
        {
            col = col.child(self.party_widget(cx, pal));
        }

        // M14/F7: чат переехал в блок журнала (вкладка «Чат»);
        // история посещений — M25 в правой колонке.
        col
    }

    /// M25: история посещений (ФР-03) — правая колонка20 % (верх);
    /// растягивается на остаток высоты и срезается по ней
    /// (overflow_hidden) — на минимуме окна не выталкивает трекер.
    fn history_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_2().min_h_0();
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(13.0))
                .child(t(lang, Key::HistoryLabel)),
        );
        let mut visited_list = div().flex().flex_col().gap_1();
        for visited in self.store.visited.iter().rev().take(10) {
            visited_list = visited_list.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(visited.name.clone()),
            );
        }
        col = col.child(
            div()
                .flex_grow_1()
                .min_h(px(32.0))
                .overflow_hidden()
                .child(visited_list),
        );
        col
    }

    /// Блок таргета (M14/F2: перенесён из левой колонки — под описание
    /// локации): имя, HP и кнопки «Атака»/«Побег».
    fn target_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        let Some(target_id) = self.store.target.clone() else {
            return div();
        };
        let Some(entity) = snap.location.entities.iter().find(|e| e.id == target_id) else {
            return div();
        };
        let name = entity.name.clone();
        let id = entity.id.clone();
        let hp = entity.hp;
        let mut block = div()
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
                    .child(format!("{}: {name}", t(lang, Key::TargetLabel))),
            );
        if let Some(hp) = hp {
            block = block.child(bar(
                hp.cur as f32 / hp.max.max(1) as f32,
                format!("{}/{}", hp.cur, hp.max),
                pal.danger,
                pal,
            ));
        }
        block = block.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(btn(
                    cx,
                    t(lang, Key::Attack).to_string(),
                    pal.danger,
                    0xffffff,
                    Click::Attack(id.clone()),
                ))
                .child(btn(
                    cx,
                    t(lang, Key::BtnFlee).to_string(),
                    pal.panel,
                    pal.text,
                    Click::Flee,
                )),
        );
        block
    }

    // ── центр: локация, выходы, сущности ──────────────────────────────────

    fn location_block(&mut self, pal: &Palette, loc: &LocationSnapshot) -> Div {
        // M14/F15: название локации — зелёное, если безопасная,
        // красное, если небезопасная (pvp) — как в шапке.
        let pvp = loc.flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
        let name_color = if pvp { pal.danger } else { pal.success };
        let mut head = div().flex().flex_row().gap_2().items_center().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(16.0))
                .text_color(rgb(name_color))
                .child(loc.name.clone()),
        );
        for flag in &loc.flags {
            let (text, color) = match flag.as_str() {
                "pvp" => ("PvP", pal.warn),
                "safe" => ("safe", pal.success),
                other => (other, pal.muted),
            };
            head = head.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(color))
                    .child(text.to_string()),
            );
        }
        head.child(
            div()
                .id("location_text")
                .flex_grow_1()
                .h(px(110.0))
                .overflow_y_scroll()
                .px_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.text))
                        .child(loc.text.clone()),
                ),
        )
    }

    /// Тосты мира (world_error/notice) — как в egui-панели локации.
    fn notice_line(&self, pal: &Palette) -> Option<Div> {
        if let Some(err) = &self.store.world_error {
            return Some(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        if let Some(notice) = &self.store.notice {
            return Some(div().text_color(rgb(pal.warn)).child(notice.clone()));
        }
        None
    }

    fn exits_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let lang = self.lang;
        // M25: вид по образцу заказчика — заголовок, разделитель
        // «---» и вертикальный список; стрелка — направление выхода
        // ИЗ локации (`dir`).
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("{}:", t(lang, Key::ExitsLabel))),
        );
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child("---"),
        );
        if loc.exits.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoExits)),
            );
        }
        for exit in &loc.exits {
            let id = exit.id.clone();
            let arrow = match exit.dir.as_deref() {
                Some("north") => "↑",
                Some("south") => "↓",
                Some("east") => "→",
                Some("west") => "←",
                _ => "",
            };
            let label = if arrow.is_empty() {
                exit.label.clone()
            } else {
                format!("{} {arrow}", exit.label)
            };
            col = col.child(btn(cx, label, pal.panel, pal.accent, Click::MoveExit(id)));
        }
        col
    }

    fn entities_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let safe = loc.flags.iter().any(|f| f == "safe");
        let mut block = div().flex().flex_col().gap_1();
        for (title, kind) in [
            (Key::PlayersLabel, EntityKind::Player),
            (Key::MonstersLabel, EntityKind::Monster),
            (Key::NpcsLabel, EntityKind::Npc),
            (Key::CorpsesLabel, EntityKind::Corpse),
            // M20/F20b: объекты-ресурсы и предметы на земле.
            (Key::ResourcesLabel, EntityKind::Resource),
            (Key::ObjectsLabel, EntityKind::Object),
            // M20/F20c: контейнеры (сундуки) — лут осмотром.
            (Key::ContainersLabel, EntityKind::Container),
        ] {
            let group: Vec<Entity> = loc
                .entities
                .iter()
                .filter(|e| e.kind == kind)
                .cloned()
                .collect();
            if group.is_empty() {
                continue;
            }
            block = block.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(self.lang, title)),
            );
            for entity in group {
                block = block.child(self.entity_row(cx, pal, entity, safe));
            }
        }
        block
    }

    /// Строка сущности: левый клик — цель, ПКМ — действия (ФР-03/05).
    fn entity_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        entity: Entity,
        safe: bool,
    ) -> AnyElement {
        let lang = self.lang;
        let player_corpse = entity.kind == EntityKind::Corpse && entity.was_player.unwrap_or(false);
        let selected = self.store.target.as_deref() == Some(entity.id.as_str());
        let id = entity.id.clone();

        let mut row = div()
            .id(format!("ld2-ent-{}", entity.id))
            .tooltip(Tooltip::text(entity_tooltip(&entity)))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(if selected { pal.selection } else { pal.panel }))
            .border_1()
            .border_color(rgb(pal.border))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(pal.selection)))
            .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        let found = this
                            .store
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.location.entities.iter().find(|e| e.id == id));
                        let kind = found.map(|e| e.kind);
                        // M20/F20c: предмет на земле (extra.ground).
                        let ground = found.is_some_and(|e| {
                            e.extra.get("ground").and_then(|v| v.as_bool()) == Some(true)
                        });
                        match kind {
                            Some(EntityKind::Monster) | Some(EntityKind::Player) => {
                                this.store.select_target(id.clone());
                            }
                            // M18/F35: клик по NPC → диалог (поговорить/квесты).
                            Some(EntityKind::Npc) => {
                                this.on_click(Click::NpcTalk(id.clone()), cx);
                            }
                            // M20/F20c: левый клик по предмету на земле —
                            // подобрать (как «Подобрать» в меню).
                            Some(EntityKind::Object) if ground => {
                                this.on_click(Click::UseItem(id.clone()), cx);
                            }
                            // M23: сундук — то же окно подятия, что и
                            // труп (лут + рюкзак); ПКМ — меню.
                            Some(EntityKind::Container) => {
                                this.on_click(Click::LootOpen(id.clone()), cx);
                            }
                            // M23: ЛКМ по трупу — окно подятия предмета
                            // (лут слева, рюкзак справа); ПКМ — меню.
                            Some(EntityKind::Corpse) => {
                                this.on_click(Click::LootOpen(id.clone()), cx);
                            }
                            _ => {}
                        }
                        cx.notify();
                    }
                }),
            );

        if player_corpse {
            row = row.child(div().text_color(rgb(pal.warn)).child("⚠"));
        }
        // M25: босс — особая строка: череп перед именем (цвет
        // опасности), глиф/имя текущей фазы и каст (подготовка).
        if entity.boss.is_some() {
            row = row.child(fa_icon("solid/skull", px(13.0), rgb(pal.danger)));
            row = row.child(
                div()
                    .flex_grow_1()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(pal.danger))
                    .child(entity.name.clone()),
            );
        } else {
            row = row.child(div().flex_grow_1().child(entity.name.clone()));
        }
        if let Some(boss) = &entity.boss {
            row = row.child(
                div()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(crate::rune_glyph::glyph_div_for(
                        boss.glyph.as_deref(),
                        &boss.name,
                    ))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(pal.muted))
                            .child(boss.name.clone()),
                    ),
            );
            if let Some(casting) = &boss.casting {
                row = row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.warn))
                        .child(tf(lang, Key::MobCasting, &[("{name}", casting.as_str())])),
                );
            }
        }
        if let Some(hp) = &entity.hp {
            row = row.child(bar(
                hp.cur as f32 / hp.max.max(1) as f32,
                format!("{}/{}", hp.cur, hp.max),
                pal.danger,
                pal,
            ));
        }
        // M25: кого монстр атакует — «атакует вас» / «атакует {имя}».
        if entity.kind == EntityKind::Monster {
            if let Some(target_id) = entity.attacking.clone() {
                let my_id = self.store.snapshot.as_ref().map(|s| s.char.id.clone());
                if Some(target_id.as_str()) == my_id.as_deref() {
                    row = row.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(pal.danger))
                            .child(t(lang, Key::MobAttacksYou)),
                    );
                } else {
                    let name = self.store.snapshot.as_ref().and_then(|s| {
                        s.location
                            .entities
                            .iter()
                            .find(|e| e.id == target_id && e.kind == EntityKind::Player)
                            .map(|e| e.name.clone())
                    });
                    if let Some(name) = name {
                        row = row.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(pal.warn))
                                .child(tf(lang, Key::MobAttacks, &[("{name}", name.as_str())])),
                        );
                    }
                }
            }
        }
        // M25: прогресс-бар атаки монстра — по заполнении атака
        // (сервер шлёт evt/mob/attack с новым сроком).
        if entity.kind == EntityKind::Monster {
            if let (Some(next), Some(interval)) = (entity.next_attack_at, entity.attack_interval) {
                if interval > 0 {
                    let now = self.now_ms();
                    let frac = if next <= now {
                        1.0
                    } else {
                        1.0 - (next - now) as f32 / interval as f32
                    };
                    row = row.child(bar(frac.clamp(0.0, 1.0), String::new(), pal.accent, pal));
                }
            }
        }
        for tag in entity.tags.iter().flatten() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.warn))
                    .child(tag.clone()),
            );
        }

        // M11.4: правый клик — контекстное меню Ely (попап по курсору;
        // паритет egui context_menu, инлайн-раскрытие M10 убрано).
        let attackable = !safe && matches!(entity.kind, EntityKind::Monster | EntityKind::Player);
        let this = cx.entity();
        let mk = |label: String, click: Click| {
            let this = this.clone();
            MenuItem::new(label).on_click(move |_window, acx| {
                this.update(acx, |root, cx| root.on_click(click.clone(), cx));
            })
        };
        // Готовый пункт меню + действие (для иконок/отключения M22).
        let mk_item = |item: MenuItem, click: Click| {
            let this = this.clone();
            item.on_click(move |_window, acx| {
                this.update(acx, |root, cx| root.on_click(click.clone(), cx));
            })
        };
        let mut menu = Menu::new();
        if entity.kind == EntityKind::Npc {
            // M18/F35: диалог NPC — поговорить/квесты.
            menu = menu.item(mk(
                t(lang, Key::BtnTalk).to_string(),
                Click::NpcTalk(id.clone()),
            ));
        }
        // M23: на трупе и сундуке «Осмотреть» открывает то же окно
        // подятия, что и ЛКМ по строке (лут + рюкзак); у прочих
        // сущностей — прежняя модалка осмотра.
        menu = menu.item(mk(
            t(lang, Key::Examine).to_string(),
            if matches!(entity.kind, EntityKind::Corpse | EntityKind::Container) {
                Click::LootOpen(id.clone())
            } else {
                Click::Examine(id.clone())
            },
        ));
        // M20/F20b: добыча ресурса (инструмент/ранг — на сервере).
        if entity.kind == EntityKind::Resource {
            menu = menu.item(mk(
                t(lang, Key::GatherBtn).to_string(),
                Click::Gather(id.clone()),
            ));
        }
        // M20/F20c: подбор предмета с земли (extra.ground).
        if entity.kind == EntityKind::Object
            && entity.extra.get("ground").and_then(|v| v.as_bool()) == Some(true)
        {
            menu = menu.item(mk(
                t(lang, Key::PickUpBtn).to_string(),
                Click::UseItem(id.clone()),
            ));
        }
        // Снятие шкур (M22): пункты — при изученной профессии, ноже
        // и флагах трупа; сделанные действия скрываются (флаги
        // skin_done/gut_done приходят в extra трупа). Иконка пункта
        // «Снять шкуру» — ожидаемый опыт (согласовано заказчиком):
        // недостаточный уровень (гейт25+) → Lock + отключение;
        // 0 XP → EyeOff, <15 → Star, <30 → Sparkles, ≥30 → Zap.
        if entity.kind == EntityKind::Corpse {
            if let Some(harv) = entity.extra.get("harvest") {
                let snap = self.store.snapshot.as_ref();
                let learned =
                    snap.is_some_and(|s| s.char.professions.iter().any(|p| p.id == "skinning"));
                let knife = snap.is_some_and(|s| {
                    s.char
                        .inventory
                        .iter()
                        .any(|i| i.item_type == "skinning_knife")
                });
                if learned && knife {
                    let level = snap
                        .and_then(|s| s.char.professions.iter().find(|p| p.id == "skinning"))
                        .map(|p| p.level as i64)
                        .unwrap_or(0);
                    let required = harv.get("level").and_then(|v| v.as_i64()).unwrap_or(1);
                    let diff = level - required;
                    let blocked = required - level >= 25;
                    if harv.get("skin").and_then(|v| v.as_bool()) == Some(true)
                        && harv.get("skin_done").and_then(|v| v.as_bool()) != Some(true)
                    {
                        // Ожидаемый опыт — формула сервера (дубль на
                        // клиенте): min(40, round((10 + треб×1.5) ×
                        // множитель по разнице уровней)).
                        let base = 10.0 + f64::from(required as u32) * 1.5;
                        let mult = if diff <= -10 {
                            1.5
                        } else if diff <= 9 {
                            1.0
                        } else if diff <= 24 {
                            0.5
                        } else {
                            0.0
                        };
                        let xp = ((base * mult).round() as i64).min(40);
                        let icon = if blocked {
                            IconName::Lock
                        } else if xp <= 0 {
                            IconName::EyeOff // опыта нет
                        } else if xp < 15 {
                            IconName::Star // мало
                        } else if xp < 30 {
                            IconName::Sparkles // средне
                        } else {
                            IconName::Zap // много (у потолка40)
                        };
                        let item = MenuItem::new(t(lang, Key::SkinBtn)).icon(icon);
                        menu = menu.item(if blocked {
                            item.disabled(true)
                        } else {
                            mk_item(item, Click::Harvest(id.clone(), "skin".to_string()))
                        });
                    }
                    if harv.get("gut").and_then(|v| v.as_bool()) == Some(true)
                        && harv.get("gut_done").and_then(|v| v.as_bool()) != Some(true)
                    {
                        let item = MenuItem::new(t(lang, Key::GutBtn));
                        menu = menu.item(if blocked {
                            item.icon(IconName::Lock).disabled(true)
                        } else {
                            mk_item(item, Click::Harvest(id.clone(), "gut".to_string()))
                        });
                    }
                }
            }
        }

        if attackable {
            menu = menu.item(mk(
                t(lang, Key::Attack).to_string(),
                Click::Attack(id.clone()),
            ));
        }
        if entity.kind == EntityKind::Player {
            menu = menu.item(mk(
                t(lang, Key::PartyInviteBtn).to_string(),
                Click::PartyInvite(id.clone()),
            ));
            menu = menu.item(mk(
                t(lang, Key::TradeBtn).to_string(),
                Click::TradeOpen(id.clone()),
            ));
        }
        if player_corpse {
            menu = menu.item(MenuItem::new(t(lang, Key::CorpsePlayerHint)).disabled(true));
        }
        ContextMenu::new(format!("ld2-ctx-{}", entity.id), menu)
            .child(row)
            .into_any_element()
    }

    // ── hotbar (ФР-05/09) ─────────────────────────────────────────────────

    /// M14/F4: панель действий (бывший hotbar): атака → активки →
    /// побег → руны → свитки/зелья. Каждая кнопка учитывает собственный
    /// кулдаун и глобальный (приоритет — max) и показывает причину
    /// недоступности в тултипе.
    /// M24: строки индикаторов вставленных рун — под индикатором
    /// ресурса персонажа (указание заказчика): глиф руны, правее —
    /// прогрессбар заполнения заряда, правее бара — `N/M`. Только
    /// РУНЫ из НАДЕТОЙ экипировки (боевой ресурс); самоцветы не
    /// показываются — у них нет заклинания (`spell` пустой) и бар
    /// зарядов бессмыслен. Бар — время до следующего такта тикера
    /// (`time`), при `item`-режиме — доля зарядов; максимум — бар
    /// заполнен. Тултип (указание заказчика): имя · редкость,
    /// серверные подписи (§8) и заряды.
    fn rune_indicator_rows(&self, snap: &StateSnapshot, pal: &Palette) -> Vec<Stateful<Div>> {
        let lang = self.lang;
        let equipped: std::collections::HashSet<&str> = snap
            .char
            .equip
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        let now = self.now_ms();
        let phase = self.store.rune_recharge_phase_ms.unwrap_or(now);
        let (mode, interval_ms) = self
            .store
            .session
            .as_ref()
            .map(|s| {
                (
                    s.magic.rune_recharge.clone(),
                    (s.magic.rune_recharge_s * 1000.0).round() as i64,
                )
            })
            .unwrap_or_else(|| ("time".to_string(), 30_000));
        // Доля прошедшего такта (время до следующего заряда).
        let tick_fill = ((now - phase) as f32 / (interval_ms.max(1) as f32)).clamp(0.0, 1.0);
        let mut rows = Vec::new();
        for item in &snap.char.inventory {
            if !equipped.contains(item.uid.as_str()) {
                continue;
            }
            let Some(sockets) = item.sockets.as_ref() else {
                continue;
            };
            for socket in sockets {
                let Some(rune) = socket.rune.as_ref() else {
                    continue;
                };
                // M24: только руны — у самоцветов spell пустой
                // (заклинания нет), бар зарядов для них бессмыслен.
                if rune.spell.is_empty() {
                    continue;
                }
                let fill = if rune.charges >= rune.max {
                    1.0
                } else if mode == "time" {
                    tick_fill
                } else {
                    rune.charges as f32 / rune.max.max(1) as f32
                };
                // Тултип: имя · редкость, серверные подписи (§8)
                // и заряды (указание заказчика).
                let mut tip = vec![format!(
                    "{} · {}",
                    rune.name,
                    rarity_label(lang, rune.rarity.as_deref().unwrap_or("common"))
                )];
                tip.extend(rune.detail.iter().flatten().cloned());
                tip.push(format!(
                    "{}: {}/{}",
                    t(lang, Key::StatCharges),
                    rune.charges,
                    rune.max
                ));
                let (head_c, body_c) = (pal.text, pal.muted);
                let row = div()
                    .id(format!("ld2-rune-ind-{}-{}", item.uid, socket.index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .child(crate::rune_glyph::glyph_div_for(
                                rune.glyph.as_deref(),
                                &rune.name,
                            )),
                    )
                    .child(
                        div()
                            .w(px(88.0))
                            .h(px(8.0))
                            .rounded_md()
                            .bg(rgb(pal.border))
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(fill.clamp(0.0, 1.0)))
                                    .rounded_md()
                                    .bg(rgb(pal.accent)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("{}/{}", rune.charges, rune.max)),
                    )
                    .tooltip(Tooltip::rich(move |_, _| {
                        let mut col = div().flex().flex_col().gap_1();
                        for (i, line) in tip.iter().enumerate() {
                            col = col.child(
                                div()
                                    .text_size(if i == 0 { px(12.0) } else { px(11.0) })
                                    .text_color(if i == 0 { rgb(head_c) } else { rgb(body_c) })
                                    .child(line.clone()),
                            );
                        }
                        col.into_any_element()
                    }));
                rows.push(row);
            }
        }
        rows
    }

    /// M24: строки активных бафов рун — глиф, название (стак) и
    /// остаток; когда до конца ≤3 с — строка моргает раз в секунду
    /// (указание заказчика; полупрозрачность — без прыжков строки).
    fn rune_buff_rows(&self, snap: &StateSnapshot, pal: &Palette) -> Vec<Stateful<Div>> {
        let lang = self.lang;
        let now_ms = self.now_ms();
        let now = now_ms / 1000; // epoch-секунды (expires_at)
        let active = filter_active_buffs(&snap.char.buffs, now);
        if active.is_empty() {
            return Vec::new();
        }
        let equipped: std::collections::HashSet<&str> = snap
            .char
            .equip
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        let mut glyphs: std::collections::HashMap<&str, Option<String>> =
            std::collections::HashMap::new();
        for item in &snap.char.inventory {
            if !equipped.contains(item.uid.as_str()) {
                continue;
            }
            for socket in item.sockets.iter().flatten() {
                if let Some(rune) = &socket.rune {
                    if let Some(def_id) = rune.def_id.as_deref() {
                        glyphs.insert(def_id, rune.glyph.clone());
                    }
                }
            }
        }
        let mut rows = Vec::new();
        for buff in active {
            // Индикаторы — только рунные баффы (kind = def_id руны).
            if !buff.kind.starts_with("rune_") {
                continue;
            }
            let left = (buff.expires_at - now).max(0);
            // Моргание раз в секунду на последних3 секундах.
            let dim = left <= 3 && (now_ms / 500) % 2 == 1;
            let glyph = glyphs.get(buff.kind.as_str()).cloned().flatten();
            let title = if buff.stacks > 1 {
                format!("{} ×{}", buff.title, buff.stacks)
            } else {
                buff.title.clone()
            };
            rows.push(
                div()
                    .id(format!("ld2-rune-buff-{}", buff.kind))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .opacity(if dim { 0.0 } else { 1.0 })
                    .child(
                        div()
                            .text_size(px(14.0))
                            .child(crate::rune_glyph::glyph_div_for(
                                glyph.as_deref(),
                                &buff.kind,
                            )),
                    )
                    .child(div().text_size(px(12.0)).child(title))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(pal.muted))
                            .child(format_duration(lang, left)),
                    ),
            );
        }
        rows
    }

    /// M24: единый прогресс-бар левой колонки — одна сущность на
    /// снятие шкур/освежевание, инкрустацию и заточку (подпись
    /// «Действие · имя», запуск — `Root::start_progress`); гаснет в
    /// tick по исчезновению источника, плавность — быстрый тик.
    fn progress_bar(&self, pal: &Palette) -> Option<Div> {
        let p = self.cast_progress.as_ref()?;
        let elapsed = (self.now_ms() - p.started_ms).max(0);
        let frac = elapsed as f32 / p.duration_ms as f32;
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .w(px(BAR_WIDTH))
                        .h(px(10.0))
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(frac.clamp(0.0, 1.0)))
                                .rounded_md()
                                .bg(rgb(pal.accent)),
                        ),
                )
                // Описание — под прогресс-баром (указание заказчика).
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.muted))
                        .child(p.label.clone()),
                ),
        )
    }

    fn hotbar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette, snap: &StateSnapshot) -> Div {
        let lang = self.lang;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);
        let now_ms = self.now_ms();
        let global_left = self.store.global_cooldown_left(now_ms);
        // Готовность действия: max(собственный кулдаун, глобальный).
        let ready = |own_left: i64| AppStore::action_ready(own_left, global_left);

        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .flex_wrap()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::HotbarLabel)),
            );

        // ── 1) Обычная атака — всегда первая; без цели — disabled ──
        let attack_label = t(lang, Key::Attack).to_string();
        match self.store.target.clone() {
            None => {
                row = row.child(action_disabled(
                    "ld2-act-attack",
                    attack_label,
                    t(lang, Key::HotbarNoTarget).to_string(),
                    pal,
                ))
            }
            Some(id) => {
                let (ok, left) = ready(0);
                if ok {
                    row = row.child(btn(
                        cx,
                        attack_label,
                        pal.danger,
                        0xffffff,
                        Click::Attack(id),
                    ));
                } else {
                    row = row.child(cd_disabled("ld2-act-attack", attack_label, left, lang, pal));
                }
            }
        }

        // ── 2) Активки: «Мощный удар» (гейт §4); Раскол скрыт ──
        if power_strike_open(&snap.char.skills) {
            let label = t(lang, Key::PowerStrike).to_string();
            let (ok, left) = ready(self.own_cd_left_ms("power_strike", now_ms));
            if ok {
                row = row.child(btn(cx, label, pal.warn, 0xffffff, Click::PowerStrike));
            } else {
                row = row.child(cd_disabled("ld2-act-power-strike", label, left, lang, pal));
            }
        }

        // ── 3) Побег ──
        let flee_label = t(lang, Key::BtnFlee).to_string();
        let (ok, left) = ready(0);
        if ok {
            row = row.child(btn(cx, flee_label, pal.panel, pal.text, Click::Flee));
        } else {
            row = row.child(cd_disabled("ld2-act-flee", flee_label, left, lang, pal));
        }

        // ── 4) Руны надетой экипировки: ×N зарядов (ФР-09; M24 —
        // не только посох: мечи/броня с рунными слотами, ADR-012) ──
        for staff in equipped_rune_items(snap) {
            let staff_uid = staff.uid.clone();
            for socket in staff.sockets.iter().flatten() {
                let Some(rune) = &socket.rune else { continue };
                // M24: самоцвет в сокете — не руна, на панель не идёт.
                if rune.spell.is_empty() {
                    continue;
                }
                let charges_ok = rune.charges > 0;
                let mp_ok = rune_cost <= 0 || snap.char.mp.cur >= rune_cost;
                // M24: класс руны — каст только своему классу.
                let wrong_class = rune
                    .class
                    .as_deref()
                    .is_some_and(|c| snap.char.profile.as_deref() != Some(c));
                let label = format!("{} ×{}", rune.name, rune.charges);
                // Причина недоступности: сначала требования
                // (заряды/мана), потом кулдаун (у кулдауна — таймер
                // и в label, как у остальных кнопок панели).
                let off: Option<OffReason> = if wrong_class {
                    Some(OffReason::Req(t(lang, Key::RuneWrongClass).to_string()))
                } else if !charges_ok {
                    Some(OffReason::Req(t(lang, Key::NoCharge).to_string()))
                } else if !mp_ok {
                    Some(OffReason::Req(t(lang, Key::NotEnoughMp).to_string()))
                } else {
                    // M17/F30g: кулдаун самой руны (cooldown_s) +
                    // глобальный — бар по полной длительности.
                    let cd = rune
                        .def_id
                        .as_deref()
                        .and_then(|d| self.store.item_cd_bar(d, now_ms));
                    let own = cd.map(|(l, _)| l).unwrap_or(0);
                    let (ok, left) = ready(own);
                    if ok {
                        None
                    } else {
                        Some(OffReason::Cd(left))
                    }
                };
                match off {
                    None => {
                        let slot = btn(
                            cx,
                            label,
                            pal.accent,
                            pal.accent_text,
                            Click::RuneCast {
                                staff_uid: staff_uid.clone(),
                                socket_index: socket.index,
                            },
                        );
                        // M11.4: стоимость каста — тултип (egui-паритет).
                        row = row.child(if rune_cost > 0 {
                            slot.id(format!("ld2-rune-{staff_uid}-{}", socket.index))
                                .tooltip(Tooltip::text(format!("{rune_cost} MP")))
                                .into_any_element()
                        } else {
                            slot.into_any_element()
                        });
                    }
                    Some(OffReason::Req(reason)) => {
                        row = row.child(action_disabled(
                            &format!("ld2-rune-off-{staff_uid}-{}", socket.index),
                            label,
                            reason,
                            pal,
                        ));
                    }
                    Some(OffReason::Cd(left)) => {
                        // M17/F30g: бар по полной длительности
                        // предметного кулдауна (GCD — дефолт 1 с).
                        let total = rune
                            .def_id
                            .as_deref()
                            .and_then(|d| self.store.item_cd_bar(d, now_ms))
                            .map(|(_, t)| t)
                            .unwrap_or(1000);
                        row = row.child(cd_bar_button(
                            &format!("ld2-rune-off-{staff_uid}-{}", socket.index),
                            label,
                            left,
                            total,
                            lang,
                            pal,
                        ));
                    }
                }
            }
        }

        // ── 5) Свитки и зелья (свиток — без маны, D4) ──
        let consumables: Vec<_> = snap
            .char
            .inventory
            .iter()
            .filter(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
            .cloned()
            .collect();
        for item in consumables {
            let label = format!("{} ×{}", item.name, item.qty.unwrap_or(1));
            // M17/F30g: собственный кулдаун предмета (cooldown_s,
            // дефолт 1 с) + глобальный; кнопка с таймером и баром.
            let cd = item
                .def_id
                .as_deref()
                .and_then(|d| self.store.item_cd_bar(d, now_ms));
            let own = cd.map(|(l, _)| l).unwrap_or(0);
            let (ok, ready_left) = ready(own);
            if ok {
                row = row.child(btn(
                    cx,
                    label,
                    pal.panel,
                    pal.text,
                    Click::UseItem(item.uid.clone()),
                ));
            } else {
                let (left, total) = cd.unwrap_or((ready_left, 1000));
                row = row.child(cd_bar_button(
                    &format!("ld2-act-item-{}", item.uid),
                    label,
                    left,
                    total,
                    lang,
                    pal,
                ));
            }
        }
        row
    }

    // ── журнал мира (ФР-17) ───────────────────────────────────────────────

    /// M14/F7: блок журнала внизу центра — две вкладки **Лог | Чат**.
    /// Лог прокручивается свободно: приклеивание вниз только когда
    /// пользователь и так внизу ([`scroll_at_bottom`]); иначе новые
    /// строки копятся и показываются кнопкой «▼ N новых» в шапке.
    fn journal_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let dark = pal.bg == crate::palette::palette("dark").bg;
        let len = self.store.log.len();

        // M14/F13: лог **всегда промотан вниз** (замена свободной
        // прокрутки F7): новая строка возвращает вниз; вручную
        // поднять можно до прихода следующей строки.
        // Фикс: весь контент — ОДИН ребёнок ScrollArea, поэтому
        // scroll_to_item(len-1) бил в несуществующий индекс
        // (child_bounds.get() = None → no-op). У gpui есть
        // прямой scroll_to_bottom().
        if len != self.journal_seen && len > 0 {
            self.journal_scroll.scroll_to_bottom();
        }
        self.journal_seen = len;

        // Шапка: вкладки «Журнал»/«Чат» + счётчик строк.
        let active = match self.log_tab {
            LogTab::Log => "log",
            LogTab::Chat => "chat",
        };
        // M14/F12: иконки вкладок (метки без изменений).
        let choices = vec![
            Choice::new("log", t(lang, Key::LogLabel)).icon(IconName::BookOpen),
            Choice::new("chat", t(lang, Key::ChLabel)).icon(IconName::MessageSquare),
        ];
        let this = cx.entity();
        let mut head = div().flex().flex_row().items_center().gap_2();
        head = head.child(Tabs::new("ld2-journal-tabs", choices, active).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.log_tab = if value == "chat" {
                        LogTab::Chat
                    } else {
                        LogTab::Log
                    };
                    cx.notify();
                });
            },
        ));
        head = head.child(div().flex_grow_1());
        head = head.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(len.to_string()),
        );
        // Тело активной вкладки.
        // Тело: flex-колонка, где сам ScrollArea — flex-элемент
        // (flex_grow_1 + min_h_0): его высота тогда определяется
        // layout'ом, а не процентом от auto-родителя — иначе
        // `size_full()` у Ely превращается в auto, контейнер растёт
        // по контенту и сообщения выходят за границы без прокрутки.
        // M14/F8: размер шрифта лога из настроек (10…20 px).
        let log_font = self.config.log_font_px;
        // M14/F13: цвета/жирность строк боя — из настроек.
        let damage_color = self.config.log_damage_color.clone();
        let attack_color = self.config.log_attack_color.clone();
        let damage_bold = self.config.log_damage_bold;
        let body =
            match self.log_tab {
                LogTab::Log => div().flex().flex_col().flex_grow_1().min_h_0().child(
                    ScrollArea::new("ld2-log-scroll")
                        .vertical()
                        .track_scroll(&self.journal_scroll)
                        .flex_grow_1()
                        .min_h_0()
                        .child(div().flex().flex_col().gap_1().children(
                            self.store.log.iter().map(|entry| {
                                // M14/F13: урон персонажу — цвет из
                                // настроек (+жирный), его атаки — свой
                                // цвет; прочие kinds — прежняя палитра.
                                let (color, bold) = match entry.kind {
                                    LogKind::CombatIn => (
                                        hex_to_u32(&damage_color).unwrap_or(0xe0_6c_75),
                                        damage_bold,
                                    ),
                                    LogKind::CombatOut => {
                                        (hex_to_u32(&attack_color).unwrap_or(0x61_af_ef), false)
                                    }
                                    kind => (log_color(kind, dark), false),
                                };
                                // M17/F30f: **...** — жирные сегменты
                                // (слова заклинания и урон), §8.
                                let text = entry.text.clone();
                                let line = div()
                                    .text_size(px(log_font))
                                    .text_color(rgb(color))
                                    .when(bold, |line| line.font_weight(gpui::FontWeight::BOLD));
                                if text.contains("**") {
                                    line.flex().flex_row().children(
                                        split_bold(&text).into_iter().map(|(seg, is_bold)| {
                                            div()
                                                .when(is_bold, |d| {
                                                    d.font_weight(gpui::FontWeight::BOLD)
                                                })
                                                .child(seg.to_string())
                                        }),
                                    )
                                } else {
                                    line.child(text)
                                }
                            }),
                        )),
                ),
                LogTab::Chat => self.chat_body(cx, pal),
            };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            // M25: фиксированная высота — 20 % игрового поля
            // (заказчик); к низу центра приклеивает spacer выше.
            .h(gpui::relative(0.2))
            .min_h(px(120.0))
            .flex_shrink_0()
            .child(head)
            .child(body)
    }

    // ── модалки (паритет egui-окон) ───────────────────────────────────────

    fn death_view(
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
                .mt(px(10.0)),
            )
    }

    /// M18/F35: окно диалога NPC: приветствие, реплики (текст готов —
    /// `{name}` подставляет сервер) и секция «Квесты» с кнопкой
    /// «Принять» (`cmd/quest.accept`; принятый исчезает по
    /// `evt/quest/updated` — store чистит `npc_dialog.quests`).
    fn npc_dialog_view(
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
                        .text_size(px(15.0))
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
        let mut actions = div().flex().flex_col().gap_1();
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
                            .map(|until| ((until - now_ms).max(0) / 1000) as u64)
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
                            .text_size(px(13.0))
                            .text_color(rgb(pal.muted))
                            .child(format!(
                                "{} ({:02}:{:02})",
                                reply.text,
                                wait_left / 60,
                                wait_left % 60
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
        if !dialog.quests.is_empty() {
            actions = actions.child(
                div()
                    .text_size(px(12.0))
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
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("• {}", goals_summary(quest))),
                    );
                }
                if let Some(reward) = &quest.reward_preview {
                    if let Some(xp) = reward.xp {
                        card = card.child(
                            div()
                                .text_size(px(12.0))
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
    fn dialog_history_view(&mut self, pal: &Palette, dialog: &NpcDialogView) -> Div {
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
                    .text_size(px(15.0))
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

    fn examine_view(
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
                    let color = super::sheet::quality_color(item["def_id"].as_str())
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

    fn loot_view(
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
                    .mt(px(8.0))
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
}

/// Полоса-прогресс (паритет egui ProgressBar).
/// M17/F30f: сегменты строки лога по маркерам `**...**` —
/// (текст, жирный). Пробелы остаются в исходных сегментах.
fn split_bold(text: &str) -> Vec<(&str, bool)> {
    let mut out: Vec<(&str, bool)> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("**") else {
            break; // незакрытый маркер — хвост как есть
        };
        if start > 0 {
            out.push((&rest[..start], false));
        }
        out.push((&after[..end], true));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        out.push((rest, false));
    }
    out
}

/// M18: ширина шкалы — общая для здоровья и ресурса (одинаковые
/// прогрессбары в левой колонке).
const BAR_WIDTH: f32 = 140.0;

fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(BAR_WIDTH))
                .h(px(10.0))
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac.clamp(0.0, 1.0)))
                        .rounded_md()
                        .bg(rgb(fill)),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(label),
        )
}

/// M25: виджет шкалы (заказчик): цифры ПОВЕРХ бара (внутри), заливка
/// — `color` (ХП остаётся зелёным), текст белый с тёмным контуром —
/// контраст и к заливке, и к фону трека. Параметры: цвет, максимум,
/// текущее, надпись, ширина, высота.
fn stat_bar(
    color: u32,
    max: i64,
    cur: i64,
    label: &str,
    width: f32,
    height: f32,
    pal: &Palette,
) -> Div {
    let frac = (cur as f32 / max.max(1) as f32).clamp(0.0, 1.0);
    let text = format!("{label} {cur}/{max}");
    let text_size = (height - 6.0).max(9.0);
    div()
        .relative()
        .w(px(width))
        .h(px(height))
        .child(
            div()
                .absolute()
                .size_full()
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac))
                        .rounded_md()
                        .bg(rgb(color)),
                ),
        )
        // Побитовый инверс заливки бара и жирный — без фона и теней
        // (указание заказчика).
        .child(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(text_size))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(rgb(!color & 0xff_ff_ff))
                .child(text),
        )
}

/// M14/F4: почему кнопка панели недоступна: требование (цель/мана/
/// заряды — текст в тултипе) или кулдаун (таймер на кнопке + тултип).
enum OffReason {
    Req(String),
    Cd(i64),
}

/// M14/F4: недоступная кнопка панели — причина в тултипе (тултипы
/// используются и для включённых кнопок — egui-паритет M11.4).
fn action_disabled(id: &str, label: String, reason: String, pal: &Palette) -> Stateful<Div> {
    div()
        .id(id.to_string())
        .tooltip(Tooltip::text(reason))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.border))
        .text_color(rgb(pal.muted))
        .child(label)
}

/// M14/F4: текст причины «кулдаун» — остаток округляется вверх
/// (100 мс до конца → «1 с»), чтобы значение не мигало в 0.
fn cooldown_reason(lang: ld2_core::l10n::Lang, left_ms: i64) -> String {
    let secs = ((left_ms.max(0) + 999) / 1000).max(1);
    tf(lang, Key::HotbarCooldown, &[("{s}", &secs.to_string())])
}

/// M14/F4: кнопка на кулдауне (собственный/глобальный): таймер остатка
/// прямо на кнопке (перерисовка тиком 50 мс) + причина в тултипе.
fn cd_disabled(
    id: &str,
    label: String,
    left_ms: i64,
    lang: ld2_core::l10n::Lang,
    pal: &Palette,
) -> Stateful<Div> {
    let secs = ((left_ms.max(0) + 999) / 1000).max(1);
    let short = tf(lang, Key::HotbarCdShort, &[("{s}", &secs.to_string())]);
    action_disabled(
        id,
        format!("{label} · {short}"),
        cooldown_reason(lang, left_ms),
        pal,
    )
}

/// M17/F30g: кнопка в предметном кулдауне — таймер на кнопке и
/// анимированный прогрессбар под ней (остаток от полной длительности;
/// анимация — тик 50 мс через cd_active в root).
fn cd_bar_button(
    id: &str,
    label: String,
    left_ms: i64,
    total_ms: i64,
    lang: ld2_core::l10n::Lang,
    pal: &Palette,
) -> Div {
    let secs = ((left_ms.max(0) + 999) / 1000).max(1);
    let short = tf(lang, Key::HotbarCdShort, &[("{s}", &secs.to_string())]);
    let frac = (left_ms as f32 / total_ms.max(1) as f32).clamp(0.0, 1.0);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(action_disabled(
            id,
            format!("{label} · {short}"),
            cooldown_reason(lang, left_ms),
            pal,
        ))
        .child(
            div().h(px(3.0)).rounded_md().bg(rgb(pal.border)).child(
                div()
                    .h_full()
                    .w(gpui::relative(frac))
                    .rounded_md()
                    .bg(rgb(pal.accent)),
            ),
        )
}

/// Цвет severity (палитра §7.4, локальная копия — egui-версия возвращает Color32).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Цвет строки журнала (палитра §7.4; светлая тема — затемнение светлых,
/// паритет M9.2 egui-клиента).
fn log_color(kind: LogKind, dark: bool) -> u32 {
    use LogKind::*;
    if !dark {
        return match kind {
            System => 0x5a_5e_66,
            Location => 0x4a_4e_58,
            Chat => 0x2a_2a_2e,
            Xp => 0x2e_7d_91,
            Loot => 0x8a_6d_1f,
            Crime => 0xa5_5a_22,
            _ => log_color(kind, true),
        };
    }
    match kind {
        System => 0x7f_84_8e,
        Location => 0xab_b2_bf,
        CombatOut => 0xe0_6c_75,
        CombatIn => 0x98_c3_79,
        Magic => 0x61_af_ef,
        Loot => 0xe5_c0_7b,
        Crime => 0xd1_9a_66,
        Quest => 0xc6_78_dd,
        Xp => 0x56_b6_c2,
        Chat => 0xeb_eb_eb,
    }
}

/// Подсказка строки сущности: HP и теги (имя — уже в строке).
fn entity_tooltip(entity: &Entity) -> String {
    let hp = entity
        .hp
        .map(|hp| format!(" · {}/{}", hp.cur, hp.max))
        .unwrap_or_default();
    let tags = entity
        .tags
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if tags.is_empty() {
        format!("{}{hp}", entity.name)
    } else {
        format!("{}{hp} · {tags}", entity.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M17/F30f: маркеры **...** → сегменты; незакрытый не ломает строку.
    #[test]
    fn split_bold_marks_segments() {
        assert_eq!(
            split_bold("Bbnf: **Farta Lussen**, вы бьёте Волк струёй пламени **10**"),
            vec![
                ("Bbnf: ", false),
                ("Farta Lussen", true),
                (", вы бьёте Волк струёй пламени ", false),
                ("10", true),
            ]
        );
        assert_eq!(
            split_bold("обычная строка"),
            vec![("обычная строка", false)]
        );
        let open = split_bold("хвост **без пары");
        assert_eq!(
            open,
            vec![("хвост **без пары", false)],
            "незакрытый — как есть"
        );
    }
}
