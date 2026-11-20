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
    div, prelude::*, px, rgb, Animation, AnimationExt, AnyElement, Context, CursorStyle, Div,
    DragMoveEvent, EmptyView, MouseButton, SharedString, Window,
};
use ld2_core::l10n::{format_duration, t, tf, Key, Lang};
use ld2_core::store::{
    crime_kind_title, filter_active_buffs, filter_active_statuses, local_epoch, Act, Screen,
    UpgradeView,
};
use ld2_protocol::cmd::{
    CrimeLog, GuildBrowse, GuildLeave, GuildRankSet, ItemEquip, ItemUnequip, ItemUse,
    SocketExtract, SocketInsert, UpgradePreview,
};
use ld2_protocol::types::{CharacterState, ItemSnapshot, Severity, SkillSnapshot};

use crate::palette::Palette;
use crate::rune_glyph;
use ely::buttons::IconButton;
use ely::forms::{Choice, Input};
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip, TooltipTrigger};

use super::quests::goals_summary;
use super::{
    act_btn, char_card_origin, clamp_char_size, scrollable_tab, DragSource, DropHit, ItemDrag,
    PopupDrag, PopupKind, PopupResize, Root,
};

/// Вкладки окна персонажа (M15/F25; персистентно строкой в config).
/// Порядок — как в правой колонке окна (спека пользователя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTab {
    /// Производные характеристики — наполняется в F26.
    Params,
    Skills,
    /// Репутация фракций — наполняется в F27.
    Reputation,
    // M18/F36: «Квесты» переехали в отдельное окно (правило дубликатов).
    Guild,
}

