//! Окно персонажа gpui (M15/F25; порт листа M10.4): левая колонка —
//! шкала опыта, экипировка, атрибуты, статусы со временем; правая —
//! вкладки (параметры/навыки/профессии/репутация/квесты/гильдия;
//! инвентарь — отдельное окно рюкзака, M16/F29b). Модалки предмета/
//! улучшения/инкрустации — прежние. Оболочка всплывающих окон
//! (scrim + drag/resize) — `Root::popup_window`.
//!
//! Правило дубликатов: атрибуты/статусы живут только в левой колонке —
//! одноимённых вкладок больше нет.
//!
//! Отличия от egui: инвентарь без drag&drop (действия в меню по ПКМ —
//! те же, что в контекстном меню egui), тултипы заменены модалкой
//! описания, combobox обучения — ряд кнопок. Состояние вкладки —
//! `Root::sheet_tab` (персистентно в config, ФР-19).

use ely::overlays::Dialog;
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, Div, DragMoveEvent, EmptyView, MouseButton,
    SharedString, Window,
};
use ld2_core::l10n::{format_duration, t, tf, Key, Lang};
use ld2_core::store::{
    crime_kind_title, filter_active_statuses, local_epoch, Act, Screen, UpgradeView,
};
use ld2_protocol::cmd::{
    CharXpInvest, CraftLearn, CraftRecipes, CraftStart, CrimeLog, GuildBrowse, GuildLeave,
    GuildRankSet, ItemDrop, ItemEquip, ItemUnequip, ItemUse, SocketExtract, SocketInsert,
    UpgradePreview, UpgradeStart,
};
use ld2_protocol::types::{CharacterState, ItemSnapshot, Severity, SkillSnapshot};

use crate::palette::Palette;
use ely::buttons::IconButton;
use ely::forms::{Choice, Input, Select};
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip};

use super::quests::goals_summary;
use super::{
    act_btn, char_card_origin, clamp_char_size, scrollable_tab, PopupDrag, PopupKind, PopupResize,
    Root,
};

/// Вкладки окна персонажа (M15/F25; персистентно строкой в config).
/// Порядок — как в правой колонке окна (спека пользователя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTab {
    /// Производные характеристики — наполняется в F26.
    Params,
    Skills,
    Professions,
    /// Репутация фракций — наполняется в F27.
    Reputation,
    // M18/F36: «Квесты» переехали в отдельное окно (правило дубликатов).
    Guild,
}

impl SheetTab {
    /// M16/F29b: «Инвентарь» убран из окна — он живёт в отдельном
    /// окне рюкзака (правило дубликатов).
    fn all() -> [(SheetTab, Key); 5] {
        [
            (SheetTab::Params, Key::SheetParams),
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Professions, Key::ProfessionsTab),
            (SheetTab::Reputation, Key::SheetReputation),
            (SheetTab::Guild, Key::GuildTab),
        ]
    }

    /// M14/F12: иконка вкладки (Lucide-бандл Ely) — метка не меняется.
    pub(crate) fn icon(self) -> IconName {
        match self {
            SheetTab::Params => IconName::Gauge,
            SheetTab::Skills => IconName::Zap,
            SheetTab::Professions => IconName::Wrench,
            SheetTab::Reputation => IconName::Shield,
            SheetTab::Guild => IconName::Flag,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SheetTab::Params => "params",
            SheetTab::Skills => "skills",
            SheetTab::Professions => "professions",
            SheetTab::Reputation => "reputation",
            SheetTab::Guild => "guild",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "skills" => SheetTab::Skills,
            "professions" => SheetTab::Professions,
            "reputation" => SheetTab::Reputation,
            "guild" => SheetTab::Guild,
            // M15/F25: старые значения удалённых вкладок (attrs/statuses)
            // из сохранённого конфига → «Параметры» (ближайшая инфо-вкладка);
            // M16/F29b: «inventory» переехал в окно рюкзака;
            // M18/F36: «quests» переехал в отдельное окно.
            "attrs" | "statuses" | "inventory" | "quests" => SheetTab::Params,
            _ => SheetTab::Params,
        }
    }
}

/// M15/F28: группы навыков в порядке GDD §3 (A–F); ид — значение
/// `skills.skill_group` из контента, ключ — заголовок группы.
const SKILL_GROUPS: [(&str, Key); 6] = [
    ("weapon", Key::SkillGroupWeapon),
    ("defense", Key::SkillGroupDefense),
    ("magic", Key::SkillGroupMagic),
    ("stealth", Key::SkillGroupStealth),
    ("practice", Key::SkillGroupPractice),
    ("social", Key::SkillGroupSocial),
];

/// Демо-каталог профессий для обучения (skills.md §5; сервер валидирует).
const LEARN_CATALOG: [(&str, Key); 7] = [
    ("blacksmith", Key::ProfBlacksmith),
    ("alchemist", Key::ProfAlchemist),
    ("woodcarver", Key::ProfWoodcarver),
    ("tailor", Key::ProfTailor),
    ("cook", Key::ProfCook),
    ("herbalist", Key::ProfHerbalist),
    ("hunter", Key::ProfHunter),
];

/// M18/F32: типы целей инкрустации — руна идёт только в оружие
/// (`weapon`/`staff`), самоцвет (`gem`) — только в броню (`armor`).
/// Общая для окна «Куда инкрустировать…» и окна сокетов; сервер
/// проверяет то же правило (`socket.insert`, истина §7).
pub(crate) fn inscribe_target_allowed(insert_type: &str, target_type: &str) -> bool {
    match insert_type {
        "gem" => target_type == "armor",
        "rune" => matches!(target_type, "weapon" | "staff"),
        _ => false,
    }
}

