//! Сущности локации: строки, руны, шкалы (RF3m).

use ld2_core::domain;

use super::*;

const MS_PER_SECOND: i64 = 1000;
const MS_PER_SECOND_F: f64 = 1000.0;
const DEFAULT_RUNE_RECHARGE_MS: i64 = 30_000;
const BUFF_BLINK_LEFT_S: i64 = 3;
const BUFF_BLINK_PERIOD_MS: i64 = 500;
const HARVEST_LEVEL_GAP: i64 = 25;
const HARVEST_XP_BASE: f64 = 10.0;
const HARVEST_XP_PER_LEVEL: f64 = 1.5;
const HARVEST_DIFF_LOW: i64 = -10;
const HARVEST_DIFF_MID: i64 = 9;
const HARVEST_DIFF_HIGH: i64 = 24;
const HARVEST_MULT_LOW: f64 = 1.5;
const HARVEST_MULT_NORMAL: f64 = 1.0;
const HARVEST_MULT_HIGH: f64 = 0.5;
const HARVEST_MULT_ZERO: f64 = 0.0;
const HARVEST_XP_MAX: i64 = 40;

impl Root {
    /// Строка сущности: левый клик — цель, ПКМ — действия (ФР-03/05).
    pub(crate) fn entity_row(
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

        // R-dnd (заказчик): предмет на земле — отображение как в
        // рюкзаке (extra["item"] = ItemSnapshot; цвет редкости, +N,
        // ×qty, иконки слотов). Старый сервер поля не шлёт → прежний
        // вид (аддитивно §11).
        let ground_snap: Option<ItemSnapshot> = entity
            .extra
            .get("item")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let mut row = div()
            .id(format!("ld2-ent-{}", entity.id))
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
        // Тултип: у предмета на земле — карточка описания (как в
        // рюкзаке), у прочих сущностей — прежний текстовый.
        if ground_snap.is_none() {
            row = row.tooltip(Tooltip::text(entity_tooltip(&entity)));
        }

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
        } else if let Some(snap) = &ground_snap {
            // R-dnd (заказчик): подпись как в рюкзаке — цвет редкости,
            // +N, ×qty, иконки слотов; свечение эпика/легендарки.
            row = row.shadow(crate::root::sheet::rarity_glow_inset(&snap.rarity, pal));
            row = row.child(
                div()
                    .flex_grow_1()
                    .child(crate::root::sheet::item_label_stack(
                        snap,
                        crate::root::sheet::item_color(snap, pal),
                    )),
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
                            .text_size(px(crate::ui_tokens::FONT_XS))
                            .text_color(rgb(pal.muted))
                            .child(boss.name.clone()),
                    ),
            );
            if let Some(casting) = &boss.casting {
                row = row.child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_XS))
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
        row = self.entity_mob_status(row, &entity, lang, pal);
        for tag in entity.tags.iter().flatten() {
            row = row.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_XS))
                    .text_color(rgb(pal.warn))
                    .child(tag.clone()),
            );
        }

        let menu = self.entity_menu(cx, lang, &entity, id.clone(), safe, player_corpse);
        let menu_el = ContextMenu::new(format!("ld2-ctx-{}", entity.id), menu).child(row);
        // Тултип описания предмета на земле — тот же виджет, что в
        // рюкзаке (задержка 1с, пульс наружу — заказчик).
        if let Some(snap) = ground_snap {
            let detail = serde_json::to_value(&snap).unwrap_or_default();
            let skills = self.skill_names_for(&detail);
            crate::root::sheet::item_tooltip_trigger(
                format!("ld2-ent-tip-{}", id),
                menu_el,
                detail,
                skills,
                self.config.theme.clone(),
                lang,
                snap.rarity.clone(),
                format!(
                    "{} · {} · +{}",
                    snap.name,
                    rarity_label(lang, &snap.rarity),
                    snap.upgrade
                ),
                id,
            )
            .into_any_element()
        } else {
            menu_el.into_any_element()
        }
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
    pub(crate) fn rune_indicator_rows(
        &self,
        snap: &StateSnapshot,
        pal: &Palette,
    ) -> Vec<Stateful<Div>> {
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
                    (s.magic.rune_recharge_s * MS_PER_SECOND_F).round() as i64,
                )
            })
            .unwrap_or_else(|| ("time".to_string(), DEFAULT_RUNE_RECHARGE_MS));
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
                    rarity_label(
                        lang,
                        rune.rarity.as_deref().unwrap_or(domain::RARITY_COMMON)
                    )
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
                    .child(div().text_size(px(crate::ui_tokens::FONT_LG)).child(
                        crate::rune_glyph::glyph_div_for(rune.glyph.as_deref(), &rune.name),
                    ))
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
                            .text_size(px(crate::ui_tokens::FONT_SM))
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
    pub(crate) fn rune_buff_rows(&self, snap: &StateSnapshot, pal: &Palette) -> Vec<Stateful<Div>> {
        let lang = self.lang;
        let now_ms = self.now_ms();
        let now = now_ms / MS_PER_SECOND; // epoch-секунды (expires_at)
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
            let dim = left <= BUFF_BLINK_LEFT_S && (now_ms / BUFF_BLINK_PERIOD_MS) % 2 == 1;
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
                    .child(div().text_size(px(crate::ui_tokens::FONT_LG)).child(
                        crate::rune_glyph::glyph_div_for(glyph.as_deref(), &buff.kind),
                    ))
                    .child(div().text_size(px(crate::ui_tokens::FONT_SM)).child(title))
                    .child(
                        div()
                            .text_size(px(crate::ui_tokens::FONT_XS))
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
    pub(crate) fn progress_bar(&self, pal: &Palette) -> Option<Div> {
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
                        .text_size(px(crate::ui_tokens::FONT_XS))
                        .text_color(rgb(pal.muted))
                        .child(p.label.clone()),
                ),
        )
    }

    /// Статус моба в строке: «атакует вас/{имя}» и прогресс атаки
    /// (RF3m: часть `entity_row`).
    fn entity_mob_status(
        &mut self,
        mut row: gpui::Stateful<gpui::Div>,
        entity: &Entity,
        lang: ld2_core::l10n::Lang,
        pal: &Palette,
    ) -> gpui::Stateful<gpui::Div> {
        // M25: кого монстр атакует — «атакует вас» / «атакует {имя}».
        if entity.kind == EntityKind::Monster {
            if let Some(target_id) = entity.attacking.clone() {
                let my_id = self.store.snapshot.as_ref().map(|s| s.char.id.clone());
                if Some(target_id.as_str()) == my_id.as_deref() {
                    row = row.child(
                        div()
                            .text_size(px(crate::ui_tokens::FONT_XS))
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
                                .text_size(px(crate::ui_tokens::FONT_XS))
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
        row
    }

    /// Контекстное меню сущности (RF3m: часть `entity_row`).
    fn entity_menu(
        &mut self,
        cx: &mut Context<'_, Self>,
        lang: ld2_core::l10n::Lang,
        entity: &Entity,
        id: String,
        safe: bool,
        player_corpse: bool,
    ) -> Menu {
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
        // M35: игрок — «Осмотреть» открывает карточку (экипировка,
        // уровень, профессии); позиция — курсор в момент клика.
        if entity.kind == EntityKind::Player {
            let this = cx.entity();
            let id = id.clone();
            menu = menu.item(MenuItem::new(t(lang, Key::Examine).to_string()).on_click(
                move |window, acx| {
                    let pos = window.mouse_position();
                    this.update(acx, |root, cx| {
                        root.inspect_at = Some((f32::from(pos.x), f32::from(pos.y)));
                        root.on_click(Click::InspectPlayer(id.clone()), cx);
                    });
                },
            ));
        } else {
            menu = menu.item(mk(
                t(lang, Key::Examine).to_string(),
                if matches!(entity.kind, EntityKind::Corpse | EntityKind::Container) {
                    Click::LootOpen(id.clone())
                } else {
                    Click::Examine(id.clone())
                },
            ));
        }
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
                let learned = snap.is_some_and(|s| {
                    s.char
                        .professions
                        .iter()
                        .any(|p| p.id == domain::PROFESSION_SKINNING)
                });
                let knife = snap.is_some_and(|s| {
                    s.char
                        .inventory
                        .iter()
                        .any(|i| i.item_type == domain::ITEM_SKINNING_KNIFE)
                });
                if learned && knife {
                    let level = snap
                        .and_then(|s| {
                            s.char
                                .professions
                                .iter()
                                .find(|p| p.id == domain::PROFESSION_SKINNING)
                        })
                        .map(|p| p.level as i64)
                        .unwrap_or(0);
                    let required = harv.get("level").and_then(|v| v.as_i64()).unwrap_or(1);
                    let diff = level - required;
                    let blocked = required - level >= HARVEST_LEVEL_GAP;
                    if harv.get(domain::ACTION_SKIN).and_then(|v| v.as_bool()) == Some(true)
                        && harv.get("skin_done").and_then(|v| v.as_bool()) != Some(true)
                    {
                        // Ожидаемый опыт — формула сервера (дубль на
                        // клиенте): min(40, round((10 + треб×1.5) ×
                        // множитель по разнице уровней)).
                        let base =
                            HARVEST_XP_BASE + f64::from(required as u32) * HARVEST_XP_PER_LEVEL;
                        let mult = if diff <= HARVEST_DIFF_LOW {
                            HARVEST_MULT_LOW
                        } else if diff <= HARVEST_DIFF_MID {
                            HARVEST_MULT_NORMAL
                        } else if diff <= HARVEST_DIFF_HIGH {
                            HARVEST_MULT_HIGH
                        } else {
                            HARVEST_MULT_ZERO
                        };
                        let xp = ((base * mult).round() as i64).min(HARVEST_XP_MAX);
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
                            mk_item(
                                item,
                                Click::Harvest(id.clone(), domain::ACTION_SKIN.to_string()),
                            )
                        });
                    }
                    if harv.get(domain::ACTION_GUT).and_then(|v| v.as_bool()) == Some(true)
                        && harv.get("gut_done").and_then(|v| v.as_bool()) != Some(true)
                    {
                        let item = MenuItem::new(t(lang, Key::GutBtn));
                        menu = menu.item(if blocked {
                            item.icon(IconName::Lock).disabled(true)
                        } else {
                            mk_item(
                                item,
                                Click::Harvest(id.clone(), domain::ACTION_GUT.to_string()),
                            )
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
        menu
    }
}