impl SheetTab {
    /// M16/F29b: «Инвентарь» убран из окна — он живёт в отдельном
    /// окне рюкзака (правило дубликатов).
    fn all() -> [(SheetTab, Key); 4] {
        [
            (SheetTab::Params, Key::SheetParams),
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Reputation, Key::SheetReputation),
            (SheetTab::Guild, Key::GuildTab),
        ]
    }

    /// M14/F12: иконка вкладки (Lucide-бандл Ely) — метка не меняется.
    pub(crate) fn icon(self) -> IconName {
        match self {
            SheetTab::Params => IconName::Gauge,
            SheetTab::Skills => IconName::Zap,
            SheetTab::Reputation => IconName::Shield,
            SheetTab::Guild => IconName::Flag,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SheetTab::Params => "params",
            SheetTab::Skills => "skills",
            SheetTab::Reputation => "reputation",
            SheetTab::Guild => "guild",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "skills" => SheetTab::Skills,
            "reputation" => SheetTab::Reputation,
            "guild" => SheetTab::Guild,
            // M15/F25: старые значения удалённых вкладок (attrs/statuses)
            // из сохранённого конфига → «Параметры» (ближайшая инфо-вкладка);
            // M16/F29b: «inventory» переехал в окно рюкзака;
            // M18/F36: «quests» переехал в отдельное окно;
            // M21: «professions» — окно профессий (как квесты).
            "attrs" | "statuses" | "inventory" | "quests" | "professions" => SheetTab::Params,
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

/// M18/F32: типы целей инкрустации — руна идёт только в оружие
/// (`weapon`/`staff`), самоцвет (`gem`) — только в броню (`armor`).
/// Общая для окна «Куда инкрустировать…» и окна сокетов; сервер
/// M18/F58f (баг «инкрустация через меню камня»): первый свободный
/// слот, который **принимает** тип вставки по `accepts` (ADR-012:
/// самоцвет — только в gem-слот, руна — в рунный). У старого сервера
/// поля нет — прежнее поведение: первый свободный (§11).
pub(crate) fn first_compatible_socket(
    sockets: &[ld2_protocol::types::Socket],
    insert_type: &str,
) -> Option<usize> {
    sockets.iter().position(|s| {
        s.rune.is_none()
            && match s.accepts.as_deref() {
                Some(accepts) => accepts == insert_type,
                None => true,
            }
    })
}

/// M18/F58f: дубликаты рун не стакаются (указание заказчика) — есть
/// ли в сокетах предмета руна с таким именем (сервер откажет
/// `E_DUPLICATE_RUNE`, §9; фильтр списка — чтобы не предлагать).
pub(crate) fn rune_already_inserted<'a>(
    name: &str,
    sockets: impl IntoIterator<Item = &'a ld2_protocol::types::Socket>,
) -> bool {
    sockets
        .into_iter()
        .filter_map(|s| s.rune.as_ref())
        .any(|r| r.name == name)
}

/// проверяет то же правило (`socket.insert`, истина §7).
pub(crate) fn inscribe_target_allowed(insert_type: &str, target_type: &str) -> bool {
    match insert_type {
        "gem" => target_type == "armor",
        "rune" => matches!(target_type, "weapon" | "staff"),
        _ => false,
    }
}

/// M18/F58 (ADR-012): можно ли вставить `insert_type` в предмет — есть
/// ли **свободный** слот, который его принимает (по `accepts` самого
/// слота). Отсюда самоцветы в оружие и отказ «руна в гемный слот».
///
/// У старого сервера поля `accepts` нет → прежнее правило по типу
/// предмета (`inscribe_target_allowed`), §11 — клиент фолбэчит.
pub(crate) fn inscribe_target_possible(insert_type: &str, item: &ItemSnapshot) -> bool {
    if !matches!(insert_type, "rune" | "gem") {
        return false;
    }
    item.sockets.iter().flatten().any(|socket| {
        if socket.rune.is_some() {
            return false; // слот занят
        }
        match socket.accepts.as_deref() {
            Some(accepts) => accepts == insert_type,
            // Старый сервер: фолбэк — прежнее правило по типу предмета.
            None => inscribe_target_allowed(insert_type, item.item_type.as_str()),
        }
    })
}

impl Root {
    /// M11.2: диалоги листа (ФР-07/09/10): инкрустация/выбор цели.
    /// M18/F53: описание предмета и заточка — попапы (см.
    /// `item_window`/`upgrade_window`), не Ely-модалки.
    pub(crate) fn sheet_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        // M18: окно сокетов — popup (`socket_window`), не модалка.
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

    /// M18/F53: описание предмета — карточка-попап (как окно
    /// персонажа): шапка с названием и крестиком, без затемнения.
    pub(crate) fn item_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let detail = self.store.item_detail.clone()?;
        let title = detail["name"].as_str().unwrap_or("?").to_string();
        // M18/F57: контент прокручивается — описание (статы, бонусы,
        // сокеты, цена) не помещалось в высоту карточки и обрезалось.
        //
        // Обёртка обязана быть **flex-колонкой**: ScrollArea рисует
        // ребёнка с `.size_full()` внутри `overflow_hidden`, поэтому его
        // собственная высота должна быть ограничена снаружи. Без `.flex().flex_col()`
        // его `flex_grow_1().min_h_0()` игнорируется (родитель не
        // flex-контейнер), высота равна контенту — скролла нет.
        // Приём сверен с `character_left_column` (рабочий).
        let body = div().flex().flex_col().flex_grow_1().min_h_0().child(
            ScrollArea::new("ld2-item-detail")
                .vertical()
                .flex_grow_1()
                .min_h_0()
                .child(self.item_view(cx, pal, detail)),
        );
        Some(self.popup_window(window, cx, pal, PopupKind::Item, title, None, body, None))
    }

    /// M18/F53: окно заточки — карточка-попап (как окно персонажа):
    /// шапка с названием предмета и крестиком, без затемняющего
    /// overlay; прогресс-бар и результат — внутри карточки.
    pub(crate) fn upgrade_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        // M18/F55: гейт окна — флаг «список показан», а не наличие
        // камня/предмета: последний камень потрачен → sharpen_stone
        // снят, но окно обязано пережить это, иначе «назад» закрывал
        // его вовсе (жалоба M18/F55).
        if !self.store.sharpen_list {
            return None;
        }
        let stone = self.store.sharpen_stone.clone();
        let view = self.store.upgrade.clone();
        let (title, leading, body) = match view {
            Some(view) => (
                view.item_name.clone(),
                // «Назад» к списку — как в окне диалога NPC (M18/F41).
                Some(self.sharpen_back_btn(cx)),
                self.upgrade_view(cx, pal, &view),
            ),
            None => (
                stone.map(|st| st.name).unwrap_or_default(),
                None,
                self.sharpen_list_view(cx, pal),
            ),
        };
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Upgrade,
            title,
            leading,
            body,
            None,
        ))
    }

    /// M18/F54: кнопка «назад» в шапке окна заточки — возврат от
    /// экрана заточки к списку предметов (само окно не закрывается).
    fn sharpen_back_btn(&mut self, cx: &mut Context<'_, Self>) -> Div {
        div().child(
            IconButton::new("ld2-sharpen-back", IconName::ArrowLeft)
                .tooltip(t(self.lang, Key::BtnBack))
                .on_click(cx.listener(|root, _, _, cx| {
                    root.store.close_upgrade();
                    root.upgrade_progress = None;
                    cx.notify();
                })),
        )
    }

    /// M18/F54: экран 1 — предметы, которые можно заточить (строки как
    /// в магазине торговца): название/уровень слева, «+N» справа;
    /// клик → экран заточки этого предмета.
    fn sharpen_list_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // M18/F55: камень потрачен (последний) — список предметов не
        // показываем: без него сервер не даст превью. Вместо пустоты —
        // заглушка, чтобы было понятно, почему дальше идти некуда.
        if self.store.sharpen_stone.is_none() {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.danger))
                .child(t(lang, Key::SharpenNoStone));
        }
        // M18/F54: качество камня — фиксированная шапка **над**
        // списком (не уезжает при прокрутке), строки — в ScrollArea:
        // предметов больше, чем помещается в карточку.
        let header = self.store.sharpen_stone.clone().map(|stone| {
            let sign = if stone.quality >= 0 { "+" } else { "" };
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!(
                    "{}: {}{}%",
                    t(lang, Key::StatQuality),
                    sign,
                    stone.quality
                ))
        });
        let mut list = div().flex().flex_col().gap_1();
        let mut found = false;
        for item in &snap.char.inventory {
            // Все улучшаемые типы (M18/F54: камни — для всех), до потолка.
            if !matches!(item.item_type.as_str(), "weapon" | "armor" | "staff")
                || item.upgrade >= max_level
            {
                continue;
            }
            found = true;
            let uid = item.uid.clone();
            let name = item.name.clone();
            let upgrade = item.upgrade;
            list = list.child(
                div()
                    .id(format!("ld2-sharpen-{uid}"))
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(pal.selection)))
                    .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Экран 2: превью этого предмета (камень уже активен).
                        this.store.open_upgrade(uid.clone(), name.clone());
                        let cmd = UpgradePreview {
                            item_uid: uid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Preview);
                        cx.notify();
                    }))
                    .child(item_label_row(item, item_color(item, pal)))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(format!("+{upgrade}")),
                    ),
            );
        }
        if !found {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::SharpenNoItems)),
            );
        }
        // Шапка (качество камня) закреплена, список прокручивается —
        // ScrollArea занимает остаток карточки (как окно NPC/персонажа).
        div()
            .flex()
            .flex_col()
            .gap_1()
            .size_full()
            .min_h_0()
            .children(header)
            .child(
                ScrollArea::new("ld2-sharpen-list")
                    .vertical()
                    .flex_grow_1()
                    .min_h_0()
                    .child(list),
            )
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
            // M18/F58: принимает ли слот предмета (accepts) — заменяет
            // прежнее правило по типу предмета + проверку свободного
            // слота; старый сервер — фолбэк (§11).
            if !inscribe_target_possible(&insert_type, item) {
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
                    .child(item_label_row(item, item_color(item, pal))),
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

    /// M16/F29b: оболочка всплывающего окна (механика M15/F25b–g):
    /// слой с карточкой — drag за шапку, resize уголком; `body` —
    /// содержимое под шапкой. Скрим общий для всех окон (могут быть
    /// открыты одновременно) — `Root::popup_scrim`; геометрия и
    /// флаги жестов — `PopupState` вида `kind`.
    // Оболочка окна: контекст (окно/cx/палитра) + вид + заголовок +
    // тело + подвал — набор неизменен, кроме растущих частей.
    /// Оболочка без правой группы в шапке (большинство окон → None).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn popup_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        kind: PopupKind,
        title: impl Into<SharedString>,
        leading: Option<Div>,
        body: Div,
        footer: Option<Div>,
    ) -> AnyElement {
        self.popup_window_full(
            window,
            cx,
            pal,
            kind,
            title,
            leading.map(|d| d.into_any_element()),
            body,
            footer,
            None,
        )
    }

    /// То же + `trailing` — группа справа, слева от крестика закрытия
    /// (M24: тумблер «режим бога» в админ-панели).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn popup_window_full(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        kind: PopupKind,
        title: impl Into<SharedString>,
        // M18/F41: кнопки действий слева в шапке (окно диалога NPC —
        // магазин/«К диалогу»); у остальных окон — None.
        leading: Option<AnyElement>,
        body: Div,
        // M16/F29g: нижняя панель под телом (по аналогии с шапкой).
        footer: Option<Div>,
        // M24: правая группа в шапке (слева от крестика).
        trailing: Option<AnyElement>,
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    // M18/F41: контекстные кнопки диалога — слева от
                    // заголовка (бывший статус-бар переехал сюда).
                    .children(leading)
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(title)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .children(trailing)
                    .child(
                        IconButton::new(format!("{id}-close"), IconName::X)
                            .tooltip(t(lang, Key::BtnClose))
                            .on_click(cx.listener(move |root, _, _, cx| {
                                root.popup_mut(kind).open = false;
                                // M18/F38: окно диалога закрывается вместе со своим
                                // состоянием (гейт рендера — наличие npc_dialog).
                                if kind == PopupKind::Npc {
                                    tracing::debug!("закрытие диалога NPC: крестик");
                                    root.store.npc_dialog = None;
                                }
                                // M18/F53: окно заточки и описание предмета —
                                // гейт рендера — наличие состояния в store.
                                if kind == PopupKind::Upgrade {
                                    tracing::debug!("закрытие окна заточки: крестик");
                                    root.store.close_upgrade();
                                    root.store.sharpen_stone = None;
                                    root.store.sharpen_list = false;
                                    root.upgrade_progress = None;
                                }
                                if kind == PopupKind::Item {
                                    tracing::debug!("закрытие описания предмета: крестик");
                                    root.store.item_detail = None;
                                }
                                if kind == PopupKind::Socketing {
                                    tracing::info!("закрытие окна сокетов: крестик");
                                    root.store.close_socketing();
                                    root.socket_pick = None;
                                }
                                if kind == PopupKind::Quantity {
                                    tracing::debug!("закрытие окна количества: крестик");
                                    root.quantity_pick = None;
                                }
                                // M23: данные окна подятия не переживают закрытие.
                                if kind == PopupKind::LootPickup {
                                    tracing::debug!("закрытие окна подятия: крестик");
                                    root.close_pickup();
                                }
                                if kind == PopupKind::Dialog {
                                    tracing::debug!("закрытие игрового диалога: крестик");
                                    root.close_game_dialog();
                                }
                                cx.notify();
                            })),
                    ),
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

        // M15/F25c: уголок изменения размера (правый нижний) — M24:
        // видимого квадратика больше нет (указание заказчика),
        // только курсор диагонального ресайза при наведении.
        let grip_this = cx.entity();
        let resize_grip = div()
            .id(format!("{id}-resize"))
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(18.0))
            .cursor(CursorStyle::ResizeUpLeftDownRight)
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
    fn skill_row(
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
        // M18 (фича): строки собираем — ниже раскладываем в две
        // колонки одинаковой высоты; слоты рук — отдельным блоком
        // внизу (указание заказчика).
        // R-dnd: тянется ли предмет (для подсветки зон).
        let drag_active = self.drag_item.is_some();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut hand_rows: Vec<AnyElement> = Vec::new();
        for slot in &equip_slots {
            let uid = snap.char.equip.get(slot).cloned().flatten();
            let item = uid
                .as_ref()
                .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
            // R-dnd: подсветка зоны при перетаскивании из рюкзака —
            // подходящий слот зелёным, неподходящий красным.
            let drop_here =
                drag_active && matches!(&self.drop_hit, Some(DropHit::EqSlot(s)) if s == slot);
            let drop_fits = drop_here
                && self
                    .drag_slot_of()
                    .as_deref()
                    .is_some_and(|ds| ds == slot.as_str());
            // M18 (указание заказчика): карточка слота — вертикально:
            // **название слота в верхней строке**, предмет ниже (правым
            // краем); кнопки «Снять»/«Описание» — в контекстном меню.
            // M18: id обязателен для hover-состояния (gpui отслеживает
            // наведение у идентифицированных элементов — подсветка
            // карточки; без id работал только mouse_down).
            // min_h + stretch — одинаковые высота/ширина карточек;
            // выделение цветом предмета раскидывается на детей через
            // наследование text_color (label/название/столбцы —
            // контрастные при hover карточки).
            let mut row = div()
                .id(format!("ld2-equip-card-{slot}"))
                // Одинаковые высота и ширина карточек (указание
                // заказчика): min_h + stretch поперёк колонки.
                .min_h(px(76.0))
                .self_stretch()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                // Каждый слот визуально заметен — рамка в цвете
                // границ панели.
                .border_1()
                .border_color(rgb(pal.border))
                // Эпик/легендарка: статичное внутреннее свечение в
                // покое (анимация — только по наведению, ниже).
                .when_some(item, |row, item| {
                    row.shadow(rarity_glow_inset(&item.rarity, pal))
                })
                .cursor_pointer()
                // R-dnd: подсветка зоны при перетаскивании из рюкзака —
                // подходящий слот зелёным, неподходящий красным.
                .when(drop_here, |row| {
                    row.border_color(rgb(if drop_fits { pal.success } else { pal.danger }))
                })
                // Подсветка: пустой слот — общий selection, занятый —
                // цветом предмета (ветки ниже); клик — контекстное меню.
                ;
            // Label слота: muted (контрастный текст при выделении
            // отменён заказчиком — выделяется только рамка).
            row = row.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(slot_label(lang, slot)),
            );
            // R-dnd: зона приёма для предметов из рюкзака.
            let zone_slot = slot.clone();
            let zone_this = cx.entity();
            let zone_move = zone_this.clone();
            let zone_drop = zone_this.clone();
            let zone_slot_move = zone_slot.clone();
            let zone_slot_drop = zone_slot.clone();
            row = row
                .on_drag_move::<ItemDrag>(move |_, _, cx| {
                    zone_move.update(cx, |root, cx| {
                        if matches!(
                            root.drag_item.as_ref().map(|d| &d.from),
                            Some(DragSource::Bp)
                        ) {
                            root.drop_hit = Some(DropHit::EqSlot(zone_slot_move.clone()));
                            cx.notify();
                        }
                    });
                })
                .on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
                    zone_drop.update(cx, |root, cx| {
                        if matches!(&drag.from, DragSource::Bp) {
                            let fits = drag.slot_req.as_deref() == Some(zone_slot_drop.as_str());
                            if fits {
                                let cmd = ItemEquip {
                                    item_uid: drag.uid.clone(),
                                    slot: zone_slot_drop.clone(),
                                };
                                root.send_cmd(&cmd, Act::Equip);
                            } else {
                                root.store.notice =
                                    Some(t(root.lang, Key::SlotMismatch).to_string());
                            }
                        }
                        root.drop_hit = None;
                        cx.notify();
                    });
                });
            let Some(item) = item else {
                // M18 (указание заказчика): пустые слоты — **не
                // подсвечиваются** и выглядят disabled (приглушены);
                // «Заблокировано двуручным оружием» больше нет — слот
                // показывается как обычный пустой (метка слота сверху
                // повторяет слот оружия).
                row = row.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::EmptySlot)),
                );
                row = row.opacity(0.55);
                if matches!(slot.as_str(), "main_hand" | "off_hand") {
                    hand_rows.push(row.into_any_element());
                } else {
                    rows.push(row.into_any_element());
                }
                continue;
            };
            // R-dnd: тянем предмет ИЗ слота (выброс/снятие).
            let drag_payload = ItemDrag {
                uid: item.uid.clone(),
                from: DragSource::Equip(slot.clone()),
                slot_req: item.slot.clone(),
            };
            let drag_this = cx.entity();
            let ghost_name = item.name.clone();
            let ghost_color = rarity_color(&item.rarity, pal);
            let ghost_theme = self.config.theme.clone();
            row = row.on_drag(drag_payload, move |drag, _, _, cx| {
                drag_this.update(cx, |root, cx| {
                    root.drag_item = Some(drag.clone());
                    root.drop_hit = None;
                    cx.notify();
                });
                // Карточка предмета под курсором (заказчик).
                let name = ghost_name.clone();
                let color = ghost_color;
                let theme = ghost_theme.clone();
                cx.new(move |_| crate::root::ItemDragGhost { name, color, theme })
            });
            let color = item_color(item, pal);
            // Отступ-заполнитель прижимает предмет к правому краю.
            // M18/F58f: подпись как в рюкзаке (указание заказчика) —
            // слоты второй строкой под названием (`item_label_stack`).
            // M18 (указание заказчика): карточка одетого предмета —
            // ниже названия слота идёт **название предмета**, ниже —
            // **столбцы самоцветов/рун** (иконки слотов отдельной
            // строкой, левым краем).
            let mut name_line = div()
                .flex()
                .flex_row()
                .gap_1()
                .items_baseline()
                .text_color(rgb(color))
                .child(div().child(item.name.clone()));
            if item.upgrade > 0 {
                name_line = name_line.child(div().child(format!("+{}", item.upgrade)));
            }
            if let Some(qty) = item.qty.filter(|q| *q > 1) {
                name_line = name_line.child(div().child(format!("×{qty}")));
            }
            row = row.child(name_line);
            if let Some(sockets) = item.sockets.as_ref().filter(|s| !s.is_empty()) {
                row = row.child(
                    div()
                        .text_color(rgb(color))
                        .child(socket_icons_row(item.item_type.as_str(), sockets)),
                );
            }
            // M18 (указание заказчика): выделение — **только рамка**
            // цветом предмета при наведении; фон и тексты не меняются
            // (контрастный текст отменён — баг при скролле).
            row = row.hover(move |st| st.border_color(rgb(color)));
            // R-dnd: зона приёма для предметов из рюкзака.
            let zone_slot = slot.clone();
            let zone_this = cx.entity();
            let zone_move = zone_this.clone();
            let zone_drop = zone_this.clone();
            let zone_slot_move = zone_slot.clone();
            let zone_slot_drop = zone_slot.clone();
            row = row
                .on_drag_move::<ItemDrag>(move |_, _, cx| {
                    zone_move.update(cx, |root, cx| {
                        if matches!(
                            root.drag_item.as_ref().map(|d| &d.from),
                            Some(DragSource::Bp)
                        ) {
                            root.drop_hit = Some(DropHit::EqSlot(zone_slot_move.clone()));
                            cx.notify();
                        }
                    });
                })
                .on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
                    zone_drop.update(cx, |root, cx| {
                        if matches!(&drag.from, DragSource::Bp) {
                            let fits = drag.slot_req.as_deref() == Some(zone_slot_drop.as_str());
                            if fits {
                                let cmd = ItemEquip {
                                    item_uid: drag.uid.clone(),
                                    slot: zone_slot_drop.clone(),
                                };
                                root.send_cmd(&cmd, Act::Equip);
                            } else {
                                root.store.notice =
                                    Some(t(root.lang, Key::SlotMismatch).to_string());
                            }
                        }
                        root.drop_hit = None;
                        cx.notify();
                    });
                });
            // Заказчик 2026-11-20: описание предмета — в тултипе (тот же
            // виджет, что и окно описания; появляется плавно).
            let tip_detail = serde_json::to_value(item).unwrap_or_default();
            let tip_skills = self.skill_names_for(&tip_detail);
            let tip_theme = self.config.theme.clone();
            let tip_name = item.name.clone();
            let tip_lang = lang;
            let tip_rarity = item.rarity.clone();
            // Меню предмета (F29d): Описание и Снять — вместо кнопок.
            let mut menu = Menu::new();
            let detail = serde_json::to_value(item).unwrap_or_default();
            menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
                this.store.close_upgrade();
                this.upgrade_progress = None;
                this.store.item_detail = Some(detail.clone());
                this.open_popup(PopupKind::Item);
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
            // M18 (указание заказчика): пункт «Инкрустация» — и в
            // разделе экипировки окна персонажа, если у вещи есть
            // слоты (пустые или занятые); тот же popup, что рюкзак.
            if item
                .sockets
                .as_ref()
                .is_some_and(|sockets| !sockets.is_empty())
            {
                let uid = item.uid.clone();
                menu = menu.item(menu_item(
                    cx,
                    t(lang, Key::BtnSocketing),
                    move |this, _cx| {
                        this.store.item_detail = None;
                        this.store.close_upgrade();
                        this.upgrade_progress = None;
                        this.store.open_socketing(uid.clone());
                        this.open_popup(PopupKind::Socketing);
                    },
                ));
            }
            // Запрос меню: левый и правый клик по строке (номер растёт —
            // Ely переоткрывает; в manual-режиме host-правый клик выключен).
            let request = match &self.equip_menu {
                Some((s, n, at)) if s == slot => Some((*n, *at)),
                _ => None,
            };
            let slot_right = slot.clone();
            let slot_left = slot.clone();
            let row = row
                .on_mouse_down(
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
                );
            // Переливание (заказчик 2026-11-20): эпик/легендарка — на
            // наведении играет ЯРКОСТЬ цвета редкости (бриллиант);
            // без наведения — статичное внутреннее свечение.
            let hover_key = format!("eq:{slot}");
            let shimmering = matches!(item.rarity.as_str(), "epic" | "legendary")
                && self.hovered_item.as_deref() == Some(hover_key.as_str());
            let hover_this = cx.entity();
            let hover_key_hover = hover_key.clone();
            let row = row.on_hover(move |hovered, _, cx| {
                let key = hover_key_hover.clone();
                hover_this.update(cx, |root, cx| {
                    root.hovered_item = hovered.then_some(key);
                    cx.notify();
                });
            });
            let row = if shimmering {
                let base = rarity_color(&item.rarity, pal) & 0xff_ffff;
                row.with_animation(
                    SharedString::from(format!("shimmer-{hover_key}")),
                    Animation::new(std::time::Duration::from_secs(4)).repeat(),
                    move |el, t| {
                        let hsla: gpui::Hsla = gpui::rgba((base << 8) | 0xff).into();
                        // Бриллиантовое переливание: играет ЯРКОСТЬ
                        // цвета редкости — оттенок не меняется, других
                        // цветов нет (заказчик).
                        let l = (hsla.l + 0.18 * (2.0 * std::f32::consts::PI * t).sin())
                            .clamp(0.0, 1.0);
                        el.border_color(gpui::hsla(hsla.h, hsla.s, l, 1.0))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(hsla.h, hsla.s, l, 0.35),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(8.0),
                                spread_radius: px(0.0),
                                inset: true,
                            }])
                    },
                )
                .into_any_element()
            } else {
                row.into_any_element()
            };
            let row = ContextMenu::new(format!("ld2-equip-ctx-{slot}"), menu)
                .manual(request)
                .child(row);
            // Тултип описания (заказчик 2026-11-20): через 1с наведения;
            // свечение медленно пульсирует наружу (~2.5 с) — пульс на
            // карточке внутри тултипа.
            let pulse_rarity = tip_rarity.clone();
            let row = TooltipTrigger::new(format!("ld2-equip-tip-{slot}"), row, move |_, _| {
                let pulse_rarity = pulse_rarity.clone();
                let hover_key = hover_key.clone();
                let pal = crate::palette::palette(&tip_theme);
                // Фон и вид — как у окна описания; отрицательные
                // поля гасят паддинг сурфейса.
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mx(px(-8.0))
                    .my(px(-4.0))
                    .p(px(10.0))
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .border_1()
                    .border_color(rgb(rarity_color(&tip_rarity, &pal)))
                    .text_color(rgb(pal.text))
                    .text_size(px(13.0))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(tip_name.clone()),
                    )
                    .child(item_description_body(
                        tip_lang,
                        &pal,
                        &tip_detail,
                        &tip_skills,
                    ))
                    .with_animation(
                        SharedString::from(format!("tip-pulse-{hover_key}")),
                        Animation::new(std::time::Duration::from_millis(2500)).repeat(),
                        move |card, t| {
                            let alpha: f32 = (0.30 + 0.20 * (2.0 * std::f32::consts::PI * t).sin())
                                .clamp(0.10, 0.50);
                            let base = rarity_color(&pulse_rarity, &pal) & 0xff_ffff;
                            card.shadow(vec![gpui::BoxShadow {
                                color: gpui::rgba((base << 8) | (alpha * 255.0) as u32).into(),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(14.0),
                                spread_radius: px(2.0),
                                inset: false,
                            }])
                        },
                    )
                    .into_any_element()
            })
            .delay(std::time::Duration::from_secs(1));
            if matches!(slot.as_str(), "main_hand" | "off_hand") {
                hand_rows.push(row.into_any_element());
            } else {
                rows.push(row.into_any_element());
            }
        }

        // Две колонки одинаковой высоты: ряд растягивает обе до
        // самой длинной (flex stretch), ширины равны (flex_grow).
        let two_cols = |items: Vec<AnyElement>| -> Div {
            let half = items.len().div_ceil(2);
            let mut left = div().flex().flex_col().gap_1().min_w_0().flex_grow_1();
            let mut right = div().flex().flex_col().gap_1().min_w_0().flex_grow_1();
            for (i, row) in items.into_iter().enumerate() {
                if i < half {
                    left = left.child(row);
                } else {
                    right = right.child(row);
                }
            }
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_stretch()
                .child(left)
                .child(right)
        };
        col = col.child(two_cols(rows));

        // M18 (указание заказчика): слоты рук — в самый низ, ниже всей
        // экипировки, отделены чертой.
        if !hand_rows.is_empty() {
            col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
            col = col.child(two_cols(hand_rows));
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

        // Использовать (свиток/зелье/…); M18/F54: камень усиления —
        // отдельный ack (Act::Sharpen) + открытие окна заточки.
        let uid = item.uid.clone();
        let is_stone = item.item_type == "upgrade_stone";
        menu = menu.item(menu_item(cx, t(lang, Key::MenuUse), move |this, _cx| {
            let cmd = ItemUse {
                item_uid: uid.clone(),
                target_id: None,
            };
            this.send_cmd(&cmd, if is_stone { Act::Sharpen } else { Act::Drop });
            if is_stone {
                // Геометрия карточки (гейт рендера — sharpen_stone из ack).
                this.open_popup(PopupKind::Upgrade);
            }
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

        // M18 (указание заказчика): пункт «Инкрустация» — в разделе
        // экипировки (после «Надеть»), если есть слоты рун или
        // самоцветов — **свободные или занятые** (любой слот слотом;
        // пустой список — не показываем).
        let has_sockets = item
            .sockets
            .as_ref()
            .is_some_and(|sockets| !sockets.is_empty());
        if has_sockets {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::BtnSocketing),
                move |this, _cx| {
                    // Окно сокетов — popup-диалог (аналогично
                    // описанию/заточке); соседние попапы закрываем.
                    this.store.item_detail = None;
                    this.store.close_upgrade();
                    this.upgrade_progress = None;
                    this.store.open_socketing(uid.clone());
                    this.open_popup(PopupKind::Socketing);
                },
            ));
        }

        // Улучшение (ФР-10): до максимума из конфига сервера.
        // M18/F54: сервер требует активный камень — пункт показываем
        // только когда камень уже выбран (иначе `E_INGREDIENTS`).
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(item.item_type.as_str(), "weapon" | "armor" | "staff")
            && self.store.sharpen_stone.is_some();
        if upgradable && item.upgrade < max_level {
            let uid = item.uid.clone();
            let name = item.name.clone();
            menu = menu.item(menu_item(cx, t(lang, Key::UpgradeBtn), move |this, _cx| {
                // M18/F53: раньше модал-диспетчер показывал одну модалку
                // за раз — теперь попапы, закрываем соседнее вручную.
                this.store.item_detail = None;
                this.store.open_upgrade(uid.clone(), name.clone());
                // M18/F53: карточка заточки — как окно персонажа
                // (базовый размер/позиция, без затемнения).
                this.open_popup(PopupKind::Upgrade);
                let cmd = UpgradePreview {
                    item_uid: uid.clone(),
                };
                this.send_cmd(&cmd, Act::Preview);
            }));
        }

        // Описание / Выбросить.
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
            this.store.close_upgrade();
            this.upgrade_progress = None;
            this.store.item_detail = Some(detail.clone());
            this.open_popup(PopupKind::Item);
        }));
        let uid = item.uid.clone();
        let max_qty = item.qty.unwrap_or(1);
        let item_name = item.name.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDrop), move |this, _cx| {
            // R-dnd (заказчик 2026-11-21): подтверждение выброса — окно
            // «Выбросить предмет?» (для стаков >1: инпут + ползунок +
            // «Все»; одна штука — сразу кнопка выброса).
            this.drop_confirm = Some(super::DropPending {
                uid: uid.clone(),
                name: item_name.clone(),
                from: super::DragSource::Bp,
                max: max_qty,
                qty: max_qty,
            });
        }));
        menu
    }

    // ── вкладка «Статусы» (ФР-11, M6) ────────────────────────────────────

    /// M15/F25: статусы со временем + журнал нарушений — низ левой
    /// колонки (перенос из вкладки «Статусы» по правилу дубликатов).
    /// M18/F58f (ADR-012 §6): активные баффы рун — название (серверная
    /// строка §8), стак и обратный отсчёт до снятия (по серверным срокам
    /// как у статусов: now = локальное + offset, §1.1).
    fn buffs_block(&self, pal: &Palette) -> Div {
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
        // Тот же виджет, что и тултип описания (заказчик 2026-11-20).
        let skills = self.skill_names_for(&detail);
        item_description_body(self.lang, pal, &detail, &skills).child(act_btn(
            cx,
            t(self.lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.item_detail = None;
                let _ = cx;
            },
        ))
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

/// Тело описания предмета — самостоятельный виджет: и окно описания,
/// и тултипы рюкзака/экипировки (заказчик 2026-11-20). Без `self`:
/// имена навыков-бонусов передаются готовыми (`skill_names`).
pub(crate) fn item_description_body(
    lang: Lang,
    pal: &Palette,
    detail: &serde_json::Value,
    skill_names: &std::collections::HashMap<String, String>,
) -> Div {
    let rarity = detail["rarity"].as_str().unwrap_or("common");
    let item_type = detail["type"].as_str().unwrap_or("?");
    // M18/F57: справа запас под скроллбар (приём character_left_column)
    // — иначе индикатор прокрутки наезжал на жирные значения статов.
    let mut col = div().flex().flex_col().gap_1().pr(px(16.0));
    // M18/F58f: у рун — глиф предмета сразу под названием (шапка
    // окна), вверху тела (указание заказчика).
    if item_type == "rune" {
        let name = detail["name"].as_str().unwrap_or("rune");
        let glyph = detail["stats"].get("glyph").and_then(|v| v.as_str());
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .child(crate::rune_glyph::glyph_div_for(glyph, name)),
        );
    } else if item_type == "gem" {
        // M18/F58f: у самоцвета — знак раритета под названием
        // (указание заказчика: рюкзак и описание).
        col = col.child(div().flex().flex_row().items_baseline().child(
            crate::gem_glyph::gem_symbol_div(detail["rarity"].as_str().unwrap_or("")),
        ));
    }
    // M18/F58f: у рун уровень заточки в описании не нужен
    // (указание заказчика) — только «редкость · тип».
    let header = if item_type == "rune" {
        format!(
            "{} · {}",
            rarity_label(lang, rarity),
            type_label(lang, item_type)
        )
    } else {
        format!(
            "{} · {} · +{}",
            rarity_label(lang, rarity),
            type_label(lang, item_type),
            detail["upgrade"].as_i64().unwrap_or(0)
        )
    };
    col = col.child(
        div()
            .text_color(rgb(quality_color(detail["def_id"].as_str())
                .unwrap_or_else(|| rarity_color(rarity, pal))))
            .child(header),
    );
    // M18/F58f: слоты — вторая строка под «редкость · тип · +N»,
    // руна-слот первым; иконки различают руны и самоцветы.
    if let Ok(sockets) =
        serde_json::from_value::<Vec<ld2_protocol::types::Socket>>(detail["sockets"].clone())
    {
        if !sockets.is_empty() {
            // Цвет слотов — как у предмета (rarity-цвет строки).
            col = col.child(
                div()
                    .text_color(rgb(rarity_color(rarity, pal)))
                    .child(socket_icons_row(item_type, &sockets)),
            );
        }
    }
    if let Some(stats) = detail["stats"].as_object() {
        // M18/F58f: строка-пара «label: value» в стиле статов окна.
        let kv = |label: String, value: String| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(div().text_color(rgb(pal.muted)).child(label))
                .child(div().font_weight(gpui::FontWeight::BOLD).child(value))
        };
        // Заказчик 2026-11-20: у воина/лучника заклинание — «приём».
        let rune_class = stats
            .get("class")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        for (key, value) in stats {
            // M16/F29e: ключи и значения — на языке игрока
            // (школы — свои названия; прочие значения — как есть).
            // M18/F53: `param` — вложенный объект {id: значение};
            // показываем каждое производное как «Критический удар: +3%».
            if key == "param" {
                if let Some(params) = value.as_object() {
                    for (pid, pval) in params {
                        let raw = pval
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| pval.to_string());
                        let suffix = if param_is_percent(pid) { "%" } else { "" };
                        col = col.child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_between()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(pal.panel))
                                .child(
                                    div()
                                        .text_color(rgb(pal.muted))
                                        .child(param_label(lang, pid)),
                                )
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(format!("{}{}", raw, suffix)),
                                ),
                        );
                    }
                    continue;
                }
            }
            // M18/F58f: описание рун (указание заказчика) — без
            // технического glyph, шанс — в процентах, бафф/эффект —
            // читаемые строки вместо JSON.
            if key == "glyph" {
                continue;
            }
            if key == "chance" {
                let pct = (value.as_f64().unwrap_or(0.0) * 100.0).round() as i64;
                col = col.child(kv(
                    t(lang, Key::RuneProcChance).to_string(),
                    format!("{pct}%"),
                ));
                continue;
            }
            if key.starts_with("buff") && value.is_object() {
                // Баффы рун (buff/buff2/…) — читаемые строки, не JSON:
                // «Урон: +10% · 12с · ×2» (pct — знак %).
                if let Some(b) = value.as_object() {
                    let param = b.get("param").and_then(|v| v.as_str()).unwrap_or("");
                    let val = b.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                    let pct = b.get("pct").and_then(|v| v.as_bool()).unwrap_or(false);
                    let dur = b.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(0);
                    let stacks = b.get("max_stacks").and_then(|v| v.as_i64()).unwrap_or(1);
                    col = col.child(kv(
                        param_label(lang, param),
                        format!(
                            "+{val}{} · {} · ×{stacks}",
                            if pct { "%" } else { "" },
                            format_duration(lang, dur)
                        ),
                    ));
                }
                continue;
            }
            if key == "effect" {
                if let Some(e) = value.as_object() {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let val = e.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                    let dur = format_duration(
                        lang,
                        e.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(0),
                    );
                    let (label, val_s) = match kind {
                        "bleed" => (
                            t(lang, Key::EffectBleed).to_string(),
                            format!("{val}{} · {dur}", t(lang, Key::PerSecond)),
                        ),
                        // R-контент: «горение» — урон за тик с периодом.
                        "burn" => (
                            t(lang, Key::EffectBurn).to_string(),
                            format!("{val}{} · {dur}", t(lang, Key::PerTick),),
                        ),
                        "stun" => (t(lang, Key::EffectStun).to_string(), dur.clone()),
                        "root" => (t(lang, Key::EffectRoot).to_string(), dur.clone()),
                        other => (other.to_string(), dur),
                    };
                    col = col.child(kv(label, val_s));
                }
                continue;
            }
            // Заказчик 2026-11-20: неразобранный JSON в описании
            // не показываем — незнакомые структурные поля контента
            // (объекты/массивы) пропускаем, а не сваливаем строкой.
            if value.is_object() || value.is_array() {
                continue;
            }
            let raw = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            let value = if key == "school" {
                school_label(lang, &raw)
            } else if key == "class" {
                // Класс руны: warrior/mage/archer → метка языка.
                match raw.as_str() {
                    "warrior" => t(lang, Key::ProfileWarrior).to_string(),
                    "mage" => t(lang, Key::ProfileMage).to_string(),
                    "archer" => t(lang, Key::ProfileArcher).to_string(),
                    _ => raw.clone(),
                }
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
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().text_color(rgb(pal.muted)).child({
                        if key == "spell" && matches!(rune_class.as_str(), "warrior" | "archer") {
                            // У воина/лучника это приём, не заклинание.
                            t(lang, Key::SpellTechnique).to_string()
                        } else {
                            stat_label(lang, key)
                        }
                    }))
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(value)),
            );
        }
    }
    // M18/F47/ADR-010: бонусы предмета — «Сила +3», «Точность +4%»,
    // «Владение мечом +2» (контент + инстанс, слитые сервером).
    if let Some(bonuses) = detail["bonuses"].as_array() {
        if !bonuses.is_empty() {
            col = col.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ItemBonuses)),
            );
            for bonus in bonuses {
                let kind = bonus["kind"].as_str().unwrap_or_default();
                let id = bonus["id"].as_str().unwrap_or_default();
                let value = bonus["value"].as_i64().unwrap_or(0);
                let pct = bonus["pct"].as_bool().unwrap_or(false);
                let name = match kind {
                    "attr" => attr_label(lang, id),
                    "param" => param_label(lang, id),
                    "skill" => skill_names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| id.to_string()),
                    _ => id.to_string(),
                };
                let sign = if value >= 0 { "+" } else { "" };
                // M18/F56: та же плитка, что у статов выше —
                // label приглушённым слева, значение жирным вправо
                // (иначе бонусы «плыли» относительно статов).
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().text_color(rgb(pal.muted)).child(name))
                        .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                            "{}{}{}",
                            sign,
                            value,
                            if pct { "%" } else { "" }
                        ))),
                );
            }
        }
    }
    if let Some(sockets) = detail["sockets"].as_array() {
        for socket in sockets {
            // M18/F58f: знаки рунных слотов — глифы шрифта Dwarven
            // Runes (rune_glyph); слоты самоцветов — прежние ◈/○.
            let is_rune_slot = socket["accepts"].as_str() != Some("gem");
            if let Some(rune) = socket["rune"].as_object() {
                // M17/F30c: самоцвет — без зарядов (заряды у рун).
                // M18/F58f: знак — контент-поле `glyph`, фолбэк — хеш.
                // Доступ к Map — только через `.get()`: индексация
                // отсутствующего ключа у serde_json::Map паникует
                // («no entry found for key» — вылет при открытии
                // описания).
                let name = rune.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let glyph =
                    rune_glyph::glyph_div_for(rune.get("glyph").and_then(|v| v.as_str()), name);
                let label = if is_rune_slot {
                    if rune
                        .get("spell")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.is_empty())
                    {
                        name.to_string()
                    } else {
                        format!(
                            "{}: {}/{}",
                            name,
                            rune.get("charges").and_then(|v| v.as_i64()).unwrap_or(0),
                            rune.get("max").and_then(|v| v.as_i64()).unwrap_or(0)
                        )
                    }
                } else {
                    name.to_string()
                };
                // M18/F58f: под занятым слотом — подробности, что
                // даёт вставка (урон/шанс/бафф/эффект/трата; сервер
                // формирует строки, §8).
                let mut cell = div().flex().flex_col().child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .items_baseline()
                        .child(if is_rune_slot {
                            glyph
                        } else {
                            // M18/F58f (правка заказчика): занятый
                            // самоцвет — знак его раритета (Symbola);
                            // старый сервер без rarity — прежний ◈
                            // (§11).
                            match rune.get("rarity").and_then(|v| v.as_str()) {
                                Some(rarity) => crate::gem_glyph::gem_symbol_div(rarity),
                                None => div().child("◈"),
                            }
                        })
                        .child(div().child(label)),
                );
                // `.get`, а не `rune["detail"]`: у serde_json::Map
                // индексация отсутствующего ключа паникует (в
                // отличие от Value → Null) — вылет при открытии
                // описания рун без подписи (M18/F58f).
                if let Some(details) = rune.get("detail").and_then(|d| d.as_array()) {
                    for line in details.iter().filter_map(|d| d.as_str()) {
                        cell = cell.child(
                            div()
                                .pl(px(20.0))
                                .text_color(rgb(pal.muted))
                                .child(line.to_string()),
                        );
                    }
                }
                col = col.child(cell);
            } else if is_rune_slot {
                let glyph = rune_glyph::empty_slot_glyph_div();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .items_baseline()
                        .child(glyph)
                        .child(div().text_color(rgb(pal.muted)).child("—")),
                );
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
    col
}