impl Root {
    /// M11.2: диалоги листа (ФР-07/09/10): предмет/улучшение/инкрустация.
    pub(crate) fn sheet_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(detail) = self.store.item_detail.clone() {
            let name = detail["name"].as_str().unwrap_or("?").to_string();
            let body = self.item_view(cx, pal, detail).into_any_element();
            let close = self.dismiss(cx, |r| r.store.item_detail = None);
            return Some(
                Dialog::new("ld2-item", name, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(view) = self.store.upgrade.clone() {
            let title = view.item_name.clone();
            let body = self.upgrade_view(cx, pal, &view).into_any_element();
            let close = self.dismiss(cx, |r| r.store.upgrade = None);
            return Some(
                Dialog::new("ld2-upgrade", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(staff_uid) = self.store.socketing.clone() {
            let title = t(self.lang, Key::SocketingTitle).to_string();
            let body = self.socket_view(cx, pal, staff_uid).into_any_element();
            let close = self.dismiss(cx, |r| r.store.close_socketing());
            return Some(
                Dialog::new("ld2-socket", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        // M17/F30a: выбор оружия под инкрустацию руны (пункт меню руны).
        if let Some(rune_uid) = self.store.inscribe.clone() {
            let title = t(self.lang, Key::InscribePick).to_string();
            let body = self.inscribe_view(cx, pal, &rune_uid).into_any_element();
            let close = self.dismiss(cx, |r| r.store.close_inscribe());
            return Some(
                Dialog::new("ld2-inscribe", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    /// M17/F30a: список оружия, куда можно инкрустировать руну —
    /// предметы с сокетами и хотя бы одним свободным; клик → каст-бар
    /// 3 с (root::start_inscribe), без отмены.
    pub(crate) fn inscribe_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        rune_uid: &str,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_1();
        let mut found = false;
        // M18/F32: тип вставки — сам выбранный предмет (rune|gem).
        let insert_type = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == rune_uid)
            .map(|i| i.item_type.clone())
            .unwrap_or_default();
        for item in &snap.char.inventory {
            // M18/F32: руна → оружие/посох, самоцвет → броня.
            if !inscribe_target_allowed(&insert_type, &item.item_type) {
                continue;
            }
            let Some(sockets) = item.sockets.as_ref() else {
                continue;
            };
            if !sockets.iter().any(|s| s.rune.is_none()) {
                continue;
            }
            found = true;
            let rune = rune_uid.to_string();
            let uid = item.uid.clone();
            col = col.child(
                div()
                    .id(format!("ld2-inscribe-{uid}"))
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_inscribe(rune.clone(), uid.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_color(rgb(rarity_color(&item.rarity, pal)))
                            .child(format!("{} {}", item.name, badges(item))),
                    ),
            );
        }
        if !found {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::InscribeNoWeapons)),
            );
        }
        col
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
            if self.sheet_tab == SheetTab::Professions && !self.store.recipes_loaded {
                self.store.recipes_loaded = true;
                self.send_cmd(&CraftRecipes { profession: None }, Act::Craft);
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
        Some(self.popup_window(window, cx, pal, PopupKind::Character, title, body, None))
    }

    /// M16/F29b: оболочка всплывающего окна (механика M15/F25b–g):
    /// слой с карточкой — drag за шапку, resize уголком; `body` —
    /// содержимое под шапкой. Скрим общий для всех окон (могут быть
    /// открыты одновременно) — `Root::popup_scrim`; геометрия и
    /// флаги жестов — `PopupState` вида `kind`.
    // Оболочка окна: контекст (окно/cx/палитра) + вид + заголовок +
    // тело + подвал — набор неизменен, кроме растущих частей.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn popup_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        kind: PopupKind,
        title: impl Into<SharedString>,
        body: Div,
        // M16/F29g: нижняя панель под телом (по аналогии с шапкой).
        footer: Option<Div>,
    ) -> AnyElement {
        let lang = self.lang;
        let title = title.into();
        let (w, h) = self.popup(kind).size;
        let delta = self.popup(kind).delta;
        let id = kind.id();

        // M15/F25c: шапка — drag-регион (захват указателя gpui:
        // события идут, даже когда курсор ушёл за карточку).
        let drag_this = cx.entity();
        let top = div()
            .id(format!("{id}-titlebar"))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .cursor_grab()
            .px_3()
            .py_2()
            // M16/F29f: шапка встык к верхней кромке окна (скругление
            // карточки срезает overflow_hidden) и слегка подсвечена.
            .bg(rgb(pal.selection))
            .border_b_1()
            .border_color(rgb(pal.border))
            // M15/F25e: жест начат на шапке — scrim его не закроет.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(
                IconButton::new(format!("{id}-close"), IconName::X)
                    .tooltip(t(lang, Key::BtnClose))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        root.popup_mut(kind).open = false;
                        cx.notify();
                    })),
            )
            // Захват: запоминаем смещение указателя внутри шапки
            // (конструктор on_drag получает offset от gpui).
            .on_drag(PopupDrag, move |_value, offset, _window, acx| {
                drag_this.update(acx, |root, cx| {
                    // M16/F29b: жест принадлежит этому окну — чужой
                    // on_drag_move на общем payload не должен ехать.
                    root.drag_owner = Some(kind);
                    root.popup_mut(kind).drag_offset =
                        Some((f32::from(offset.x), f32::from(offset.y)));
                    cx.notify();
                });
                acx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |root, event: &DragMoveEvent<PopupDrag>, window, cx| {
                    if root.drag_owner != Some(kind) {
                        return;
                    }
                    let st = root.popup_mut(kind);
                    let Some(offset) = st.drag_offset else {
                        return;
                    };
                    let position = event.event.position;
                    let viewport = window.viewport_size();
                    let (w, h) = st.size;
                    let base_x = ((f32::from(viewport.width) - w) / 2.0).max(0.0);
                    let base_y = ((f32::from(viewport.height) - h) / 2.0).max(0.0);
                    // M16/F29f: шапка встык с кромкой карточки —
                    // её левый верх = указатель − offset (без −12).
                    st.delta.0 = f32::from(position.x) - offset.0 - base_x;
                    st.delta.1 = f32::from(position.y) - offset.1 - base_y;
                    cx.notify();
                },
            ));

        // M15/F25c: уголок изменения размера (правый нижний).
        let grip_this = cx.entity();
        let resize_grip = div()
            .id(format!("{id}-resize"))
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(18.0))
            .rounded_md()
            .border_l_1()
            .border_t_1()
            .border_color(rgb(pal.border))
            .bg(rgb(pal.panel))
            .cursor_pointer()
            // M15/F25e: жест начат на уголке — scrim не закроет.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            .on_drag(PopupResize, move |_value, _offset, _window, acx| {
                grip_this.update(acx, |root, cx| {
                    // M16/F29b: жест принадлежит этому окну; новый
                    // захват — новый якорь (иначе скачок по остатку).
                    root.drag_owner = Some(kind);
                    root.popup_mut(kind).resize_anchor = None;
                    cx.notify();
                });
                acx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |root, event: &DragMoveEvent<PopupResize>, window, cx| {
                    if root.drag_owner != Some(kind) {
                        return;
                    }
                    let pos = (
                        f32::from(event.event.position.x),
                        f32::from(event.event.position.y),
                    );
                    let st = root.popup_mut(kind);
                    let (cur_w, cur_h) = st.size;
                    // Якорь — первый move (позицию захвата в конструктор
                    // on_drag не передаём); кадр движения теряем.
                    let Some((ax, ay, w0, h0, dx0, dy0)) = st.resize_anchor else {
                        st.resize_anchor =
                            Some((pos.0, pos.1, cur_w, cur_h, st.delta.0, st.delta.1));
                        return;
                    };
                    let viewport = window.viewport_size();
                    let (nw, nh) = clamp_char_size(
                        w0 + (pos.0 - ax),
                        h0 + (pos.1 - ay),
                        f32::from(viewport.width),
                        f32::from(viewport.height),
                    );
                    // Якорим левый верх угол: компенсируем центрирование
                    // Backdrop (resize от центра сдвигал бы оба края
                    // на Δ/2).
                    st.delta.0 = dx0 + (nw - w0) / 2.0;
                    st.delta.1 = dy0 + (nh - h0) / 2.0;
                    st.size = (nw, nh);
                    cx.notify();
                },
            ));

        // M15/F25d: absolute-позиция от слоя (F14-подход): центр
        // вьюпорта + смещение — марджины центрирования давали лишь
        // половину сдвига («резиновый» drag).
        let viewport = window.viewport_size();
        let (left, top_xy) = char_card_origin(
            f32::from(viewport.width),
            f32::from(viewport.height),
            w,
            h,
            delta,
        );
        let card = div()
            .id(format!("{id}-card"))
            // M15/F25g: hitbox карточки перекрывает scrim под ней —
            // клик в ЛЮБУЮ точку окна попадает в окно, а не в скрим
            // (иначе «пустые» места карточки без interactivity уходили
            // в scrim и закрывали окно «будто по пустоте»).
            .occlude()
            // M15/F25g: любой жест, начавшийся внутри карточки,
            // снимает scrim-флаг — закрыть может только полный клик,
            // целиком прошедший по scrim.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            .absolute()
            .left(px(left))
            .top(px(top_xy))
            .flex()
            .flex_col()
            .w(px(w))
            .h(px(h))
            .rounded_md()
            // M16/F29f: шапка прилегает к кромке — карточка срезает
            // её углы своим скруглением.
            .overflow_hidden()
            .border_1()
            .border_color(rgb(pal.border))
            .bg(rgb(pal.panel))
            .child(top)
            // M16/F29f: тело — с внутренними отступами (бывший
            // padding карточки переехал сюда).
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_h_0()
                    .p(px(12.0))
                    .child(body),
            )
            // M16/F29g: нижняя панель (по аналогии с верхней) — у рюкзака.
            .children(footer)
            .child(resize_grip);

