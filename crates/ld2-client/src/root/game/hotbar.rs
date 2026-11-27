//! Хотбар боевых действий (RF3m).

use super::*;

impl Root {
    pub(crate) fn hotbar(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
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
        row = self.hotbar_runes(row, cx, pal, snap, lang, now_ms, global_left, rune_cost);
        row = self.hotbar_consumables(row, cx, pal, snap, lang, now_ms, global_left);
        row
    }

    /// Кнопки активных рун в хотбаре (RF3m: часть `hotbar`).
    #[allow(clippy::too_many_arguments)]
    fn hotbar_runes(
        &mut self,
        mut row: gpui::Div,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
        lang: ld2_core::l10n::Lang,
        now_ms: i64,
        global_left: i64,
        rune_cost: i64,
    ) -> gpui::Div {
        let ready = |own_left: i64| AppStore::action_ready(own_left, global_left);
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

        row
    }

    /// Кнопки свитков и зелий в хотбаре (RF3m: часть `hotbar`).
    #[allow(clippy::too_many_arguments)]
    fn hotbar_consumables(
        &mut self,
        mut row: gpui::Div,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
        lang: ld2_core::l10n::Lang,
        now_ms: i64,
        global_left: i64,
    ) -> gpui::Div {
        let ready = |own_left: i64| AppStore::action_ready(own_left, global_left);
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
}