impl Root {
    /// M18/F55: итог заточки для показа прямо на экране заточки —
    /// карточка зелёным при успехе и красным при провале (разрушение /
    /// без изменений). Вызывается и в ветке «превью ещё нет», и в
    /// основной — итог переживает автопревью.
    fn sharpen_result_line(&self, lang: Lang, pal: &Palette, res: &serde_json::Value) -> Div {
        let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let (text, color) = if ok {
            let n = res.get("new_upgrade").and_then(|v| v.as_u64()).unwrap_or(0);
            (
                tf(lang, Key::SharpenResultOk, &[("s", &n.to_string())]),
                pal.success,
            )
        } else if res
            .get("item_destroyed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            (t(lang, Key::SharpenResultBroken).to_string(), pal.danger)
        } else {
            (t(lang, Key::SharpenResultFail).to_string(), pal.danger)
        };
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(color))
            .child(text)
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
        let progress = self
            .upgrade_progress
            .as_ref()
            .filter(|p| p.0 == view.item_uid)
            .cloned();
        let mut col = div().flex().flex_col().gap_1();
        let Some(preview) = view.preview.clone() else {
            if let Some(res) = view.result.as_ref() {
                // M18/F55: итог броска — прямо на экране заточки,
                // зелёный успех / красный провал (а не уведомление ОС).
                col = col.child(self.sharpen_result_line(lang, pal, res));
            } else {
                col = col.child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::Loading)),
                );
            }
            // M18/F53: кнопки внизу нет — закрытие крестиком в шапке.
            return col;
        };

        // M18/F55: итог прошлого броска остаётся виден на том же
        // экране (автопревью его не затирает) — гасится только при
        // старте следующего улучшения.
        if let Some(res) = view.result.as_ref() {
            col = col.child(self.sharpen_result_line(lang, pal, res));
        }

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

        // M18/F53: что будет при неудаче — из capability `upgrade.rules`.
        let fail_rule = self
            .store
            .session
            .as_ref()
            .and_then(|s| s.features.get("upgrade.rules"))
            .and_then(|v| v.as_str())
            .unwrap_or("keep");
        let fail_text = match fail_rule {
            "level_down" => "При неудаче: уровень снижается на 1 (не ниже 0).",
            "break_on_fail" => "При неудаче: предмет разрушается.",
            _ => "При неудаче: уровень не меняется, предмет сохраняется.",
        };
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(fail_text),
        );

        let preview_id = preview["preview_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let can_start = !preview_id.is_empty() && !pending && progress.is_none();
        if let Some((_uid, _pid, started, duration)) = progress.clone() {
            let now = self.now_ms();
            let left = (started + duration - now).max(0);
            let frac = ((now - started) as f32 / duration.max(1) as f32).clamp(0.0, 1.0);
            let secs = (left + 999) / 1000;
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt(px(8.0))
                    .child(
                        div().h(px(6.0)).rounded_md().bg(rgb(pal.border)).child(
                            div()
                                .h_full()
                                .w(gpui::relative(frac))
                                .rounded_md()
                                .bg(rgb(pal.warn)),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("{}… {} с", t(lang, Key::Loading), secs)),
                    ),
            );
        }
        // M18/F53: внизу только «Улучшить» — закрытие крестиком в шапке.
        let mut row = div().flex().flex_row().gap_2().mt(px(8.0));
        let uid = view.item_uid.clone();
        if can_start {
            row = row.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.warn,
                0xffffff,
                move |this, cx| {
                    // M18/F53: прогресс 3 с (из настроек сервера), затем
                    // отправка cmd/upgrade.start (root::tick).
                    let duration_ms = this
                        .store
                        .session
                        .as_ref()
                        .map(|s| (s.upgrade.duration_s * 1000.0).round() as i64)
                        .unwrap_or(3_000);
                    // M18/F55: прошлый итог не должен висеть во время
                    // прогресса нового броска.
                    if let Some(v) = this.store.upgrade.as_mut() {
                        v.result = None;
                    }
                    this.upgrade_progress =
                        Some((uid.clone(), preview_id.clone(), this.now_ms(), duration_ms));
                    // M24: единый прогресс-бар левой колонки —
                    // заточка, подпись «Действие · имя».
                    let item_name = this
                        .store
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.char.inventory.iter().find(|i| i.uid == uid))
                        .map(|i| i.name.clone())
                        .unwrap_or_default();
                    this.start_progress(
                        super::ProgressSource::Sharpen,
                        format!("{} · {}", t(this.lang, Key::UpgradeBtn), item_name),
                        duration_ms,
                    );
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
    /// M18: окно сокетов (инкрустация через меню оружия) — popup-диалог
    /// с шапкой и крестиком (раньше — модалка модал-диспетчера).
    pub(crate) fn socket_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let staff_uid = self.store.socketing.clone()?;
        // M18 (указание заказчика): окно «Инкрустация» — две страницы
        // (слоты → выбор вставки), на второй — кнопка «Назад» в шапке
        // (паттерн окна заточки).
        let title = t(self.lang, Key::SocketingTitle).to_string();
        let lang = self.lang;
        let leading = self.socket_pick.is_some().then(|| {
            div().child(
                IconButton::new("ld2-socket-back", IconName::ArrowLeft)
                    .tooltip(t(lang, Key::BtnBack))
                    .on_click(cx.listener(|root, _, _, cx| {
                        root.socket_pick = None;
                        cx.notify();
                    })),
            )
        });
        let inner = match self.socket_pick {
            Some(index) => self.pick_list_view(cx, pal, staff_uid, index),
            None => self.socket_view(cx, pal, staff_uid),
        };
        // Обёртка-флекс-колонка под ScrollArea — приём item_window (M18/F57).
        let body = div().flex().flex_col().flex_grow_1().min_h_0().child(
            ScrollArea::new("ld2-socketing-scroll")
                .vertical()
                .flex_grow_1()
                .min_h_0()
                .child(inner),
        );
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Socketing,
            title,
            leading,
            body,
            None,
        ))
    }

    /// M18 (указание заказчика): окно выбора вставки — список рун или
    /// самоцветов для выбранного слота, **по типу магазина в диалогах**
    /// (строки: слева предмет — справа действие); клик по строке —
    /// процесс инкрустации в этот слот, окно закрывается.
    pub(crate) fn pick_list_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
        index: u32,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(socket) = staff.sockets.iter().flatten().find(|s| s.index == index) else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::NoRunes));
        };
        // Список: принимает ли слот вставку (accepts; §11 — фолбэк по
        // типу цели), для рун — без уже вставленных (дубликаты).
        let inserts: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| match socket.accepts.as_deref() {
                Some(accepts) => i.item_type == accepts,
                None => inscribe_target_allowed(&i.item_type, staff.item_type.as_str()),
            })
            .filter(|i| {
                i.item_type != "rune"
                    || !rune_already_inserted(&i.name, staff.sockets.iter().flatten())
            })
            .cloned()
            .collect();

        // Строки в стиле магазина (game::shop_view): панель, подсветка
        // наведения, клик по строке → действие.
        let mut col = div().flex().flex_col().gap_1().flex_grow_1().min_h_0();
        if inserts.is_empty() {
            col = col.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoRunes)),
            );
        }
        for item in &inserts {
            let pick = item.uid.clone();
            let pick_staff = staff_uid.clone();
            let row_id = format!("ld2-pick-{}", item.uid);
            col = col.child(
                div()
                    .id(row_id)
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .cursor_pointer()
                    .hover(move |st| st.bg(rgb(pal.selection)))
                    .child(item_label_row(item, rarity_color(&item.rarity, pal)))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::InsertRune)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Процесс инкрустации: вставка в выбранный слот.
                        let cmd = SocketInsert {
                            staff_uid: pick_staff.clone(),
                            socket_index: index,
                            rune_uid: pick.clone(),
                        };
                        this.send_cmd(&cmd, Act::Socket);
                        this.socket_pick = None;
                        cx.notify();
                    })),
            );
        }
        col
    }

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
            tracing::info!("закрытие окна сокетов: staff не найден в инвентаре ({staff_uid})");
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
            // M18/F58f (баг): принимает слот — его `accepts`, а не тип
            // предмета (ADR-012): для цели (staff) с gem-слотами в списке
            // должны быть и самоцветы; заодно — только вставки, которым
            // есть свободный подходящий слот.
            .filter(|i| inscribe_target_possible(&i.item_type, &staff))
            // M18/F58f: уже вставленные руны не предлагаем (дубликаты
            // не стакаются — сервер и так откажет, §9).
            .filter(|i| {
                insert_type != "rune"
                    || !rune_already_inserted(&i.name, staff.sockets.iter().flatten())
            })
            .cloned()
            .collect();

        let mut col = div().flex().flex_col().gap_1();
        for socket in staff.sockets.iter().flatten() {
            let mut row = div().flex().flex_row().gap_2().items_center();
            // M18/F58f: маркер руна-слота — глиф Dwarven Runes
            // (пустой слот — свой глиф); слот самоцвета — ◈, а у
            // **инкрустированного** — знак его раритета (Symbola):
            // указание заказчика — глиф по рангу рядом со selfцветом.
            if socket.accepts.as_deref() == Some("gem") {
                // M18 (указание заказчика): **пустой** слот после
                // извлечения камня — пустой кружочек (как у рунного),
                // а не ромбик ◈ (читался как занятый).
                let marker = match &socket.rune {
                    Some(rune) => match rune.rarity.as_deref() {
                        Some(rarity) => crate::gem_glyph::gem_symbol_div(rarity),
                        None => div().child("◈"),
                    },
                    None => div().child("\u{25CB}"),
                };
                row = row.child(marker);
                if socket.rune.is_none() {
                    row = row.child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(socket.index.to_string()),
                    );
                }
            } else {
                row = row.child(match &socket.rune {
                    Some(rune) => rune_glyph::glyph_div_for(rune.glyph.as_deref(), &rune.name),
                    // M18 (указание заказчика): пустой рунный слот —
                    // **пустой квадратик**; индекс слота рядом с рунами
                    // не показывается (как и возле занятых рун).
                    None => rune_glyph::empty_slot_glyph_div(),
                });
            }
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
                    // M18/F58f (баг «слот принимает только руны»): слот
                    // принимает по СВОЕМУ accepts — в рунный слот не
                    // предлагаем самоцветы и наоборот (ADR-012); у старого
                    // сервера поля нет — фолбэк по типу цели (§11).
                    let slot_inserts: Vec<&ItemSnapshot> = runes
                        .iter()
                        .filter(|i| match socket.accepts.as_deref() {
                            Some(accepts) => i.item_type == accepts,
                            None => inscribe_target_allowed(&i.item_type, staff.item_type.as_str()),
                        })
                        .collect();
                    let index = socket.index;
                    if slot_inserts.is_empty() {
                        row = row.child(
                            div()
                                .text_color(rgb(pal.muted))
                                .child(t(lang, Key::NoRunes)),
                        );
                    } else {
                        // M18 (указание заказчика): клик по пустому слоту
                        // открывает **новое окно** со списком самоцветов
                        // или рун для этого слота (по типу магазина в
                        // диалогах) — `pick_window`.
                        // M18 (указание заказчика): подсказка — по типу
                        // слота (самоцвет/руна), не многоточие.
                        let hint = match socket.accepts.as_deref() {
                            Some("gem") => t(lang, Key::PickGem),
                            Some(_) => t(lang, Key::PickRune),
                            None => t(
                                lang,
                                if staff.item_type == "armor" {
                                    Key::PickGem
                                } else {
                                    Key::PickRune
                                },
                            ),
                        };
                        row = row
                            .cursor_pointer()
                            .hover(|st| st.bg(rgb(pal.selection)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    // Вторая страница окна — список вставок
                                    // этого слота (кнопка «Назад» в шапке).
                                    this.socket_pick = Some(index);
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(pal.muted))
                                    .child(hint),
                            );
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
                tracing::info!("закрытие окна сокетов: кнопка «Выход»");
                this.store.close_socketing();
                let _ = cx;
            },
        ));
        col
    }
}