        // M16/F29b: слой карточки (позиционирование); scrim общий
        // для всех окон — `Root::popup_scrim` лежит под карточками,
        // клик мимо закрывает верхнее открытое окно.
        div()
            .id(format!("{id}-dialog-layer"))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(card)
            .into_any_element()
    }

    /// M15/F25: левая колонка окна — шкала опыта, экипировка,
    /// разделитель, статусы со временем (+ журнал нарушений);
    /// M18: атрибуты перенесены во вкладку «Навыки».
    fn character_left_column(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        // M16/F29f: справа запас под скроллбар — цифры значений не
        // налазили на индикатор прокрутки.
        let mut col = div().flex().flex_col().gap_2().pr(px(16.0));
        col = col.child(self.character_xp_block(pal));
        col = col.child(self.equip_block(cx, pal));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(self.statuses_block(cx, pal));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(360.0))
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
    fn character_xp_block(&self, pal: &Palette) -> Div {
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
    fn character_tabs(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
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
            SheetTab::Professions => self.tab_professions(cx, pal),
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
            SheetTab::Professions => {
                scrollable_tab("ld2-professions-scroll", &self.professions_scroll, body)
            }
            _ => body,
        });
        col
    }

    /// M15/F26: вкладка «Параметры» — 16 производных характеристик
    /// (серверные `derived`; у сервера без M15 — заглушка).
    fn tab_params(&self, pal: &Palette) -> Div {
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

    /// Отправка команды листа (паритет egui `send`).
    pub(crate) fn send_cmd<C: ld2_protocol::Command>(&mut self, cmd: &C, act: Act) {
        match self.transport.send(cmd) {
            Ok(_) => self.store.on_sheet_sent(act),
            Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
        }
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

    // ── вкладка «Навыки» (ФР-04) ──────────────────────────────────────────

    fn tab_skills(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
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
        // если в снапшоте есть хоть один её навык.
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
            col = col.child(
                div()
                    .mt(px(6.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, key)),
            );
            for skill in group {
                col = col.child(self.skill_row(cx, pal, skill, pool));
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
            col = col.child(self.skill_row(cx, pal, skill, pool));
        }
        col
    }

    /// M15/F28: строка навыка (перенос из таба) — название/ранг,
    /// тултип описания, ⓘ-карточка, прогресс и вложение XP из пула.
    fn skill_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        skill: &SkillSnapshot,
        pool: i64,
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
        // Вложение очков из пула (D3/D4, ФР-04).
        if pool > 0 {
            let mut buttons = div().flex().flex_row().gap_1();
            for (label, points) in [("+1", 1_i64), ("+5", 5_i64)] {
                let sid = id.clone();
                buttons =
                    buttons.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                        this.invest(&sid, points.min(pool));
                        let _ = cx;
                    }));
            }
            let sid = id.clone();
            buttons = buttons.child(act_btn(
                cx,
                t(lang, Key::MaxLabel),
                pal.panel,
                pal.text,
                move |this, cx| {
                    this.invest(&sid, pool);
                    let _ = cx;
                },
            ));
            row = row.child(buttons);
        }
        row
    }

    fn invest(&mut self, skill_id: &str, points: i64) {
        let cmd = CharXpInvest {
            skill_id: skill_id.to_string(),
            points,
        };
        self.send_cmd(&cmd, Act::Invest);
    }

    // ── вкладка «Атрибуты» ────────────────────────────────────────────────

    /// M15/F25: атрибуты — середина левой колонки (перенос из вкладки
    /// «Атрибуты» по правилу дубликатов) — с заголовком секции.
    fn attrs_block(&self, pal: &Palette) -> Div {
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

    // ── вкладка «Инвентарь» (ФР-07) ───────────────────────────────────────

    /// M15/F25: экипировка — верх левой колонки (перенос из «Инвентаря»
    /// по правилу дубликатов): слоты с действиями снятия/описания.
    fn equip_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let equip_slots: Vec<String> = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.equip_slots.clone())
            .unwrap_or_default();

        let mut col = div().flex().flex_col().gap_2();

        // Слоты экипировки (ФР-07): вместо drag&drop — действия в строке.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::EquipLabel)),
        );
        for slot in &equip_slots {
            let uid = snap.char.equip.get(slot).cloned().flatten();
            let item = uid
                .as_ref()
                .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
            // M16/F29d: подпись слева, предмет — по правому краю;
            // кнопки «Снять»/«Описание» убраны в контекстное меню.
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(72.0))
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(slot_label(lang, slot)),
                );
            let Some(item) = item else {
                // M17/F30b: левая рука закрыта, пока в правой двуручник.
                let twohand = snap
                    .char
                    .equip
                    .get("main_hand")
                    .cloned()
                    .flatten()
                    .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == u))
                    .is_some_and(|i| i.hands.unwrap_or(1) >= 2);
                let blocked = slot == "off_hand" && twohand;
                row = row.child(
                    div()
                        .flex_grow_1()
                        .text_size(px(12.0))
                        .text_color(rgb(if blocked { pal.warn } else { pal.muted }))
                        .child(if blocked {
                            t(lang, Key::SlotBlocked)
                        } else {
                            t(lang, Key::EmptySlot)
                        }),
                );
                col = col.child(row);
                continue;
            };
            let color = rarity_color(&item.rarity, pal);
            // Отступ-заполнитель прижимает предмет к правому краю.
            row =
                row.child(div().flex_grow_1())
                    .child(div().text_color(rgb(color)).child(format!(
                        "{} {}",
                        item.name,
                        badges(item)
                    )));
            // Меню предмета (F29d): Описание и Снять — вместо кнопок.
            let mut menu = Menu::new();
            let detail = serde_json::to_value(item).unwrap_or_default();
            menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
                this.store.item_detail = Some(detail.clone());
            }));
            let slot_for_cmd = slot.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::MenuUnequip),
                move |this, _cx| {
                    let cmd = ItemUnequip {
                        slot: slot_for_cmd.clone(),
                    };
                    this.send_cmd(&cmd, Act::Unequip);
                },
            ));
            // Запрос меню: левый и правый клик по строке (номер растёт —
            // Ely переоткрывает; в manual-режиме host-правый клик выключен).
            let request = match &self.equip_menu {
                Some((s, n, at)) if s == slot => Some((*n, *at)),
                _ => None,
            };
            let slot_right = slot.clone();
            let slot_left = slot.clone();
            let row = ContextMenu::new(format!("ld2-equip-ctx-{slot}"), menu)
                .manual(request)
                .child(
                    row.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            window.prevent_default();
                            this.bump_equip_menu(slot_right.clone(), event.position, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            window.prevent_default();
                            this.bump_equip_menu(slot_left.clone(), event.position, cx);
                        }),
                    ),
                );
            col = col.child(row);
        }

        col
    }

    /// Действия предмета — контекстное меню Ely (ФР-07/09/10, egui-паритет).
    pub(crate) fn item_actions(
        &mut self,
        cx: &mut Context<'_, Self>,
        _pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
    ) -> Menu {
        let lang = self.lang;
        let mut menu = Menu::new();

        // Использовать (свиток/зелье/…).
        let uid = item.uid.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuUse), move |this, _cx| {
            let cmd = ItemUse {
                item_uid: uid.clone(),
                target_id: None,
            };
            this.send_cmd(&cmd, Act::Drop);
        }));

        // M17/F30a/F30c: инкрустация руны ИЛИ самоцвета — окно
        // выбора предмета + каст-бар.
        if matches!(item.item_type.as_str(), "rune" | "gem") {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::MenuInscribe),
                move |this, _cx| {
                    this.store.open_inscribe(uid.clone());
                },
            ));
        }

        // M17/F30b: экипировка — по слоту предмета; сервер — истина (§7).
        if let Some(slot) = slot_for(item, ch) {
            let equipped = ch.equip.values().flatten().any(|u| u == &item.uid);
            if !equipped {
                let uid = item.uid.clone();
                menu = menu.item(menu_item(cx, t(lang, Key::MenuEquip), move |this, _cx| {
                    let cmd = ItemEquip {
                        item_uid: uid.clone(),
                        slot: slot.clone(),
                    };
                    this.send_cmd(&cmd, Act::Equip);
                }));
            }
        }

        // Улучшение (ФР-10): до максимума из конфига сервера.
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(item.item_type.as_str(), "weapon" | "armor" | "staff");
        if upgradable && item.upgrade < max_level {
            let uid = item.uid.clone();
            let name = item.name.clone();
            menu = menu.item(menu_item(cx, t(lang, Key::UpgradeBtn), move |this, _cx| {
                this.store.open_upgrade(uid.clone(), name.clone());
                let cmd = UpgradePreview {
                    item_uid: uid.clone(),
                };
                this.send_cmd(&cmd, Act::Preview);
            }));
        }

        // Инкрустация (ФР-09): предмет с сокетами — посох или меч
        // (M17/F30a: сокеты у любого оружия).
        if item.sockets.is_some() {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::BtnSocketing),
                move |this, _cx| {
                    this.store.open_socketing(uid.clone());
                },
            ));
        }

        // Описание / Выбросить.
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
            this.store.item_detail = Some(detail.clone());
        }));
        let uid = item.uid.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDrop), move |this, _cx| {
            let cmd = ItemDrop {
                item_uid: uid.clone(),
                qty: None,
            };
            this.send_cmd(&cmd, Act::Drop);
        }));
        menu
    }

    // ── вкладка «Статусы» (ФР-11, M6) ────────────────────────────────────

    /// M15/F25: статусы со временем + журнал нарушений — низ левой
    /// колонки (перенос из вкладки «Статусы» по правилу дубликатов).
    fn statuses_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
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

    // ── вкладка «Профессии» (ФР-08) ───────────────────────────────────────

    fn tab_professions(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_2();

        // Изученные профессии: ранги и XP (ФР-08).
        for prof in &snap.char.professions {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(div().child(prof_label(lang, &prof.id)))
                            .child(div().text_color(rgb(pal.muted)).child(
                                t(lang, Key::RankFmt).replace("{r}", &prof.rank.to_string()),
                            )),
                    )
                    .child(bar(
                        if prof.xp_to_next > 0 {
                            prof.xp as f32 / prof.xp_to_next as f32
                        } else {
                            0.0
                        },
                        format!("{}/{}", prof.xp, prof.xp_to_next),
                        pal.accent,
                        pal,
                    )),
            );
        }

        // M11.3: обучение — Select Ely + подтверждение (отклонение
        // «ряд кнопок вместо combobox» снят).
        let available: Vec<(&'static str, Key)> = LEARN_CATALOG
            .iter()
            .copied()
            .filter(|(id, _)| !snap.char.professions.iter().any(|p| p.id == *id))
            .collect();
        if !available.is_empty() {
            let choices: Vec<Choice> = available
                .iter()
                .map(|(id, key)| Choice::new(*id, t(lang, *key)))
                .collect();
            let pick = self.learn_pick.clone();
            let this = cx.entity();
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(div().flex_grow_1().child({
                let this = this.clone();
                let sel =
                    Select::new("ld2-learn-prof", choices).placeholder(t(lang, Key::LearnLabel));
                match pick {
                    Some(value) => sel.selected(value),
                    None => sel,
                }
                .on_change(move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, _cx| root.learn_pick = Some(value));
                })
            }));
            if let Some(id) = self.learn_pick.clone() {
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::LearnLabel),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftLearn {
                            profession_id: id.clone(),
                        };
                        this.send_cmd(&cmd, Act::Learn);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::RecipesLabel)),
        );
        if self.store.recipes.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoRecipes)),
            );
        }

        // Рецепты: требования, ингредиенты (красные при нехватке — ФР-08).
        let recipes = self.store.recipes.clone();
        for recipe in &recipes {
            let recipe_id = recipe["id"].as_str().unwrap_or_default().to_string();
            let name = recipe["name"].as_str().unwrap_or("?").to_string();
            let profession = recipe["profession"].as_str().unwrap_or("").to_string();
            let rank = recipe["rank"].as_i64().unwrap_or(0);
            let station = recipe["station_label"].as_str().unwrap_or("").to_string();

            let mut ingredients_ok = true;
            let mut ingredient_lines: Vec<(String, bool)> = Vec::new();
            if let Some(inputs) = recipe["inputs"].as_array() {
                for input in inputs {
                    let need_type = input["type"].as_str().unwrap_or_default();
                    let need_qty = input["qty"].as_i64().unwrap_or(1);
                    let have: i64 = snap
                        .char
                        .inventory
                        .iter()
                        .filter(|i| i.item_type == need_type)
                        .map(|i| i.qty.unwrap_or(1))
                        .sum();
                    let ok = have >= need_qty;
                    if !ok {
                        ingredients_ok = false;
                    }
                    ingredient_lines.push((format!("{need_type}: {have}/{need_qty}"), ok));
                }
            }

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
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(div().child(name.clone()))
                        .child(div().text_size(px(12.0)).text_color(rgb(pal.muted)).child(
                            format!(
                                "· {} · {}",
                                prof_label(lang, &profession),
                                t(lang, Key::RankFmt).replace("{r}", &rank.to_string())
                            ),
                        )),
                );

            // Крафт: кнопка активна при полном наборе и вне ожидания (§7).
            let pending = self.store.pending == Some(Act::Craft);
            if ingredients_ok && !pending {
                card = card.child(act_btn(
                    cx,
                    t(lang, Key::BtnCraft),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftStart {
                            recipe_id: recipe_id.clone(),
                            qty: 1,
                        };
                        this.send_cmd(&cmd, Act::Craft);
                        let _ = cx;
                    },
                ));
            } else {
                card = card.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::BtnCraft)),
                );
            }

            if !station.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(station),
                );
            }
            for (line, ok) in ingredient_lines {
                card = card.child(if ok {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {line}"))
                } else {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.danger))
                        .child(format!("• {}: {}", t(lang, Key::MissingIngredients), line))
                });
            }
            col = col.child(card);
        }
        col
    }

    // ── вкладка «Гильдия» (ФР-16, M8) ─────────────────────────────────────

    fn tab_guild(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let target = self.store.target.clone();
        let guild = self.store.guild.clone();
        let my_id = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.id.clone())
            .unwrap_or_default();

        let Some(g) = guild else {
            // Не в гильдии: создать или вступить по списку мира (§7).
            let mut col = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::GuildNone)),
                )
                .child(
                    div()
                        .w(px(320.0))
                        .child(Input::new(self.ti(&self.guild_input))),
                )
                .child(act_btn(
                    cx,
                    t(lang, Key::GuildCreateBtn),
                    pal.accent,
                    pal.accent_text,
                    |this, cx| {
                        let name = this.ti(&this.guild_input).read(cx).text().to_string();
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            let cmd = ld2_protocol::cmd::GuildCreate { name };
                            this.send_cmd(&cmd, Act::Guild);
                        }
                    },
                ));
            col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
            col = col.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::GuildBrowseLbl)),
            );
            if self.store.guild_browse.is_empty() {
                col = col.child(div().text_color(rgb(pal.muted)).child("…"));
            }
            for entry in self.store.guild_browse.clone() {
                let id = entry["id"].as_str().unwrap_or("").to_string();
                let name = entry["name"].as_str().unwrap_or("?").to_string();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_2()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().child(name))
                        .child(act_btn(
                            cx,
                            t(lang, Key::GuildJoinBtn),
                            pal.accent,
                            pal.accent_text,
                            move |this, cx| {
                                let cmd = ld2_protocol::cmd::GuildJoin {
                                    guild_id: Some(id.clone()),
                                };
                                this.send_cmd(&cmd, Act::Guild);
                                let _ = cx;
                            },
                        )),
                );
            }
            return col;
        };

        // В гильдии: инфо, состав, операции, журнал.
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(g.name.clone()),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::GuildMembers)),
            );
        let i_am_master = g
            .members
            .iter()
            .any(|m| m.id == my_id && m.rank == "master");
        for member in &g.members {
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(div().flex_grow_1().child(member.name.clone()))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(if member.rank == "master" {
                            pal.warn
                        } else {
                            pal.muted
                        }))
                        .child(member.rank.clone()),
                );
            if member.online {
                row = row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.success))
                        .child("●"),
                );
            }
            if i_am_master && member.id != my_id {
                let pid = member.id.clone();
                let next = match member.rank.as_str() {
                    "master" => "officer",
                    "officer" => "member",
                    _ => "master",
                }
                .to_string();
                let pid2 = pid.clone();
                let rank = next.clone();
                row = row.child(act_btn(
                    cx,
                    format!("{} → {next}", t(lang, Key::GuildRankLbl)),
                    pal.panel,
                    pal.text,
                    move |this, cx| {
                        let cmd = GuildRankSet {
                            player_id: pid2.clone(),
                            rank: rank.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::GuildKickBtn),
                    pal.panel,
                    pal.danger,
                    move |this, cx| {
                        let cmd = ld2_protocol::cmd::GuildKick {
                            player_id: pid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        let mut ops = div().flex().flex_row().gap_1();
        if let Some(target) = &target {
            let tid = target.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::GuildInviteBtn),
                pal.panel,
                pal.text,
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::GuildInvite {
                        player_id: tid.clone(),
                    };
                    this.send_cmd(&cmd, Act::Guild);
                    let _ = cx;
                },
            ));
        }
        let gid = g.id.clone();
        ops = ops.child(act_btn(
            cx,
            t(lang, Key::GuildLeaveBtn),
            pal.panel,
            pal.danger,
            move |this, cx| {
                let cmd = GuildLeave {
                    guild_id: Some(gid.clone()),
                };
                this.send_cmd(&cmd, Act::Guild);
                let _ = cx;
            },
        ));
        col = col.child(ops);

        if !self.store.guild_log.is_empty() {
            col = col.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::GuildLogLbl)),
            );
            for entry in self.store.guild_log.iter().rev().take(5) {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", entry.text)),
                );
            }
        }
        col
    }

    // ── модалки (паритет egui-окон; вызываются из game_screen) ────────────

    /// Описание предмета (ФР-07).
    pub(crate) fn item_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        let lang = self.lang;
        let rarity = detail["rarity"].as_str().unwrap_or("common");
        let item_type = detail["type"].as_str().unwrap_or("?");
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .text_color(rgb(rarity_color(rarity, pal)))
                .child(format!(
                    "{} · {} · +{}",
                    rarity_label(lang, rarity),
                    type_label(lang, item_type),
                    detail["upgrade"].as_i64().unwrap_or(0)
                )),
        );
        if let Some(stats) = detail["stats"].as_object() {
            for (key, value) in stats {
                // M16/F29e: ключи и значения — на языке игрока
                // (школы — свои названия; прочие значения — как есть).
                let raw = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                let value = if key == "school" {
                    school_label(lang, &raw)
                } else if key == "attr" {
                    // M17/F30c: значение самоцвета — «str» → «Сила».
                    attr_label(lang, &raw)
                } else if key == "spell" {
                    // M17/F30f: id заклинания → название (без storm_bolt).
                    ld2_core::spells::spell_key(&raw)
                        .map(|k| t(lang, k).to_string())
                        .unwrap_or(raw)
                } else {
                    raw
                };
                col = col.child(div().child(format!("{}: {}", stat_label(lang, key), value)));
            }
        }
        if let Some(sockets) = detail["sockets"].as_array() {
            for socket in sockets {
                if let Some(rune) = socket["rune"].as_object() {
                    // M17/F30c: самоцвет — без зарядов (заряды у рун).
                    let label = if rune["spell"].as_str().is_some_and(|s| s.is_empty()) {
                        format!("◈ {}", rune["name"])
                    } else {
                        format!("◈ {}: {}/{}", rune["name"], rune["charges"], rune["max"])
                    };
                    col = col.child(div().child(label));
                } else {
                    col = col.child(div().text_color(rgb(pal.muted)).child("○ —"));
                }
            }
        }
        if let Some(value) = detail["gold_value"].as_i64() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("{value} зол.")),
            );
        }
        col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.item_detail = None;
                let _ = cx;
            },
        ))
    }

    /// Улучшение (ФР-10): «сейчас +N → после +M», шанс, стоимость (§7).
    pub(crate) fn upgrade_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        view: &UpgradeView,
    ) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Upgrade);
        let mut col = div().flex().flex_col().gap_1();
        let Some(preview) = view.preview.clone() else {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
            col = col.child(act_btn(
                cx,
                t(lang, Key::Exit),
                pal.panel,
                pal.text,
                |this, cx| {
                    this.store.close_upgrade();
                    let _ = cx;
                },
            ));
            return col;
        };

        let current = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.char
                    .inventory
                    .iter()
                    .find(|i| i.uid == view.item_uid)
                    .map(|i| i.upgrade)
            })
            .unwrap_or(0);
        let delta = preview["delta"]["upgrade"].as_str().unwrap_or("+?");
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("+{current} → {delta}")),
        );
        if let Some(chance) = preview["chance"].as_f64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {:.0}%",
                t(lang, Key::ChanceLabel),
                chance * 100.0
            )));
        }
        if let Some(gold) = preview["cost"]["gold"].as_i64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {} зол.",
                t(lang, Key::CostLabel),
                gold
            )));
        }
        if let Some(materials) = preview["cost"]["materials"].as_object() {
            for (name, qty) in materials {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("  • {name}: {qty}")),
                );
            }
        }

        let preview_id = preview["preview_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let can_start = !preview_id.is_empty() && !pending;
        let mut row = div().flex().flex_row().gap_2().mt(px(8.0));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_upgrade();
                let _ = cx;
            },
        ));
        let uid = view.item_uid.clone();
        if can_start {
            row = row.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.warn,
                0xffffff,
                move |this, cx| {
                    let cmd = UpgradeStart {
                        item_uid: uid.clone(),
                        preview_id: preview_id.clone(),
                    };
                    this.send_cmd(&cmd, Act::Upgrade);
                    let _ = cx;
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(if pending {
                        t(lang, Key::Loading)
                    } else {
                        t(lang, Key::UpgradeBtn)
                    }),
            );
        }
        col = col.child(row);
        col
    }

    /// Инкрустация посоха (ФР-09).
    pub(crate) fn socket_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div().size_full();
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            self.store.close_socketing();
            return div().size_full();
        };
        // M18/F32: от оружия/посоха — только руны, от брони — только
        // самоцветы; у цели иного типа вставлять нечего.
        let insert_type = match staff.item_type.as_str() {
            "weapon" | "staff" => "rune",
            "armor" => "gem",
            _ => "",
        };
        let runes: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| inscribe_target_allowed(insert_type, &i.item_type))
            .cloned()
            .collect();

        let mut col = div().flex().flex_col().gap_1();
        for socket in staff.sockets.iter().flatten() {
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("◈ {}", socket.index)),
            );
            match &socket.rune {
                Some(rune) => {
                    let index = socket.index;
                    // M17/F30c: самоцвет — без зарядов (заряды у рун).
                    let text = if rune.spell.is_empty() {
                        rune.name.clone()
                    } else {
                        format!("{} ({}/{})", rune.name, rune.charges, rune.max)
                    };
                    row = row.child(div().child(text));
                    let suid = staff.uid.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::ExtractRune),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = SocketExtract {
                                staff_uid: suid.clone(),
                                socket_index: index,
                            };
                            this.send_cmd(&cmd, Act::Socket);
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    if runes.is_empty() {
                        row = row.child(
                            div()
                                .text_color(rgb(pal.muted))
                                .child(t(lang, Key::NoRunes)),
                        );
                    }
                    for rune in &runes {
                        let index = socket.index;
                        let suid = staff.uid.clone();
                        let ruid = rune.uid.clone();
                        let label = format!("{}: {}", t(lang, Key::InsertRune), rune.name);
                        row =
                            row.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                                let cmd = SocketInsert {
                                    staff_uid: suid.clone(),
                                    socket_index: index,
                                    rune_uid: ruid.clone(),
                                };
                                this.send_cmd(&cmd, Act::Socket);
                                let _ = cx;
                            }));
                    }
                }
            }
            col = col.child(row);
        }

        col = col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_socketing();
                let _ = cx;
            },
        ));
        col
    }
}

