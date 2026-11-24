//! Окно персонажа: character (RF3l).

use super::*;

impl Root {
    // ── каркас окна ───────────────────────────────────────────────────────

    /// M15/F25b/M16: **окно персонажа** — всплывающий диалог поверх
    /// игры: скрим (клик мимо закрывает), карточка 760×560 по центру;
    /// Esc и ✕ в шапке закрывают, кнопка в шапке/хоткей `c` —
    /// открывают. Игра видна позади (слой поверх `game_screen`, а не
    /// замена ему). Инвентарь — отдельное окно рюкзака (M16/F29b);
    /// геометрия/жесты — оболочка `popup_window`.
    pub(crate) fn character_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.char_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        // Автозагрузки один раз (ФР-08/16 + M15/F25 журнал статусов).
        if self.store.snapshot.is_some() {
            // M15/F25: журнал нарушений нужен левой колонке окна —
            // грузим при открытии (вкладки «Статусы» больше нет).
            if !self.store.crime_log_loaded {
                self.store.crime_log_loaded = true;
                self.send_cmd(&CrimeLog { limit: Some(50) }, Act::CrimeLog);
            }
            if self.sheet_tab == SheetTab::Guild && !self.store.guild_browse_loaded {
                self.store.guild_browse_loaded = true;
                self.send_cmd(&GuildBrowse { query: None }, Act::GuildBrowse);
            }
        }