// ── вспомогательные ────────────────────────────────────────────────────────

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

/// M18/F58f: как рисовать слот — **рунный знаком руны** (шрифт
/// Dwarven Runes), слот самоцвета — прежними ◈/○. Пустой рунный слот
/// отличим от пустого слота самоцвета (требование заказчика).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotIcon {
    /// Рунный слот: `Some(glyph)` — заполнен (глиф руны — латинские
    /// буквы, может быть несколько; заказчик 2026-11-20), `None` — пуст.
    Rune(Option<String>),
    /// Слот самоцвета: `filled` — заполнен; `rarity` — раритет
    /// вставленного (wire `RuneInfo.rarity`): есть → знак раритета
    /// (Symbola), нет (старый сервер/запись, §11) → прежний ◈;
    /// пустой слот → ○.
    Gem {
        filled: bool,
        rarity: Option<String>,
    },
}

/// M18/F58f: иконка слота. `accepts` из wire решает; у старого сервера
/// (поля нет) — фолбэк по типу предмета (как в инкрустации, §11):
/// броня — самоцветы, прочее — руны.
pub(crate) fn slot_icon(item_type: &str, socket: &ld2_protocol::types::Socket) -> SlotIcon {
    // Неизвестное/отсутствующее значение — фолбэк по типу (§11).
    let is_gem = match socket.accepts.as_deref() {
        Some("gem") => true,
        Some("rune") => false,
        _ => item_type == "armor",
    };
    if is_gem {
        SlotIcon::Gem {
            filled: socket.rune.is_some(),
            rarity: socket.rune.as_ref().and_then(|r| r.rarity.clone()),
        }
    } else {
        SlotIcon::Rune(
            socket
                .rune
                .as_ref()
                .map(|r| crate::rune_glyph::resolve_glyph(r.glyph.as_deref(), &r.name)),
        )
    }
}