// ── вспомогательные ────────────────────────────────────────────────────────

/// Название профессии: l10n-ключ по id, фолбэк — сам id.
fn prof_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "blacksmith" => Some(Key::ProfBlacksmith),
        "alchemist" => Some(Key::ProfAlchemist),
        "scribe" => Some(Key::ProfScribe),
        "woodcarver" => Some(Key::ProfWoodcarver),
        "tailor" => Some(Key::ProfTailor),
        "cook" => Some(Key::ProfCook),
        "herbalist" => Some(Key::ProfHerbalist),
        "hunter" => Some(Key::ProfHunter),
        _ => None,
    };
    key.map(|k| t(lang, k).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// M15/F25: строка «подпись … значение» в панели опыта окна персонажа.
fn xp_kv_row(pal: &Palette, label: &str, value: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .text_size(px(12.0))
        .child(div().text_color(rgb(pal.muted)).child(label.to_string()))
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(value.to_string()),
        )
}

/// Бейджи плитки: +N, сокеты ◈○, количество ×N (паритет egui).
pub(crate) fn badges(item: &ItemSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if item.upgrade > 0 {
        parts.push(format!("+{}", item.upgrade));
    }
    if let Some(sockets) = &item.sockets {
        let filled = sockets.iter().filter(|s| s.rune.is_some()).count();
        let empty = sockets.len() - filled;
        parts.push(format!("{}{}", "◈".repeat(filled), "○".repeat(empty)));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        parts.push(format!("×{qty}"));
    }
    parts.join(" ")
}