        let lang = self.lang;
        // M16/F29d: в заголовке — никнейм персонажа (не «Персонаж»).
        let title = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.name.clone())
            .unwrap_or_else(|| t(lang, Key::BtnCharacter).to_string());
        // M16/F29b: тело собирается до оболочки — та берёт у Root
        // только геометрию `PopupState` вида `kind`.
        let body = div()
            .flex()
            .flex_row()
            .gap_3()
            .flex_grow_1()
            .min_h_0()
            .child(self.character_left_column(cx, pal))
            .child(self.character_tabs(cx, pal));
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Character,
            title,
            None,
            body,
            None,
        ))
    }

    /// M15/F25: левая колонка окна — шкала опыта, экипировка,
    /// разделитель, статусы со временем (+ журнал нарушений);
    /// M18: атрибуты перенесены во вкладку «Навыки».
    pub(crate) fn character_left_column(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Div {
        // M16/F29f: справа запас под скроллбар — цифры значений не
        // налазили на индикатор прокрутки.
        let mut col = div().flex().flex_col().gap_2().pr(px(16.0));
        col = col.child(self.character_xp_block(pal));
        col = col.child(self.equip_block(cx, pal));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        // M18/F58f: активные баффы рун — над статусами (ADR-012 §6).
        col = col.child(self.buffs_block(pal));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(self.statuses_block(cx, pal));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(420.0))
            .min_h_0()
            .child(
                ScrollArea::new("ld2-char-left")
                    .vertical()
                    .flex_grow_1()
                    .min_h_0()
                    .child(col),
            )
    }

    /// M15/F25: шкала опыта — уровень и прогресс (данные F24),
    /// общий опыт и очки опыта; `level = None` (сервер без M15) —
    /// показываем только очки.
    pub(crate) fn character_xp_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .rounded_md()
            .bg(rgb(pal.panel));
        let Some(snap) = self.store.snapshot.as_ref() else {
            return panel.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
        };
        if let (Some(level), Some(xp), Some(next)) =
            (snap.char.level, snap.char.xp_in_level, snap.char.xp_to_next)
        {
            panel = panel.child(div().font_weight(gpui::FontWeight::BOLD).child(tf(
                lang,
                Key::CharLevel,
                &[("{n}", &level.to_string())],
            )));
            if next > 0 {
                panel = panel.child(bar(
                    xp as f32 / next as f32,
                    format!("{xp} / {next}"),
                    pal.accent,
                    pal,
                ));
            }
        }
        if let Some(total) = snap.char.total_xp {
            panel = panel.child(xp_kv_row(
                pal,
                t(lang, Key::CharXpTotal),
                &total.to_string(),
            ));
        }
        panel = panel.child(xp_kv_row(
            pal,
            t(lang, Key::CharXpPool),
            &snap.char.xp_pool.to_string(),
        ));
        panel
    }

    /// M15/F25: правая колонка окна — вкладки с автозагрузками.
    pub(crate) fn character_tabs(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let tab = self.sheet_tab;
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_h_0()
            .min_w_0();

        // M11.4: вкладки листа — Tabs Ely (выбор + клавиатурная навигация).
        let choices: Vec<Choice> = SheetTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let this = cx.entity();
        col = col.child(
            Tabs::new("ld2-sheet-tabs", choices, tab.as_str()).on_change(
                move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, cx| {
                        root.sheet_tab = SheetTab::parse(&value);
                        root.config.sheet_tab = value;
                        root.save_config();
                        cx.notify();
                    });
                },
            ),
        );
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));

        let body = match tab {
            // M15/F26: производные характеристики; заглушка до F27 — «Репутация».
            SheetTab::Params => self.tab_params(pal),
            SheetTab::Reputation => div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::CharComingSoon)),
            SheetTab::Skills => self.tab_skills(cx, pal),
            SheetTab::Guild => self.tab_guild(cx, pal),
        };
        // M16/F29f: справа запас под скроллбар — цифры значений не
        // налазили на индикатор прокрутки (как в левой колонке).
        let body = body.pr(px(16.0));
        // M14/F3: длинные списки (навыки/профессии с рецептами) —
        // прокручиваются, остальные вкладки ведут себя как раньше.
        col = col.child(match tab {
            SheetTab::Skills => scrollable_tab("ld2-skills-scroll", &self.skills_scroll, body),
            SheetTab::Params => scrollable_tab("ld2-params-scroll", &self.params_scroll, body),
            _ => body,
        });
        col
    }

    /// M15/F26: вкладка «Параметры» — 16 производных характеристик
    /// (серверные `derived`; у сервера без M15 — заглушка).
    pub(crate) fn tab_params(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(d) = snap.char.derived.as_ref() else {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::CharComingSoon));
        };
        // M16/F29d: группы аналогично навыкам (заголовок + строки);
        // порядок 16 позиций спецификации сохранён внутри групп.
        let groups: [(&Key, Vec<(&Key, String)>); 3] = [
            (
                &Key::ParGroupAttack,
                vec![
                    (&Key::ParAccuracy, format!("{}%", d.accuracy)),
                    (
                        &Key::ParDamage,
                        format!("{}..{}", d.damage_min, d.damage_max),
                    ),
                    (&Key::ParCritChance, format!("{}%", d.crit_chance)),
                    (&Key::ParCritDamage, format!("{}%", d.crit_damage)),
                ],
            ),
            (
                &Key::SkillGroupDefense,
                vec![
                    (&Key::ParArmor, d.armor.to_string()),
                    (&Key::ParDodge, format!("{}%", d.dodge)),
                    (&Key::ParParry, format!("{}..{}", d.parry_min, d.parry_max)),
                    (&Key::ParParryChance, format!("{}%", d.parry_chance)),
                    (&Key::ParBlock, format!("{}..{}", d.block_min, d.block_max)),
                    (&Key::ParBlockChance, format!("{}%", d.block_chance)),
                ],
            ),
            (
                &Key::SkillGroupMagic,
                vec![
                    (&Key::ParMagicDefense, d.magic_defense.to_string()),
                    (&Key::ParMagicDodge, format!("{}%", d.magic_dodge)),
                    (&Key::ParMagicResist, format!("{}%", d.magic_resist)),
                    (&Key::ParMagicDispel, format!("{}%", d.magic_dispel)),
                    (
                        &Key::ParMagicCritDamage,
                        format!("{}%", d.magic_crit_damage),
                    ),
                    (
                        &Key::ParMagicCritChance,
                        format!("{}%", d.magic_crit_chance),
                    ),
                ],
            ),
        ];
        let mut col = div().flex().flex_col().gap_1();
        for (header, rows) in groups {
            col = col.child(
                div()
                    .mt(px(6.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, *header)),
            );
            for (label, value) in rows {
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().text_color(rgb(pal.muted)).child(t(lang, *label)))
                        .child(div().font_weight(gpui::FontWeight::BOLD).child(value)),
                );
            }
        }
        col
    }

    // ── вкладка «Навыки» (ФР-04) ──────────────────────────────────────────

    pub(crate) fn tab_skills(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let pool = snap.char.xp_pool;
        // M18: атрибуты сверху, черта-разделитель, затем навыки.
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.attrs_block(pal))
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                "{}: {}",
                t(lang, Key::XpPoolLabel),
                pool
            )));
        // M15/F28: навыки по группам GDD §3 (A–F) — заголовок группы,
        // если в снапшоте есть хоть один её навык. Заказчик 2026-11-20:
        // группы сворачиваемые — кнопка-заголовок (акцент = развёрнута,
        // панель = свёрнута, как пати-виджет), состояние в конфиге.
        for (gid, key) in SKILL_GROUPS {
            let group: Vec<&SkillSnapshot> = snap
                .char
                .skills
                .iter()
                .filter(|s| s.group.as_deref() == Some(gid))
                .collect();
            if group.is_empty() {
                continue;
            }
            let collapsed = self.config.sheet_collapsed.iter().any(|id| id == gid);
            let gid_owned = gid.to_string();
            let label = format!("{} ({})", t(lang, key), group.len());
            col = col.child(div().mt(px(6.0)).child(act_btn(
                cx,
                label,
                if collapsed { pal.panel } else { pal.accent },
                pal.text,
                move |this, cx| {
                    if let Some(pos) = this
                        .config
                        .sheet_collapsed
                        .iter()
                        .position(|id| *id == gid_owned)
                    {
                        this.config.sheet_collapsed.remove(pos);
                    } else {
                        this.config.sheet_collapsed.push(gid_owned.clone());
                    }
                    this.save_config();
                    let _ = cx;
                },
            )));
            if !collapsed {
                for skill in group {
                    col = col.child(self.skill_row(cx, pal, skill));
                }
            }
        }
        // Строки без известной группы — в конце: старый сервер (group =
        // None) или новая группа контента без ключа l10n.
        let rest: Vec<&SkillSnapshot> = snap
            .char
            .skills
            .iter()
            .filter(|s| {
                s.group
                    .as_deref()
                    .is_none_or(|g| !SKILL_GROUPS.iter().any(|(id, _)| *id == g))
            })
            .collect();
        for skill in rest {
            col = col.child(self.skill_row(cx, pal, skill));
        }
        col
    }

    /// M15/F28: строка навыка (перенос из таба) — название/ранг,
    /// тултип описания, ⓘ-карточка, прогресс; M18/F44: при полной
    /// шкале — подсказка «возможно повышение ранга».
    pub(crate) fn skill_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        skill: &SkillSnapshot,
    ) -> impl gpui::IntoElement {
        let lang = self.lang;
        let name = skill.name.clone().unwrap_or_else(|| skill.id.clone());
        let id = skill.id.clone();
        let rank = skill.rank;
        // M14/F17: описание навыка — в тултипе строки; ⓘ открывает
        // карточку с полной информацией.
        let desc = skill.description.clone().unwrap_or_default();
        let info_id = id.clone();
        let mut row = div().id(format!("ld2-skill-{id}"));
        if !desc.is_empty() {
            row = row.tooltip(Tooltip::text(desc));
        }
        let mut row = row
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().child(name))
                    .child(div().text_color(rgb(pal.muted)).child(format!("·{rank}")))
                    .child(div().flex_grow_1())
                    .child(
                        IconButton::new(format!("ld2-skill-info-{id}"), IconName::Info)
                            .tooltip(t(lang, Key::SkillInfoBtn))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.skill_info = Some(info_id.clone());
                                cx.notify();
                            })),
                    ),
            )
            .child(bar(
                if skill.xp_to_next > 0 {
                    skill.xp as f32 / skill.xp_to_next as f32
                } else {
                    0.0
                },
                format!("{}/{}", skill.xp, skill.xp_to_next),
                pal.accent,
                pal,
            ));
        // M18/F44: вложение очков из пула убрано (ADR-009) — повышение
        // ранга только у тренера в диалоге; при полной шкале — подсказка.
        if skill.xp_to_next > 0 && skill.xp >= skill.xp_to_next {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.accent))
                    .child(t(lang, Key::SkillReady)),
            );
        }
        row
    }

    /// M14/F17: карточка информации о навыке (иконка ⓘ во вкладке
    /// «Навыки»): описание из контента, ранг и опыт. Навык исчез
    /// (смена персонажа/экрана) → карточка закрывается.
    pub(crate) fn skill_info_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let id = self.skill_info.clone()?;
        let info = self.store.snapshot.as_ref().and_then(|snap| {
            let skill = snap.char.skills.iter().find(|s| s.id == id)?;
            Some((
                skill.name.clone().unwrap_or_else(|| skill.id.clone()),
                skill.description.clone().unwrap_or_default(),
                skill.rank,
                skill.xp,
                skill.xp_to_next,
            ))
        });
        let Some((name, desc, rank, xp, next)) = info else {
            self.skill_info = None;
            return None;
        };
        let lang = self.lang;
        let close = self.dismiss(cx, |r| r.skill_info = None);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(t(lang, Key::RankFmt).replace("{r}", &rank.to_string())))
            .child(bar(
                if next > 0 {
                    xp as f32 / next as f32
                } else {
                    0.0
                },
                format!("{xp}/{next}"),
                pal.accent,
                pal,
            ));
        if !desc.is_empty() {
            body = body.child(div().child(desc));
        }
        Some(
            Dialog::new(format!("ld2-skill-info-{id}"), name, close)
                .child(body)
                .into_any_element(),
        )
    }

    // ── трекер квестов (ФР-12) ────────────────────────────────────────────

    pub(crate) fn tracker_block(&self, pal: &Palette) -> Option<Div> {
        if self.store.quest_tracker.is_empty() {
            return None;
        }
        let lang = self.lang;
        let snap = self.store.snapshot.as_ref()?;
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::TrackerLabel)),
            );
        for id in &self.store.quest_tracker {
            if let Some(q) = snap.char.quests.iter().find(|q| &q.id == id) {
                block = block.child(div().text_size(px(13.0)).child(format!(
                    "{} — {}",
                    q.title.clone().unwrap_or_else(|| q.id.clone()),
                    goals_summary(q)
                )));
            }
        }
        Some(block)
    }

    // ── вкладка «Атрибуты» ────────────────────────────────────────────────

    /// M15/F25: атрибуты — середина левой колонки (перенос из вкладки
    /// «Атрибуты» по правилу дубликатов) — с заголовком секции.
    pub(crate) fn attrs_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::SheetAttrs)),
        );
        for (key, value) in &snap.char.attrs {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(attr_label(lang, key)),
                    )
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(value.to_string()),
                    ),
            );
        }
        col
    }

    // ── вкладка «Статусы» (ФР-11, M6) ────────────────────────────────────

    /// M15/F25: статусы со временем + журнал нарушений — низ левой
    /// колонки (перенос из вкладки «Статусы» по правилу дубликатов).
    /// M18/F58f (ADR-012 §6): активные баффы рун — название (серверная
    /// строка §8), стак и обратный отсчёт до снятия (по серверным срокам
    /// как у статусов: now = локальное + offset, §1.1).
    pub(crate) fn buffs_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::BuffsHeader)),
        );
        let Some(snap) = self.store.snapshot.as_ref() else {
            return col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
        };
        let now = local_epoch() + self.store.time_offset;
        let active = filter_active_buffs(&snap.char.buffs, now);
        if active.is_empty() {
            return col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoBuffs)),
            );
        }
        for buff in active {
            let left = buff.expires_at - now;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().child(buff.title.clone()))
                    .child(
                        div()
                            .text_color(rgb(pal.accent))
                            .child(format!("×{}", buff.stacks)),
                    )
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(format_duration(lang, left)),
                    ),
            );
        }
        col
    }

    pub(crate) fn statuses_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // Отсчёт по серверным срокам: now = локальное + offset (§1.1).
        let now = local_epoch() + self.store.time_offset;
        let active = filter_active_statuses(&snap.char.crime.statuses, now);
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::StatusesTab)),
        );

        if active.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoStatuses)),
            );
        }
        for status in &active {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(severity_rgb(status.severity)))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(format_duration(lang, left)),
                    ),
            );
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::CrimeJournal)),
        );
        col = col.child(act_btn(
            cx,
            t(lang, Key::ReloadJournal),
            pal.panel,
            pal.text,
            |this, cx| {
                let cmd = CrimeLog { limit: Some(50) };
                this.send_cmd(&cmd, Act::CrimeLog);
                let _ = cx;
            },
        ));
        if self.store.crime_log.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::JournalEmpty)),
            );
        }
        for entry in self.store.crime_log.iter().rev().take(60) {
            let title = crime_kind_title(&self.store, &entry.kind);
            col = col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(format!(
                        "{title}: +{} · {} {}",
                        format_duration(lang, entry.delta_s),
                        t(lang, Key::TotalLabel),
                        format_duration(lang, entry.total_s)
                    )),
            );
        }
        col
    }

    /// Имена навыков для бонусов описания (`skill`: id → название).
    pub(crate) fn skill_names_for(
        &self,
        detail: &serde_json::Value,
    ) -> std::collections::HashMap<String, String> {
        let mut names = std::collections::HashMap::new();
        if let Some(bonuses) = detail["bonuses"].as_array() {
            for bonus in bonuses {
                if bonus["kind"] == "skill" {
                    let id = bonus["id"].as_str().unwrap_or_default().to_string();
                    let name = self
                        .store
                        .snapshot
                        .as_ref()
                        .and_then(|snap| snap.char.skills.iter().find(|skill| skill.id == id))
                        .and_then(|skill| skill.name.clone())
                        .unwrap_or_else(|| id.clone());
                    names.insert(id, name);
                }
            }
        }
        names
    }
}