/// M18/F58f: ряд иконок слотов — **руна-слоты первыми** (требование
/// UI), далее слоты самоцветов; внутри группы — порядок `index`.
pub(crate) fn socket_icons_row(item_type: &str, sockets: &[ld2_protocol::types::Socket]) -> Div {
    let mut rune_slots: Vec<&ld2_protocol::types::Socket> = Vec::new();
    let mut gem_slots: Vec<&ld2_protocol::types::Socket> = Vec::new();
    for s in sockets {
        match s.accepts.as_deref() {
            Some("gem") => gem_slots.push(s),
            // Рунный (и неизвестный/отсутствующий — фолбэк §11) —
            // старый сервер: сохраняем прежний порядок.
            _ => rune_slots.push(s),
        }
    }
    let mut row = div().flex().flex_row().gap_1().items_center();
    for s in rune_slots.into_iter().chain(gem_slots) {
        row = row.child(slot_icon_div(item_type, s));
    }
    row
}

/// M18/F58f: одна иконка слота (глиф руны шрифтом Dwarven Runes либо ◈/○).
fn slot_icon_div(item_type: &str, socket: &ld2_protocol::types::Socket) -> Div {
    match slot_icon(item_type, socket) {
        SlotIcon::Rune(Some(glyph)) => crate::rune_glyph::glyph_div(&glyph),
        SlotIcon::Rune(None) => crate::rune_glyph::empty_slot_glyph_div(),
        // Занятый самоцвет — знак его раритета (Symbola); у старого
        // сервера без поля rarity — прежний ◈ (§11).
        SlotIcon::Gem {
            filled: true,
            rarity: Some(r),
        } => crate::gem_glyph::gem_symbol_div(&r),
        SlotIcon::Gem {
            filled: true,
            rarity: None,
        } => div().child("◈"),
        SlotIcon::Gem { filled: false, .. } => div().child("○"),
    }
}