/// Редкость → цвет (палитра §7.4).
/// MenuItem с действием над Root (запись через entity: замыкания `on_click`
/// должны быть `'static`, а `&mut Root` живёт только в цикле сборки).
fn menu_item(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    body: impl Fn(&mut Root, &mut Context<'_, Root>) + 'static,
) -> MenuItem {
    let this = cx.entity();
    MenuItem::new(label).on_click(move |_window, acx| {
        this.update(acx, |root, rcx| {
            body(root, rcx);
            rcx.notify();
        });
    })
}

/// Цвет редкости предмета (палитра §7.4).
pub(crate) fn rarity_color(rarity: &str, pal: &Palette) -> u32 {
    match rarity {
        "common" => pal.muted,
        "uncommon" => 0x98_c3_79,
        "rare" => 0x61_af_ef,
        "epic" => 0xa0_52_2d,
        "legendary" => 0xe5_c0_7b,
        _ => pal.text,
    }
}

/// M16/F29e: название редкости (l10n; неизвестная — как есть).
pub(crate) fn rarity_label(lang: Lang, rarity: &str) -> String {
    match rarity {
        "common" => t(lang, Key::RarityCommon),
        "uncommon" => t(lang, Key::RarityUncommon),
        "rare" => t(lang, Key::RarityRare),
        "epic" => t(lang, Key::RarityEpic),
        "legendary" => t(lang, Key::RarityLegendary),
        other => other,
    }
    .to_string()
}

/// M17/F30d: имя основной характеристики (l10n; харизма убрана,
/// неизвестный ключ — как есть).
pub(crate) fn attr_label(lang: Lang, key: &str) -> String {
    match key {
        "str" => t(lang, Key::AttrStr),
        "dex" => t(lang, Key::AttrDex),
        "vit" => t(lang, Key::AttrVit),
        "int" => t(lang, Key::AttrInt),
        other => other,
    }
    .to_string()
}

/// M16/F29e: слот экипировки (конфиг открытый — fallback сырой).
pub(crate) fn slot_label(lang: Lang, slot: &str) -> String {
    match slot {
        "head" => t(lang, Key::SlotHead),
        "body" => t(lang, Key::SlotBody),
        "main_hand" => t(lang, Key::SlotMainHand),
        "staff" => t(lang, Key::SlotStaff),
        "feet" => t(lang, Key::SlotFeet),
        "shoulders" => t(lang, Key::SlotShoulders),
        "bracers" => t(lang, Key::SlotBracers),
        "gloves" => t(lang, Key::SlotGloves),
        "legs" => t(lang, Key::SlotLegs),
        "cloak" => t(lang, Key::SlotCloak),
        "ring1" => t(lang, Key::SlotRing1),
        "ring2" => t(lang, Key::SlotRing2),
        "amulet" => t(lang, Key::SlotAmulet),
        "off_hand" => t(lang, Key::SlotOffHand),
        "artifact1" => t(lang, Key::SlotArtifact1),
        "artifact2" => t(lang, Key::SlotArtifact2),
        other => other,
    }
    .to_string()
}

/// M16/F29e: тип предмета (контент открытый — fallback сырой).
pub(crate) fn type_label(lang: Lang, item_type: &str) -> String {
    match item_type {
        "weapon" => t(lang, Key::ItemTypeWeapon),
        "staff" => t(lang, Key::ItemTypeStaff),
        "potion" => t(lang, Key::ItemTypePotion),
        "rune" => t(lang, Key::ItemTypeRune),
        "scroll" => t(lang, Key::ItemTypeScroll),
        "armor" => t(lang, Key::ItemTypeArmor),
        "money" => t(lang, Key::ItemTypeMoney),
        "herb" => t(lang, Key::ItemTypeHerb),
        "wood_oak" => t(lang, Key::ItemTypeWoodOak),
        "parchment" => t(lang, Key::ItemTypeParchment),
        "ink" => t(lang, Key::ItemTypeInk),
        "flask" => t(lang, Key::ItemTypeFlask),
        "chisel" => t(lang, Key::ItemTypeChisel),
        "quill" => t(lang, Key::ItemTypeQuill),
        "shield" => t(lang, Key::ItemTypeShield),
        "artifact" => t(lang, Key::ItemTypeArtifact),
        "gem" => t(lang, Key::ItemTypeGem),
        other => other,
    }
    .to_string()
}

/// M16/F29e: характеристика предмета (`stats`; `dmg` и `damage` —
/// один ключ). Значения школ переводятся отдельно.
pub(crate) fn stat_label(lang: Lang, stat: &str) -> String {
    match stat {
        "dmg" | "damage" => t(lang, Key::StatDamage),
        "heal" => t(lang, Key::StatHeal),
        "armor" => t(lang, Key::StatArmor),
        "magic" => t(lang, Key::StatMagic),
        "spell" => t(lang, Key::StatSpell),
        "school" => t(lang, Key::StatSchool),
        "charges" => t(lang, Key::StatCharges),
        "max" => t(lang, Key::StatMax),
        "attr" => t(lang, Key::StatAttr),
        "value" => t(lang, Key::StatValue),
        other => other,
    }
    .to_string()
}

/// M16/F29e: школа магии (значение `school`; fallback — сырое).
pub(crate) fn school_label(lang: Lang, school: &str) -> String {
    match school {
        "fire" => t(lang, Key::SchoolFire),
        "frost" => t(lang, Key::SchoolFrost),
        "storm" => t(lang, Key::SchoolStorm),
        other => other,
    }
    .to_string()
}

/// M17/F30b: слот для предмета — из поля `slot` снапшота (парные
/// ring/artifact занимают первую свободную ячейку); `None` — предмет
/// не экипируется либо пара занята целиком. Сервер — истина (§7).
fn slot_for(item: &ItemSnapshot, ch: &CharacterState) -> Option<String> {
    let slot = item.slot.as_deref()?;
    let free = |s: &str| ch.equip.get(s).map(|v| v.is_none()).unwrap_or(true);
    match slot {
        "ring" => ["ring1", "ring2"]
            .into_iter()
            .find(|s| free(s))
            .map(str::to_string),
        "artifact" => ["artifact1", "artifact2"]
            .into_iter()
            .find(|s| free(s))
            .map(str::to_string),
        other => Some(other.to_string()),
    }
}

/// Цвет severity (палитра §7.4).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
pub(crate) fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F32: руна идёт только в оружие (weapon/staff), самоцвет —
    /// только в броню (armor); неизвестный тип вставки — ничего.
    #[test]
    fn inscribe_target_allowed_by_insert_type() {
        assert!(inscribe_target_allowed("rune", "weapon"));
        assert!(inscribe_target_allowed("rune", "staff"));
        assert!(!inscribe_target_allowed("rune", "armor"), "руна в броню");
        assert!(!inscribe_target_allowed("rune", "shield"));
        assert!(inscribe_target_allowed("gem", "armor"));
        assert!(
            !inscribe_target_allowed("gem", "weapon"),
            "самоцвет в оружие"
        );
        assert!(!inscribe_target_allowed("gem", "staff"));
        assert!(!inscribe_target_allowed("", "weapon"), "нет типа вставки");
        assert!(
            !inscribe_target_allowed("potion", "weapon"),
            "не инкрустация"
        );
    }

    /// M16/F29e: лейблы переводят контент и падают на сырое значение
    /// для открытых строк (новый тип/слот/статс/школа).
    #[test]
    fn labels_translate_and_fall_back() {
        let ru = Lang::Ru;
        assert_eq!(rarity_label(ru, "rare"), "Редкий");
        assert_eq!(rarity_label(ru, "mythic"), "mythic", "fallback");
        assert_eq!(slot_label(ru, "main_hand"), "Правая рука");
        assert_eq!(slot_label(ru, "shoulders"), "Наплечники");
        assert_eq!(slot_label(ru, "artifact1"), "Артефакт 1");
        assert_eq!(slot_label(Lang::En, "off_hand"), "Off hand");
        assert_eq!(slot_label(ru, "tail"), "tail", "fallback");
        assert_eq!(type_label(ru, "money"), "Валюта");
        assert_eq!(type_label(ru, "shield"), "Щит");
        assert_eq!(type_label(ru, "artifact"), "Артефакт");
        assert_eq!(type_label(ru, "saddle"), "saddle", "fallback");
        assert_eq!(stat_label(ru, "dmg"), "Урон");
        assert_eq!(stat_label(ru, "damage"), "Урон", "dmg и damage — один ключ");
        assert_eq!(stat_label(ru, "piety"), "piety", "fallback");
        assert_eq!(school_label(ru, "fire"), "Огонь");
        assert_eq!(school_label(ru, "storm"), "Шторм");
        assert_eq!(school_label(ru, "necro"), "necro", "fallback");
    }

    /// M17/F30d: основные характеристики переведены; харизма (cha)
    /// убрана — неизвестный ключ падает на сырое значение.
    #[test]
    fn attr_labels_translate_and_fall_back() {
        let ru = Lang::Ru;
        let en = Lang::En;
        assert_eq!(attr_label(ru, "str"), "Сила");
        assert_eq!(attr_label(ru, "dex"), "Ловкость");
        assert_eq!(attr_label(ru, "vit"), "Стойкость");
        assert_eq!(attr_label(ru, "int"), "Интеллект");
        assert_eq!(attr_label(en, "str"), "Strength");
        assert_eq!(attr_label(en, "int"), "Intellect");
        assert_eq!(attr_label(ru, "cha"), "cha", "харизма убрана");
    }

    /// M15/F28: группы — в порядке GDD §3, иды уникальны.
    #[test]
    fn skill_groups_match_gdd_order() {
        let ids: Vec<&str> = SKILL_GROUPS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            ["weapon", "defense", "magic", "stealth", "practice", "social"],
            "порядок групп как в skills.md §3 (A–F)"
        );
        let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "иды групп уникальны");
    }

    /// M15/F25: состав и порядок вкладок окна — по спеке пользователя
    /// (Параметры | Навыки | Профессии | Репутация + нетронутые);
    /// M16/F29b: «Инвентарь» убран (окно рюкзака — отдельное).
    #[test]
    fn character_window_tabs_cover_spec() {
        let tabs: Vec<&'static str> = SheetTab::all().iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tabs,
            ["params", "skills", "professions", "reputation", "guild"],
            "порядок вкладок правой колонки"
        );
        for (tab, _) in SheetTab::all() {
            assert_eq!(SheetTab::parse(tab.as_str()), tab, "round-trip");
        }
        // Старые значения удалённых вкладок (правило дубликатов)
        // из сохранённого конфига маппятся на «Параметры».
        assert_eq!(SheetTab::parse("attrs"), SheetTab::Params);
        assert_eq!(SheetTab::parse("statuses"), SheetTab::Params);
        // M18/F36: вкладка «Квесты» переехала в отдельное окно.
        assert_eq!(SheetTab::parse("quests"), SheetTab::Params);
        assert_eq!(SheetTab::parse("inventory"), SheetTab::Params);
    }
}