/// M18/F58f: бейджи строкой-рядом: +N, иконки слотов, ×N (паритет
/// egui; слоты нарисованы Div'ами — смешанные шрифты, §13).
pub(crate) fn badges_row(item: &ItemSnapshot) -> Div {
    let mut row = div().flex().flex_row().gap_1().items_center();
    if item.upgrade > 0 {
        row = row.child(div().child(format!("+{}", item.upgrade)));
    }
    if let Some(sockets) = &item.sockets {
        row = row.child(socket_icons_row(item.item_type.as_str(), sockets));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        row = row.child(div().child(format!("×{qty}")));
    }
    row
}

/// M18/F58f: подпись предмета в **рюкзаке** — строка1: название,
/// +N, ×N; строка2: иконки слотов (указание заказчика: слоты — на
/// новую строку). Слоты — Div'ами: руны их шрифтом.
pub(crate) fn item_label_stack(item: &ItemSnapshot, color: u32) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .gap_1()
        .items_center()
        .text_color(rgb(color));
    // M18/F58f: у самоцвета — знак раритета (Symbola) перед названием
    // (указание заказчика: рюкзак и описание).
    if item.item_type == "gem" {
        row = row.child(crate::gem_glyph::gem_symbol_div(&item.rarity));
    }
    row = row.child(div().child(item.name.clone()));
    if item.upgrade > 0 {
        row = row.child(div().child(format!("+{}", item.upgrade)));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        row = row.child(div().child(format!("×{qty}")));
    }
    // Слоты — вплотную под названием (без промежутка), тем же
    // цветом, что и предмет (указание заказчика).
    //
    // M18/F58f (ADR-012 «делегаты списков»): вторая строка есть
    // **всегда** — у предметов без слотов невидимый заполнитель той же
    // высоты (□ как пустой слот, `invisible`): строки рюкзака одинаковой
    // высоты, а не «рваные» (одна строка vs две).
    let mut col = div().flex().flex_col().child(row);
    let second = match &item.sockets {
        Some(sockets) if !sockets.is_empty() => div()
            .text_color(rgb(color))
            .child(socket_icons_row(item.item_type.as_str(), sockets)),
        // Невидимый дубль высоты строки иконок (тот же символ, что
        // пустой руна-слот — идентичная линейная высота).
        _ => div().invisible().child("\u{25A1}"),
    };
    col = col.child(second);
    col
}

/// M18/F58f: подпись предмета в списках: название + бейджи (ряд Div'ов
/// — слоты рун рисуются их шрифтом).
pub(crate) fn item_label_row(item: &ItemSnapshot, color: u32) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_1()
        .items_center()
        .text_color(rgb(color))
        .child(div().child(item.name.clone()))
        .child(badges_row(item))
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
/// Статичное внутреннее свечение рамки (inset) для эпика/легендарки —
/// в покое; анимация (переливание) запускается только по наведению
/// (заказчик 2026-11-20). Пустой vec — без свечения.
pub(crate) fn rarity_glow_inset(rarity: &str, pal: &Palette) -> Vec<gpui::BoxShadow> {
    if !matches!(rarity, "epic" | "legendary") {
        return vec![];
    }
    let base = rarity_color(rarity, pal) & 0xff_ffff;
    vec![gpui::BoxShadow {
        color: gpui::rgba((base << 8) | 0x40).into(),
        offset: gpui::point(px(0.0), px(0.0)),
        blur_radius: px(8.0),
        spread_radius: px(0.0),
        inset: true,
    }]
}

pub(crate) fn rarity_color(rarity: &str, pal: &Palette) -> u32 {
    match rarity {
        "common" => pal.muted,
        "uncommon" => 0x98_c3_79,
        "rare" => 0x61_af_ef,
        "epic" => 0xa0_52_2d,
        // Легендарка: яркий фиолетовый (орхидея) — тёмный
        // 0x6606b2 плохо читался на тёмной теме (заказчик).
        "legendary" => 0xc0_60_f0,
        _ => pal.text,
    }
}

/// Снятие шкур (M22): цвета **качества** шкур/обрезок — своя
/// палитра (**не** цвета редкости — указание заказчика); качество =
/// ранг профессии (`hide_q1..5` / `hide_scraps_q1..5`).
pub(crate) fn quality_color(def_id: Option<&str>) -> Option<u32> {
    let id = def_id?;
    let tier = id
        .strip_prefix("hide_q")
        .or_else(|| id.strip_prefix("hide_scraps_q"))?
        .parse::<u32>()
        .ok()?;
    Some(match tier {
        1 => 0x7a_6a_55,
        2 => 0x96_79_3f,
        3 => 0xb8_86_0b,
        4 => 0xda_a5_20,
        5 => 0xff_d7_00,
        _ => return None,
    })
}

/// Цвет названия предмета: качество (снятие шкур) иначе редкость.
pub(crate) fn item_color(item: &ItemSnapshot, pal: &Palette) -> u32 {
    quality_color(item.def_id.as_deref()).unwrap_or_else(|| rarity_color(&item.rarity, pal))
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
        // M18/F54: камни усиления (заточка).
        "upgrade_stone" => t(lang, Key::ItemTypeStone),
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
        // M18/F54: качество камня усиления (процентные пункты).
        "quality" => t(lang, Key::StatQuality),
        // R-контент: метки статов руны.
        "cost" => t(lang, Key::StatCost),
        "class" => t(lang, Key::StatClass),
        other => other,
    }
    .to_string()
}

/// M18/F47: id производного параметра → название (бонусы предмета,
/// ADR-010); `damage` — общая метка «Урон».
pub(crate) fn param_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "accuracy" => Key::ParAccuracy,
        "damage" | "damage_min" | "damage_max" => Key::ParDamage,
        "crit_chance" => Key::ParCritChance,
        "crit_damage" => Key::ParCritDamage,
        "armor" => Key::ParArmor,
        "dodge" => Key::ParDodge,
        "parry_min" | "parry_max" => Key::ParParry,
        "parry_chance" => Key::ParParryChance,
        "block_min" | "block_max" => Key::ParBlock,
        "block_chance" => Key::ParBlockChance,
        "magic_defense" => Key::ParMagicDefense,
        "magic_dodge" => Key::ParMagicDodge,
        "magic_resist" => Key::ParMagicResist,
        "magic_dispel" => Key::ParMagicDispel,
        "magic_crit_damage" => Key::ParMagicCritDamage,
        "magic_crit_chance" => Key::ParMagicCritChance,
        _ => return id.to_string(),
    };
    t(lang, key).to_string()
}

/// M18: производные параметры, которые измеряются в процентах.
pub(crate) fn param_is_percent(id: &str) -> bool {
    matches!(
        id,
        "accuracy"
            | "crit_chance"
            | "crit_damage"
            | "dodge"
            | "parry_chance"
            | "block_chance"
            | "magic_dodge"
            | "magic_resist"
            | "magic_dispel"
            | "magic_crit_damage"
            | "magic_crit_chance"
    )
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

    /// M18/F58f: под инкрустацию выбирается слот, принимающий тип
    /// вставки — самоцвет не идёт в рунный слот (баг меню камня).
    #[test]
    fn first_compatible_socket_respects_accepts() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let slot = |index: u32, accepts: Option<&str>, filled: bool| Socket {
            index,
            accepts: accepts.map(str::to_string),
            rune: filled.then(|| RuneInfo {
                class: None,
                name: "x".into(),
                spell: String::new(),
                charges: 1,
                max: 1,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: None,
            }),
        };
        // Оружие: rune, rune, gem — гем идёт в слот2, руна в свободный0.
        let weapon = [
            slot(0, Some("rune"), false),
            slot(1, Some("rune"), false),
            slot(2, Some("gem"), false),
        ];
        assert_eq!(first_compatible_socket(&weapon, "gem"), Some(2));
        assert_eq!(first_compatible_socket(&weapon, "rune"), Some(0));
        // Занятый слот пропускается.
        let weapon = [
            slot(0, Some("rune"), true),
            slot(1, Some("rune"), false),
            slot(2, Some("gem"), false),
        ];
        assert_eq!(first_compatible_socket(&weapon, "rune"), Some(1));
        // Старый сервер без accepts: первый свободный (§11).
        let legacy = [slot(0, None, false), slot(1, None, false)];
        assert_eq!(first_compatible_socket(&legacy, "gem"), Some(0));
        // Подходящего слота нет — None.
        let rune_only = [slot(0, Some("rune"), true)];
        assert_eq!(first_compatible_socket(&rune_only, "gem"), None);
        assert_eq!(first_compatible_socket(&[], "rune"), None);
    }

    /// M18/F58f: дубликаты рун — вставленная руна больше не
    /// предлагается для этого предмета; чужие имена не трогаются.
    #[test]
    fn rune_already_inserted_detects_duplicates() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let socket_with = |rune_name: &str| Socket {
            index: 0,
            accepts: Some("rune".into()),
            rune: Some(RuneInfo {
                class: None,
                name: rune_name.into(),
                spell: "fire_dart".into(),
                charges: 7,
                max: 10,
                damage: None,
                def_id: None,
                glyph: Some("f".into()),
                detail: None,
                rarity: None,
            }),
        };
        let occupied = socket_with("Руна огня");
        let free = Socket {
            index: 1,
            accepts: Some("rune".into()),
            rune: None,
        };
        let sockets = [occupied, free];
        assert!(
            rune_already_inserted("Руна огня", sockets.iter()),
            "вставленная руна найдена"
        );
        assert!(
            !rune_already_inserted("Руна мороза", sockets.iter()),
            "другая руна не блокируется"
        );
        // Пустые слоты ничего не блокируют.
        let empty = [Socket {
            index: 1,
            accepts: Some("rune".into()),
            rune: None,
        }];
        assert!(!rune_already_inserted("Руна огня", empty.iter()));
    }

    /// M18/F58f: иконка слота — рунный слот даёт глиф руны (из поля
    /// glyph, фолбэк — хеш имени), слот самоцвета — ◈/○; у старого
    /// сервера без `accepts` — фолбэк по типу предмета (§11).
    #[test]
    fn slot_icon_distinguishes_runes_and_gems() {
        use ld2_protocol::types::{RuneInfo, Socket};
        let sock = |accepts: Option<&str>, filled: bool| Socket {
            index: 0,
            accepts: accepts.map(str::to_string),
            rune: filled.then(|| RuneInfo {
                class: None,
                name: "Руна огня".into(),
                spell: "fire_dart".into(),
                charges: 7,
                max: 10,
                damage: None,
                def_id: None,
                glyph: Some("f".into()),
                detail: None,
                rarity: None,
            }),
        };

        // Рунный слот: заполненный — глиф из контента (f), пустой — None.
        assert_eq!(
            slot_icon("weapon", &sock(Some("rune"), true)),
            SlotIcon::Rune(Some("f".into())),
            "заполненный рунный — глиф поля glyph"
        );
        assert_eq!(
            slot_icon("weapon", &sock(Some("rune"), false)),
            SlotIcon::Rune(None),
            "пустой рунный — отличается от пустого самоцвета"
        );
        // Слот самоцвета: пустой ○; занятый — раритет из wire (тут
        // None — фолбэк §11) либо знак раритета, если поле есть.
        assert_eq!(
            slot_icon("armor", &sock(Some("gem"), false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            }
        );
        let with_rarity = Socket {
            index: 0,
            accepts: Some("gem".into()),
            rune: Some(RuneInfo {
                class: None,
                name: "Самоцвет силы".into(),
                spell: String::new(),
                charges: 0,
                max: 0,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: Some("rare".into()),
            }),
        };
        assert_eq!(
            slot_icon("armor", &with_rarity),
            SlotIcon::Gem {
                filled: true,
                rarity: Some("rare".into())
            },
            "занятый самоцвет несёт раритет для знака"
        );
        // Старый сервер без accepts: фолбэк по типу — броня = самоцвет,
        // оружие = руна.
        assert_eq!(
            slot_icon("armor", &sock(None, false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            },
            "фолбэк: броня — слоты самоцветов"
        );
        assert_eq!(
            slot_icon("weapon", &sock(None, true)),
            SlotIcon::Rune(Some("f".into())),
            "фолбэк: оружие — рунные слоты"
        );
        // Неизвестное значение accepts — как отсутствующее (§11).
        assert_eq!(
            slot_icon("armor", &sock(Some("mystery"), false)),
            SlotIcon::Gem {
                filled: false,
                rarity: None
            }
        );
        // Занятый рунный слот — РОВНО глиф (не ◈, не квадрат):
        // пустой (□) и занятый (глиф руны) различимы.
        let occupied = slot_icon("weapon", &sock(Some("rune"), true));
        let empty = slot_icon("weapon", &sock(Some("rune"), false));
        assert_eq!(occupied, SlotIcon::Rune(Some("f".into())));
        assert_ne!(
            format!("{occupied:?}"),
            format!("{empty:?}"),
            "занят ≠ пуст"
        );
    }

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

    /// M18/F58 (ADR-012): вставку решает `accepts` слота — отсюда
    /// самоцветы в оружие и отказ «руна в гемный слот»; у старого
    /// сервера (поля нет) — прежнее правило по типу предмета.
    #[test]
    fn inscribe_target_possible_by_socket_accepts() {
        use ld2_protocol::types::{ItemSnapshot, Socket};

        let item = |item_type: &str, sockets: Vec<Socket>| ItemSnapshot {
            slot: None,
            hands: None,
            def_id: None,
            uid: "i_t".into(),
            item_type: item_type.into(),
            name: "Тест".into(),
            rarity: "common".into(),
            upgrade: 0,
            stats: Default::default(),
            requires: None,
            sockets: Some(sockets),
            qty: Some(1),
            gold_value: None,
            bonuses: None,
            extra: Default::default(),
        };
        let free = |accepts: Option<&str>, index: u32| Socket {
            index,
            rune: None,
            accepts: accepts.map(str::to_string),
        };
        let filled = |index: u32| Socket {
            index,
            rune: Some(ld2_protocol::types::RuneInfo {
                class: None,
                name: "Руна".into(),
                spell: "fire_dart".into(),
                charges: 1,
                max: 1,
                damage: None,
                def_id: None,
                glyph: None,
                detail: None,
                rarity: None,
            }),
            accepts: Some("rune".into()),
        };

        // Новый сервер: тип слота решает всё.
        let sword = item(
            "weapon",
            vec![free(Some("gem"), 0), free(Some("rune"), 1), filled(2)],
        );
        assert!(
            inscribe_target_possible("gem", &sword),
            "самоцвет в оружие — слот accepts:gem (F58)"
        );
        assert!(
            inscribe_target_possible("rune", &sword),
            "руна в рунный слот"
        );
        assert!(
            !inscribe_target_possible("potion", &sword),
            "не инкрустация"
        );

        // Занятый слот не предлагается.
        let only_filled = item("weapon", vec![filled(0)]);
        assert!(
            !inscribe_target_possible("rune", &only_filled),
            "слот занят"
        );

        // Гемный слот руне не подходит.
        let gem_only = item("weapon", vec![free(Some("gem"), 0)]);
        assert!(
            !inscribe_target_possible("rune", &gem_only),
            "руна в гемный слот"
        );

        // Старый сервер (без accepts) — прежнее правило по типу.
        let legacy_weapon = item("weapon", vec![free(None, 0)]);
        assert!(
            inscribe_target_possible("rune", &legacy_weapon),
            "фолбэк: руна в оружие"
        );
        assert!(
            !inscribe_target_possible("gem", &legacy_weapon),
            "фолбэк: самоцвет в оружие — как раньше"
        );
        let legacy_armor = item("armor", vec![free(None, 0)]);
        assert!(
            inscribe_target_possible("gem", &legacy_armor),
            "фолбэк: гем в броню"
        );
        assert!(
            !inscribe_target_possible("rune", &legacy_armor),
            "фолбэк: руна в броню"
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
    /// (Параметры | Навыки | Репутация + нетронутые); M16/F29b:
    /// «Инвентарь» убран (окно рюкзака — отдельное); M21: «Профессии»
    /// убраны (отдельное окно, как квесты).
    #[test]
    fn character_window_tabs_cover_spec() {
        let tabs: Vec<&'static str> = SheetTab::all().iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tabs,
            ["params", "skills", "reputation", "guild"],
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
