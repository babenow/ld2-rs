//! Корневое представление редактора (E3): разделы, списки, инспектор,
//! сохранение/undo/redo, панель валидации.
//!
//! Логика — в [`crate::project::EditorProject`] (без gpui, юнит-тесты);
//! здесь только рендер и обработка ввода.

use ely::forms::{Choice, Input, InputEvent, Select, TextInput};
use gpui::{
    div, prelude::*, px, rgb, uniform_list, Context, ElementId, EmptyView, Entity, IntoElement,
    Render, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
};
use ld2_content::{
    EntityKind, EFFECT_KINDS, ITEM_ATTRS, ITEM_CLASSES, ITEM_PARAMS, ITEM_RARITIES, ITEM_SLOTS,
    SPELL_SCHOOLS,
};
use serde_json::Value;

use crate::map::{auto_layout, compass_dir, node_size, snap_cell, MapLayout};
use crate::project::{Edit, EditorProject, PathSeg, RefKind};
use ld2_content::ContentFile;

const BG: u32 = 0x1e1f22;
const PANEL: u32 = 0x24262b;
const PANEL_ALT: u32 = 0x2a2d33;
const BORDER: u32 = 0x3a3d44;
const TEXT: u32 = 0xd7d9de;
const MUTED: u32 = 0x8b8f98;
const ACCENT: u32 = 0x4c9aff;
const OK: u32 = 0x61c66a;
const WARN: u32 = 0xd8a657;
const ERR: u32 = 0xe06c75;

/// Раздел левой рейки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Items,
    Locations,
    Map,
    Dialogs,
    Shops,
    Quests,
    Recipes,
    Skills,
    Professions,
    Help,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Items => "Предметы",
            Section::Locations => "Локации",
            Section::Map => "Карта",
            Section::Dialogs => "Диалоги",
            Section::Shops => "Магазины",
            Section::Quests => "Квесты",
            Section::Recipes => "Рецепты",
            Section::Skills => "Навыки",
            Section::Professions => "Профессии",
            Section::Help => "Помощь",
        }
    }
}

/// Строка палитры быстрого перехода (E6).
#[derive(Clone)]
struct QuickEntry {
    section: Section,
    index: Option<usize>,
    label: String,
}

/// Группа записей верхнего уровня с таблицей полей (E5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldGroup {
    Dialogs,
    Shops,
    Quests,
    Recipes,
    Skills,
    Professions,
}

impl FieldGroup {
    fn section(self) -> Section {
        match self {
            FieldGroup::Dialogs => Section::Dialogs,
            FieldGroup::Shops => Section::Shops,
            FieldGroup::Quests => Section::Quests,
            FieldGroup::Recipes => Section::Recipes,
            FieldGroup::Skills => Section::Skills,
            FieldGroup::Professions => Section::Professions,
        }
    }

    fn file(self) -> ContentFile {
        match self {
            FieldGroup::Dialogs => ContentFile::Dialogs,
            FieldGroup::Shops => ContentFile::Shops,
            FieldGroup::Quests => ContentFile::Quests,
            FieldGroup::Recipes => ContentFile::Recipes,
            FieldGroup::Skills => ContentFile::Skills,
            FieldGroup::Professions => ContentFile::Professions,
        }
    }
}

/// Вид поля формы (преобразование текста в JSON).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Text,
    Int,
    Float,
    Json,
}

/// Описание поля формы: путь задаётся относительно записи массива.
#[derive(Clone)]
struct FieldSpec {
    label: &'static str,
    input: Entity<TextInput>,
    path: &'static [&'static str],
    kind: FieldKind,
    picker: Option<RefKind>,
    /// Относительный путь ключа, удаляемого при пустом поле (необязательные).
    clear: Option<&'static [&'static str]>,
}

/// Открытый пикер-ссылка (E5): поиск по контенту нужного типа.
struct PickerState {
    kind: RefKind,
    title: &'static str,
    target: Entity<TextInput>,
    filter: String,
    search: Entity<TextInput>,
}

/// Название типа ссылки для заголовка пикера.
fn ref_kind_title(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Item => "предмет",
        RefKind::ItemType => "тип предмета",
        RefKind::Skill => "навык",
        RefKind::Profession => "профессия",
        RefKind::Location => "локация",
        RefKind::Npc => "НПС",
        RefKind::Monster => "монстр",
        RefKind::Station => "станок (свойство объекта)",
        RefKind::Quest => "квест",
        RefKind::DialogLine => "строка диалога",
    }
}

/// Текст поля из JSON-значения (для перечитывания формы).
fn value_text(kind: FieldKind, value: Option<&Value>) -> String {
    match kind {
        FieldKind::Text => value
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        FieldKind::Int => value
            .and_then(Value::as_i64)
            .map(|number| number.to_string())
            .unwrap_or_default(),
        FieldKind::Float => value
            .and_then(Value::as_f64)
            .map(|number| number.to_string())
            .unwrap_or_default(),
        FieldKind::Json => value
            .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
            .unwrap_or_default(),
    }
}

/// JSON-значение поля из текста формы.
fn value_from_text(kind: FieldKind, text: &str) -> Result<Value, String> {
    match kind {
        FieldKind::Text => Ok(Value::from(text)),
        FieldKind::Int => text
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("не целое число: {text}")),
        FieldKind::Float => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(Value::from)
            .ok_or_else(|| format!("не число: {text}")),
        FieldKind::Json => serde_json::from_str(text).map_err(|err| err.to_string()),
    }
}

/// Корневое представление окна редактора.
pub struct EditorApp {
    project: Option<EditorProject>,
    open_error: Option<String>,
    section: Section,
    filter: String,
    status: String,

    // выбор в списках
    selected_item: Option<usize>,
    selected_location: Option<usize>,
    selected_exit: Option<usize>,
    selected_entity: Option<usize>,

    // карта-полотно (E4)
    map: Option<MapLayout>,
    map_zoom: f32,
    map_pan: (f32, f32),
    map_pan_last: Option<(f32, f32)>,
    map_viewport: (f32, f32),
    map_centered: bool,
    map_dirty: bool,
    map_link: bool,
    map_dragging: Option<String>,
    map_drag_grab: (f32, f32),
    /// Скролл виртуализованного списка локаций (1189 строк без лагов).
    locations_scroll: UniformListScrollHandle,

    // поля поиска
    search: Option<Entity<TextInput>>,

    // форма предмета (E6.2: структурные зоны без JSON)
    item_id: Option<Entity<TextInput>>,
    item_name: Option<Entity<TextInput>>,
    item_gold: Option<Entity<TextInput>>,
    item_qty: Option<Entity<TextInput>>,
    item_upgrade: Option<Entity<TextInput>>,
    item_sockets: Option<Entity<TextInput>>,
    item_rune_sockets: Option<Entity<TextInput>>,
    item_hands: Option<Entity<TextInput>>,
    item_cooldown: Option<Entity<TextInput>>,
    // статы предмета (stats)
    stat_dmg: Option<Entity<TextInput>>,
    stat_armor: Option<Entity<TextInput>>,
    stat_magic: Option<Entity<TextInput>>,
    stat_heal: Option<Entity<TextInput>>,
    stat_quality: Option<Entity<TextInput>>,
    stat_damage: Option<Entity<TextInput>>,
    stat_chance: Option<Entity<TextInput>>,
    stat_charges: Option<Entity<TextInput>>,
    stat_max: Option<Entity<TextInput>>,
    stat_cost: Option<Entity<TextInput>>,
    stat_glyph: Option<Entity<TextInput>>,
    stat_spell: Option<Entity<TextInput>>,
    stat_value: Option<Entity<TextInput>>,
    // параметры stats.param и бонусы ADR-010
    selected_param: Option<String>,
    param_value: Option<Entity<TextInput>>,
    selected_bonus: Option<usize>,
    bonus_value: Option<Entity<TextInput>>,
    // автобаффы stats.buff / stats.buff2
    buff_value: Option<Entity<TextInput>>,
    buff_duration: Option<Entity<TextInput>>,
    buff_stacks: Option<Entity<TextInput>>,
    buff2_value: Option<Entity<TextInput>>,
    buff2_duration: Option<Entity<TextInput>>,
    buff2_stacks: Option<Entity<TextInput>>,
    // эффект stats.effect
    effect_value: Option<Entity<TextInput>>,
    effect_duration: Option<Entity<TextInput>>,
    effect_period: Option<Entity<TextInput>>,
    // требования requires
    req_str: Option<Entity<TextInput>>,
    req_dex: Option<Entity<TextInput>>,
    req_vit: Option<Entity<TextInput>>,
    req_int: Option<Entity<TextInput>>,
    req_level: Option<Entity<TextInput>>,

    // форма локации
    loc_name: Option<Entity<TextInput>>,
    loc_text: Option<Entity<TextInput>>,
    loc_flags: Option<Entity<TextInput>>,
    loc_spawn: Option<Entity<TextInput>>,

    // форма выхода
    exit_label: Option<Entity<TextInput>>,
    exit_dir: Option<Entity<TextInput>>,
    exit_to: Option<Entity<TextInput>>,

    // форма сущности
    ent_kind: Option<Entity<TextInput>>,
    ent_name: Option<Entity<TextInput>>,
    ent_extra: Option<Entity<TextInput>>,

    // E5: выбор и таблицы полей записей
    selected_quest: Option<usize>,
    selected_goal: Option<usize>,
    selected_reward_item: Option<usize>,
    selected_recipe: Option<usize>,
    selected_recipe_input: Option<usize>,
    selected_skill: Option<usize>,
    selected_profession: Option<usize>,
    quest_fields: Vec<FieldSpec>,
    recipe_fields: Vec<FieldSpec>,
    skill_fields: Vec<FieldSpec>,
    profession_fields: Vec<FieldSpec>,
    picker_search: Option<Entity<TextInput>>,
    picker: Option<PickerState>,

    // E5: поля квеста
    q_id: Option<Entity<TextInput>>,
    q_title: Option<Entity<TextInput>>,
    q_giver: Option<Entity<TextInput>>,
    q_reward_label: Option<Entity<TextInput>>,
    q_xp: Option<Entity<TextInput>>,
    q_rank_up: Option<Entity<TextInput>>,

    // E5: поля рецепта
    r_id: Option<Entity<TextInput>>,
    r_name: Option<Entity<TextInput>>,
    r_profession: Option<Entity<TextInput>>,
    r_rank: Option<Entity<TextInput>>,
    r_station: Option<Entity<TextInput>>,
    r_station_label: Option<Entity<TextInput>>,
    r_time: Option<Entity<TextInput>>,
    r_xp: Option<Entity<TextInput>>,
    r_tool: Option<Entity<TextInput>>,
    r_output_type: Option<Entity<TextInput>>,
    r_output_qty: Option<Entity<TextInput>>,

    // E5: поля навыка/профессии
    s_id: Option<Entity<TextInput>>,
    s_name: Option<Entity<TextInput>>,
    s_group: Option<Entity<TextInput>>,
    s_desc: Option<Entity<TextInput>>,
    p_id: Option<Entity<TextInput>>,
    p_name: Option<Entity<TextInput>>,
    p_type: Option<Entity<TextInput>>,

    // E5: подсписки — цель квеста, награда, ингредиент рецепта
    goal_kind: Option<Entity<TextInput>>,
    goal_target: Option<Entity<TextInput>>,
    goal_need: Option<Entity<TextInput>>,
    goal_text: Option<Entity<TextInput>>,
    reward_type: Option<Entity<TextInput>>,
    reward_qty: Option<Entity<TextInput>>,
    input_type: Option<Entity<TextInput>>,
    input_qty: Option<Entity<TextInput>>,

    // E5.2: диалоги и магазины
    selected_dialog: Option<usize>,
    selected_dialog_line: Option<usize>,
    selected_dialog_reply: Option<usize>,
    selected_training: Option<usize>,
    selected_shop: Option<usize>,
    selected_currency: Option<usize>,
    selected_shop_item: Option<usize>,
    dialog_fields: Vec<FieldSpec>,
    shop_fields: Vec<FieldSpec>,
    dlg_id: Option<Entity<TextInput>>,
    dlg_root: Option<Entity<TextInput>>,
    shp_id: Option<Entity<TextInput>>,
    shp_markup: Option<Entity<TextInput>>,
    line_id: Option<Entity<TextInput>>,
    line_text: Option<Entity<TextInput>>,
    line_quest: Option<Entity<TextInput>>,
    line_every: Option<Entity<TextInput>>,
    line_when: Option<Entity<TextInput>>,
    reply_id: Option<Entity<TextInput>>,
    reply_text: Option<Entity<TextInput>>,
    reply_next: Option<Entity<TextInput>>,
    reply_action: Option<Entity<TextInput>>,
    reply_when: Option<Entity<TextInput>>,
    train_skill: Option<Entity<TextInput>>,
    train_points: Option<Entity<TextInput>>,
    train_gold: Option<Entity<TextInput>>,
    curr_currency: Option<Entity<TextInput>>,
    curr_rate: Option<Entity<TextInput>>,
    shop_item_id: Option<Entity<TextInput>>,
    // E6: быстрый переход, справка, seed.
    quick_open: bool,
    quick_filter: String,
    quick_selected: usize,
    seed_running: bool,
    seed_log: Option<String>,
}

impl EditorApp {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let (project, open_error) = match std::env::current_dir() {
            Ok(cwd) => match EditorProject::discover(&cwd) {
                Ok(project) => (Some(project), None),
                Err(err) => (None, Some(err.to_string())),
            },
            Err(err) => (None, Some(format!("не удалось определить CWD: {err}"))),
        };
        let map = project.as_ref().map(|project| {
            let mut layout = MapLayout::load(project.content_dir())
                .ok()
                .flatten()
                .unwrap_or_default();
            layout.merge_auto(&project.content().locations);
            layout
        });
        Self {
            project,
            open_error,
            section: Section::Map,
            filter: String::new(),
            status: "E4: карта — колесо/± зум, drag — панорама, клик — узел".into(),
            selected_item: None,
            selected_location: None,
            selected_exit: None,
            selected_entity: None,
            map,
            map_zoom: 1.0,
            map_pan: (0.0, 0.0),
            map_pan_last: None,
            map_viewport: (1200.0, 700.0),
            map_centered: false,
            map_dirty: false,
            map_link: false,
            map_dragging: None,
            map_drag_grab: (0.0, 0.0),
            locations_scroll: UniformListScrollHandle::new(),
            search: None,
            item_id: None,
            item_name: None,
            item_gold: None,
            item_qty: None,
            item_upgrade: None,
            item_sockets: None,
            item_rune_sockets: None,
            item_hands: None,
            item_cooldown: None,
            stat_dmg: None,
            stat_armor: None,
            stat_magic: None,
            stat_heal: None,
            stat_quality: None,
            stat_damage: None,
            stat_chance: None,
            stat_charges: None,
            stat_max: None,
            stat_cost: None,
            stat_glyph: None,
            stat_spell: None,
            stat_value: None,
            selected_param: None,
            param_value: None,
            selected_bonus: None,
            bonus_value: None,
            buff_value: None,
            buff_duration: None,
            buff_stacks: None,
            buff2_value: None,
            buff2_duration: None,
            buff2_stacks: None,
            effect_value: None,
            effect_duration: None,
            effect_period: None,
            req_str: None,
            req_dex: None,
            req_vit: None,
            req_int: None,
            req_level: None,
            loc_name: None,
            loc_text: None,
            loc_flags: None,
            loc_spawn: None,
            exit_label: None,
            exit_dir: None,
            exit_to: None,
            ent_kind: None,
            ent_name: None,
            ent_extra: None,
            selected_quest: None,
            selected_goal: None,
            selected_reward_item: None,
            selected_recipe: None,
            selected_recipe_input: None,
            selected_skill: None,
            selected_profession: None,
            quest_fields: Vec::new(),
            recipe_fields: Vec::new(),
            skill_fields: Vec::new(),
            profession_fields: Vec::new(),
            picker_search: None,
            picker: None,
            q_id: None,
            q_title: None,
            q_giver: None,
            q_reward_label: None,
            q_xp: None,
            q_rank_up: None,
            r_id: None,
            r_name: None,
            r_profession: None,
            r_rank: None,
            r_station: None,
            r_station_label: None,
            r_time: None,
            r_xp: None,
            r_tool: None,
            r_output_type: None,
            r_output_qty: None,
            s_id: None,
            s_name: None,
            s_group: None,
            s_desc: None,
            p_id: None,
            p_name: None,
            p_type: None,
            goal_kind: None,
            goal_target: None,
            goal_need: None,
            goal_text: None,
            reward_type: None,
            reward_qty: None,
            input_type: None,
            input_qty: None,
            selected_dialog: None,
            selected_dialog_line: None,
            selected_dialog_reply: None,
            selected_training: None,
            selected_shop: None,
            selected_currency: None,
            selected_shop_item: None,
            dialog_fields: Vec::new(),
            shop_fields: Vec::new(),
            dlg_id: None,
            dlg_root: None,
            shp_id: None,
            shp_markup: None,
            line_id: None,
            line_text: None,
            line_quest: None,
            line_every: None,
            line_when: None,
            reply_id: None,
            reply_text: None,
            reply_next: None,
            reply_action: None,
            reply_when: None,
            train_skill: None,
            train_points: None,
            train_gold: None,
            curr_currency: None,
            curr_rate: None,
            shop_item_id: None,
            quick_open: false,
            quick_filter: String::new(),
            quick_selected: 0,
            seed_running: false,
            seed_log: None,
        }
    }

    /// Ctrl/Cmd+S — сохранить, Ctrl/Cmd+Z — undo, Shift+Z / Y — redo,
    /// Ctrl/Cmd+R — перечитать с диска.
    pub fn handle_keystroke(
        &mut self,
        ks: &gpui::Keystroke,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // E6: пока открыта палитра быстрого перехода, ввод идёт в неё.
        if self.quick_open {
            match ks.key.as_str() {
                "escape" => {
                    self.quick_open = false;
                    self.status = "быстрый переход: отменён".into();
                }
                "enter" => self.quick_activate_selected(cx),
                "up" => self.quick_move(-1, cx),
                "down" => self.quick_move(1, cx),
                "backspace" => {
                    self.quick_filter.pop();
                    self.quick_selected = 0;
                }
                key => {
                    if !ks.modifiers.control
                        && !ks.modifiers.platform
                        && !ks.modifiers.alt
                        && key.chars().count() == 1
                    {
                        self.quick_filter.push_str(key);
                        self.quick_selected = 0;
                    }
                }
            }
            cx.notify();
            return;
        }
        if (ks.modifiers.control || ks.modifiers.platform) && ks.key == "p" {
            self.quick_open = true;
            self.quick_filter.clear();
            self.quick_selected = 0;
            self.status = "быстрый переход: введите часть id/имени, Enter — выбрать".into();
            cx.notify();
            return;
        }
        if ks.key == "f1" {
            self.switch_section(Section::Help, cx);
            return;
        }
        // Карта: стрелки двигают выбранный узел, +/- — зум.
        if self.section == Section::Map && !ks.modifiers.control && !ks.modifiers.platform {
            match ks.key.as_str() {
                "left" => return self.move_map_node(-1, 0, cx),
                "right" => return self.move_map_node(1, 0, cx),
                "up" => return self.move_map_node(0, -1, cx),
                "down" => return self.move_map_node(0, 1, cx),
                "+" | "=" => return self.zoom_step(1.2, cx),
                "-" => return self.zoom_step(1.0 / 1.2, cx),
                "l" => return self.toggle_link_mode(cx),
                _ => {}
            }
        }
        let cmd = ks.modifiers.control || ks.modifiers.platform;
        if !cmd {
            return;
        }
        match ks.key.as_str() {
            "s" => self.save(cx),
            "z" if ks.modifiers.shift => self.redo(cx),
            "z" => self.undo(cx),
            "y" => self.redo(cx),
            "r" => self.reload(cx),
            _ => {}
        }
    }

    // ─── ввод ────────────────────────────────────────────────────────────

    fn init_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.search.is_some() {
            return;
        }
        let search = cx.new(|cx| TextInput::new(window, cx).placeholder("Поиск по id / имени"));
        self.search = Some(search.clone());
        let mk = |window: &mut Window, cx: &mut Context<'_, Self>, placeholder: &'static str| {
            cx.new(|cx| TextInput::new(window, cx).placeholder(placeholder))
        };
        self.item_id = Some(mk(window, cx, "id предмета"));
        self.item_name = Some(mk(window, cx, "Имя предмета"));
        self.item_gold = Some(mk(window, cx, "Цена (золото)"));
        self.item_qty = Some(mk(window, cx, "Количество в стаке"));
        self.item_upgrade = Some(mk(window, cx, "Предел заточки (upgrade)"));
        self.item_sockets = Some(mk(window, cx, "Сокетов всего"));
        self.item_rune_sockets = Some(mk(window, cx, "Из них под руны"));
        self.item_hands = Some(mk(window, cx, "Рук (1/2)"));
        self.item_cooldown = Some(mk(window, cx, "Кулдаун, с (1.0)"));
        self.stat_dmg = Some(mk(window, cx, "урон «6..9»"));
        self.stat_armor = Some(mk(window, cx, "броня"));
        self.stat_magic = Some(mk(window, cx, "магия"));
        self.stat_heal = Some(mk(window, cx, "лечение"));
        self.stat_quality = Some(mk(window, cx, "качество (-5/0)"));
        self.stat_damage = Some(mk(window, cx, "урон заклинания"));
        self.stat_chance = Some(mk(window, cx, "шанс"));
        self.stat_charges = Some(mk(window, cx, "заряды"));
        self.stat_max = Some(mk(window, cx, "макс. зарядов"));
        self.stat_cost = Some(mk(window, cx, "стоимость каста"));
        self.stat_glyph = Some(mk(window, cx, "глиф руны"));
        self.stat_spell = Some(mk(window, cx, "id заклинания"));
        self.stat_value = Some(mk(window, cx, "значение (attr)"));
        self.param_value = Some(mk(window, cx, "значение параметра"));
        self.bonus_value = Some(mk(window, cx, "значение бонуса"));
        self.buff_value = Some(mk(window, cx, "значение"));
        self.buff_duration = Some(mk(window, cx, "секунд"));
        self.buff_stacks = Some(mk(window, cx, "стаков"));
        self.buff2_value = Some(mk(window, cx, "значение"));
        self.buff2_duration = Some(mk(window, cx, "секунд"));
        self.buff2_stacks = Some(mk(window, cx, "стаков"));
        self.effect_value = Some(mk(window, cx, "урон эффекта"));
        self.effect_duration = Some(mk(window, cx, "секунд"));
        self.effect_period = Some(mk(window, cx, "период, с"));
        self.req_str = Some(mk(window, cx, "str"));
        self.req_dex = Some(mk(window, cx, "dex"));
        self.req_vit = Some(mk(window, cx, "vit"));
        self.req_int = Some(mk(window, cx, "int"));
        self.req_level = Some(mk(window, cx, "уровень"));
        self.loc_name = Some(mk(window, cx, "Имя локации"));
        self.loc_text = Some(mk(window, cx, "Текст/описание (E3: одна строка)"));
        self.loc_flags = Some(mk(window, cx, "Флаги через запятую: safe, pvp"));
        self.loc_spawn = Some(mk(window, cx, "spawn: true/false"));
        self.exit_label = Some(mk(window, cx, "Подпись выхода"));
        self.exit_dir = Some(mk(window, cx, "dir: north/south/… (пусто — нет)"));
        self.exit_to = Some(mk(window, cx, "to: id целевой локации"));
        self.ent_kind = Some(mk(window, cx, "kind: monster/npc/object/resource"));
        self.ent_name = Some(mk(window, cx, "Имя сущности"));
        self.ent_extra = Some(mk(window, cx, "extra — JSON-объект (hp/difficulty/…) "));

        // ── E5: поля записей ──
        self.q_id = Some(mk(window, cx, "id квеста"));
        self.q_title = Some(mk(window, cx, "Заголовок"));
        self.q_giver = Some(mk(window, cx, "giver: id НПС/локации (пусто — нет)"));
        self.q_reward_label = Some(mk(window, cx, "reward_label (пусто — нет)"));
        self.q_xp = Some(mk(window, cx, "reward.xp (целое)"));
        self.q_rank_up = Some(mk(window, cx, "reward.rank_up: профессия (пусто — нет)"));

        self.r_id = Some(mk(window, cx, "id рецепта"));
        self.r_name = Some(mk(window, cx, "Имя рецепта"));
        self.r_profession = Some(mk(window, cx, "profession: id профессии"));
        self.r_rank = Some(mk(window, cx, "rank (целое)"));
        self.r_station = Some(mk(window, cx, "station: станок (пусто — нет)"));
        self.r_station_label = Some(mk(window, cx, "station_label (пусто — нет)"));
        self.r_time = Some(mk(window, cx, "time_s (целое)"));
        self.r_xp = Some(mk(window, cx, "xp.profession (целое)"));
        self.r_tool = Some(mk(window, cx, "tool.type: тип предмета (пусто — нет)"));
        self.r_output_type = Some(mk(window, cx, "output.type: id предмета"));
        self.r_output_qty = Some(mk(window, cx, "output.qty (целое)"));

        self.s_id = Some(mk(window, cx, "id навыка"));
        self.s_name = Some(mk(window, cx, "Имя навыка"));
        self.s_group = Some(mk(window, cx, "group (misc/combat/…)"));
        self.s_desc = Some(mk(window, cx, "Описание (пусто — нет)"));
        self.p_id = Some(mk(window, cx, "id профессии"));
        self.p_name = Some(mk(window, cx, "Имя профессии"));
        self.p_type = Some(mk(window, cx, "type: primary/secondary (пусто — primary)"));

        self.goal_kind = Some(mk(window, cx, "type: kill"));
        self.goal_target = Some(mk(window, cx, "target: id монстра"));
        self.goal_need = Some(mk(window, cx, "need (целое)"));
        self.goal_text = Some(mk(window, cx, "text цели"));
        self.reward_type = Some(mk(window, cx, "type: id предмета"));
        self.reward_qty = Some(mk(window, cx, "qty (целое)"));
        self.input_type = Some(mk(window, cx, "type: id/тип предмета"));
        self.input_qty = Some(mk(window, cx, "qty (целое)"));

        // ── E5.2: диалоги и магазины ──
        self.dlg_id = Some(mk(window, cx, "id НПС диалога"));
        self.dlg_root = Some(mk(window, cx, "root: id стартовой строки"));
        self.shp_id = Some(mk(window, cx, "id НПС магазина"));
        self.shp_markup = Some(mk(window, cx, "markup (число, напр. 1.15)"));
        self.line_id = Some(mk(window, cx, "id строки"));
        self.line_text = Some(mk(window, cx, "Текст строки диалога"));
        self.line_quest = Some(mk(window, cx, "quest: id квеста (пусто — нет)"));
        self.line_every = Some(mk(window, cx, "every_s (целое, пусто — нет)"));
        self.line_when = Some(mk(window, cx, "when — JSON (пусто — нет)"));
        self.reply_id = Some(mk(window, cx, "id реплики"));
        self.reply_text = Some(mk(window, cx, "Текст реплики игрока"));
        self.reply_next = Some(mk(window, cx, "next: id строки (пусто — нет)"));
        self.reply_action = Some(mk(window, cx, "action: shop/train (пусто — нет)"));
        self.reply_when = Some(mk(window, cx, "when — JSON (пусто — нет)"));
        self.train_skill = Some(mk(window, cx, "skill: id навыка"));
        self.train_points = Some(mk(window, cx, "points (целое)"));
        self.train_gold = Some(mk(window, cx, "gold (целое)"));
        self.curr_currency = Some(mk(window, cx, "currency: id предмета-валюты"));
        self.curr_rate = Some(mk(window, cx, "rate (число)"));
        self.shop_item_id = Some(mk(window, cx, "id товара (предмет)"));

        let picker_search = mk(window, cx, "Поиск ссылки");
        self.picker_search = Some(picker_search.clone());

        let spec = |label: &'static str,
                    input: &Option<Entity<TextInput>>,
                    path: &'static [&'static str],
                    kind: FieldKind,
                    picker: Option<RefKind>,
                    clear: Option<&'static [&'static str]>| FieldSpec {
            label,
            input: input.clone().expect("поле создано"),
            path,
            kind,
            picker,
            clear,
        };
        use FieldKind::{Float, Int, Json, Text};
        self.quest_fields = vec![
            spec("id", &self.q_id, &["id"], Text, None, None),
            spec("title", &self.q_title, &["title"], Text, None, None),
            spec(
                "giver",
                &self.q_giver,
                &["giver"],
                Text,
                Some(RefKind::Npc),
                Some(&["giver"]),
            ),
            spec(
                "reward_label",
                &self.q_reward_label,
                &["reward_label"],
                Text,
                None,
                Some(&["reward_label"]),
            ),
            spec("reward.xp", &self.q_xp, &["reward", "xp"], Int, None, None),
            spec(
                "reward.rank_up",
                &self.q_rank_up,
                &["reward", "rank_up"],
                Text,
                Some(RefKind::Profession),
                Some(&["reward", "rank_up"]),
            ),
        ];
        self.recipe_fields = vec![
            spec("id", &self.r_id, &["id"], Text, None, None),
            spec("name", &self.r_name, &["name"], Text, None, None),
            spec(
                "profession",
                &self.r_profession,
                &["profession"],
                Text,
                Some(RefKind::Profession),
                None,
            ),
            spec("rank", &self.r_rank, &["rank"], Int, None, None),
            spec(
                "station",
                &self.r_station,
                &["station"],
                Text,
                Some(RefKind::Station),
                Some(&["station"]),
            ),
            spec(
                "station_label",
                &self.r_station_label,
                &["station_label"],
                Text,
                None,
                Some(&["station_label"]),
            ),
            spec("time_s", &self.r_time, &["time_s"], Int, None, None),
            spec(
                "xp.profession",
                &self.r_xp,
                &["xp", "profession"],
                Int,
                None,
                None,
            ),
            spec(
                "tool.type",
                &self.r_tool,
                &["tool", "type"],
                Text,
                Some(RefKind::ItemType),
                Some(&["tool"]),
            ),
            spec(
                "output.type",
                &self.r_output_type,
                &["output", "type"],
                Text,
                Some(RefKind::Item),
                None,
            ),
            spec(
                "output.qty",
                &self.r_output_qty,
                &["output", "qty"],
                Int,
                None,
                None,
            ),
        ];
        self.skill_fields = vec![
            spec("id", &self.s_id, &["id"], Text, None, None),
            spec("name", &self.s_name, &["name"], Text, None, None),
            spec("group", &self.s_group, &["group"], Text, None, None),
            spec(
                "description",
                &self.s_desc,
                &["description"],
                Text,
                None,
                Some(&["description"]),
            ),
        ];
        self.profession_fields = vec![
            spec("id", &self.p_id, &["id"], Text, None, None),
            spec("name", &self.p_name, &["name"], Text, None, None),
            spec("type", &self.p_type, &["type"], Text, None, Some(&["type"])),
        ];
        self.dialog_fields = vec![
            spec("id", &self.dlg_id, &["id"], Text, None, None),
            spec(
                "root",
                &self.dlg_root,
                &["root"],
                Text,
                Some(RefKind::DialogLine),
                Some(&["root"]),
            ),
        ];
        self.shop_fields = vec![
            spec("id", &self.shp_id, &["id"], Text, None, None),
            spec("markup", &self.shp_markup, &["markup"], Float, None, None),
        ];
        // json-полей в этих формах пока нет; импорт Json оставлен для будущих.
        let _ = Json;

        cx.subscribe(&search, |this, field, event: &InputEvent, cx| {
            if *event == InputEvent::Changed {
                this.filter = field.read(cx).text().to_string();
                // Сужение фильтра возвращает прокрутку к началу списка.
                this.locations_scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        })
        .detach();

        let submit_groups: [(&[&Option<Entity<TextInput>>], ApplyKind); 3] = [
            (
                &[
                    &self.loc_name,
                    &self.loc_text,
                    &self.loc_flags,
                    &self.loc_spawn,
                ],
                ApplyKind::Location,
            ),
            (
                &[&self.exit_label, &self.exit_dir, &self.exit_to],
                ApplyKind::Exit,
            ),
            (
                &[&self.ent_kind, &self.ent_name, &self.ent_extra],
                ApplyKind::Entity,
            ),
        ];
        for (fields, kind) in submit_groups {
            for field in fields.iter().filter_map(|slot| (*slot).clone()) {
                cx.subscribe(&field, move |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Submit {
                        this.apply(kind, cx);
                    }
                })
                .detach();
            }
        }

        // E5: Enter в поле записи применяет всю группу полей.
        let spec_groups: [(ApplyKind, Vec<FieldSpec>); 6] = [
            (ApplyKind::Dialogs, self.dialog_fields.clone()),
            (ApplyKind::Shops, self.shop_fields.clone()),
            (ApplyKind::Quests, self.quest_fields.clone()),
            (ApplyKind::Recipes, self.recipe_fields.clone()),
            (ApplyKind::Skills, self.skill_fields.clone()),
            (ApplyKind::Professions, self.profession_fields.clone()),
        ];
        for (kind, specs) in spec_groups {
            for spec in specs {
                cx.subscribe(&spec.input, move |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Submit {
                        this.apply(kind, cx);
                    }
                })
                .detach();
            }
        }

        // E5: Enter в полях подсписков.
        let sub_groups: [(ApplyKind, Vec<Option<Entity<TextInput>>>); 8] = [
            (
                ApplyKind::Goal,
                vec![
                    self.goal_kind.clone(),
                    self.goal_target.clone(),
                    self.goal_need.clone(),
                    self.goal_text.clone(),
                ],
            ),
            (
                ApplyKind::RewardItem,
                vec![self.reward_type.clone(), self.reward_qty.clone()],
            ),
            (
                ApplyKind::RecipeInput,
                vec![self.input_type.clone(), self.input_qty.clone()],
            ),
            (
                ApplyKind::DialogLine,
                vec![
                    self.line_id.clone(),
                    self.line_text.clone(),
                    self.line_quest.clone(),
                    self.line_every.clone(),
                    self.line_when.clone(),
                ],
            ),
            (
                ApplyKind::DialogReply,
                vec![
                    self.reply_id.clone(),
                    self.reply_text.clone(),
                    self.reply_next.clone(),
                    self.reply_action.clone(),
                    self.reply_when.clone(),
                ],
            ),
            (
                ApplyKind::Training,
                vec![
                    self.train_skill.clone(),
                    self.train_points.clone(),
                    self.train_gold.clone(),
                ],
            ),
            (
                ApplyKind::ShopCurrency,
                vec![self.curr_currency.clone(), self.curr_rate.clone()],
            ),
            (ApplyKind::ShopItem, vec![self.shop_item_id.clone()]),
        ];
        for (kind, inputs) in sub_groups {
            for input in inputs.into_iter().flatten() {
                cx.subscribe(&input, move |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Submit {
                        this.apply(kind, cx);
                    }
                })
                .detach();
            }
        }

        // E6.2: Enter в полях структурных зон предмета.
        let item_submit_groups: [(ApplyKind, Vec<Option<Entity<TextInput>>>); 8] = [
            (
                ApplyKind::Item,
                vec![
                    self.item_id.clone(),
                    self.item_name.clone(),
                    self.item_gold.clone(),
                    self.item_qty.clone(),
                    self.item_upgrade.clone(),
                    self.item_sockets.clone(),
                    self.item_rune_sockets.clone(),
                    self.item_hands.clone(),
                    self.item_cooldown.clone(),
                ],
            ),
            (
                ApplyKind::ItemStats,
                vec![
                    self.stat_dmg.clone(),
                    self.stat_armor.clone(),
                    self.stat_magic.clone(),
                    self.stat_heal.clone(),
                    self.stat_quality.clone(),
                    self.stat_damage.clone(),
                    self.stat_chance.clone(),
                    self.stat_charges.clone(),
                    self.stat_max.clone(),
                    self.stat_cost.clone(),
                    self.stat_glyph.clone(),
                    self.stat_spell.clone(),
                    self.stat_value.clone(),
                ],
            ),
            (ApplyKind::ItemParam, vec![self.param_value.clone()]),
            (
                ApplyKind::ItemBuff,
                vec![
                    self.buff_value.clone(),
                    self.buff_duration.clone(),
                    self.buff_stacks.clone(),
                ],
            ),
            (
                ApplyKind::ItemBuff2,
                vec![
                    self.buff2_value.clone(),
                    self.buff2_duration.clone(),
                    self.buff2_stacks.clone(),
                ],
            ),
            (
                ApplyKind::ItemEffect,
                vec![
                    self.effect_value.clone(),
                    self.effect_duration.clone(),
                    self.effect_period.clone(),
                ],
            ),
            (ApplyKind::ItemBonus, vec![self.bonus_value.clone()]),
            (
                ApplyKind::ItemRequires,
                vec![
                    self.req_str.clone(),
                    self.req_dex.clone(),
                    self.req_vit.clone(),
                    self.req_int.clone(),
                    self.req_level.clone(),
                ],
            ),
        ];
        for (kind, inputs) in item_submit_groups {
            for input in inputs.into_iter().flatten() {
                cx.subscribe(&input, move |this, _, event: &InputEvent, cx| {
                    if *event == InputEvent::Submit {
                        this.apply(kind, cx);
                    }
                })
                .detach();
            }
        }

        // E5: поиск в пикере-ссылке.
        cx.subscribe(&picker_search, |this, field, event: &InputEvent, cx| {
            if *event == InputEvent::Changed {
                let text = field.read(cx).text().to_string();
                if let Some(picker) = this.picker.as_mut() {
                    picker.filter = text;
                }
                cx.notify();
            }
        })
        .detach();
    }

    fn field_text(&self, slot: &Option<Entity<TextInput>>, cx: &Context<'_, Self>) -> String {
        slot.as_ref()
            .map(|entity| entity.read(cx).text().to_string())
            .unwrap_or_default()
    }

    fn put_text(
        slot: &Option<Entity<TextInput>>,
        text: impl Into<String>,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(field) = slot {
            let text = text.into();
            field.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    // ─── выбор ───────────────────────────────────────────────────────────

    fn select_item(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_item = Some(index);
        self.selected_param = None;
        self.selected_bonus = None;
        self.refresh_item_fields(cx);
        if let Some(item) = self
            .project
            .as_ref()
            .and_then(|p| p.content().items.get(index))
        {
            self.status = format!("выбран предмет {}", item.id);
        }
        cx.notify();
    }

    /// Номер отфильтрованной строки локации (для scroll_to_item).
    fn location_row_of(&self, index: usize) -> Option<usize> {
        let filter = self.filter.trim().to_lowercase();
        let project = self.project.as_ref()?;
        let mut row = 0;
        for (ix, location) in project.content().locations.iter().enumerate() {
            if !filter.is_empty()
                && !location.id.to_lowercase().contains(&filter)
                && !location.name.to_lowercase().contains(&filter)
            {
                continue;
            }
            if ix == index {
                return Some(row);
            }
            row += 1;
        }
        None
    }

    fn select_location(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_location = Some(index);
        self.selected_exit = None;
        self.selected_entity = None;
        self.refresh_location_fields(cx);
        Self::put_text(&self.exit_label, "", cx);
        Self::put_text(&self.exit_dir, "", cx);
        Self::put_text(&self.exit_to, "", cx);
        Self::put_text(&self.ent_kind, "", cx);
        Self::put_text(&self.ent_name, "", cx);
        Self::put_text(&self.ent_extra, "", cx);
        if let Some(location) = self
            .project
            .as_ref()
            .and_then(|p| p.content().locations.get(index))
        {
            self.status = format!(
                "выбрана локация {} (выходов {}, сущностей {})",
                location.id,
                location.exits.len(),
                location.entities.len()
            );
        }
        // Виртуализованный список: показать выбранную строку.
        if let Some(row) = self.location_row_of(index) {
            self.locations_scroll
                .scroll_to_item(row, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn select_exit(&mut self, exit_index: usize, cx: &mut Context<'_, Self>) {
        self.selected_exit = Some(exit_index);
        self.selected_entity = None;
        self.refresh_exit_fields(cx);
        cx.notify();
    }

    fn select_entity(&mut self, entity_index: usize, cx: &mut Context<'_, Self>) {
        self.selected_entity = Some(entity_index);
        self.selected_exit = None;
        self.refresh_entity_fields(cx);
        cx.notify();
    }

    // ─── перечитывание полей ─────────────────────────────────────────────

    fn refresh_item_fields(&mut self, cx: &mut Context<'_, Self>) {
        self.refresh_item_zone_fields(cx);
    }

    /// Перечитать структурные зоны предмета (основное, статы, баффы, …).
    fn refresh_item_zone_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let Some(item) = self
            .project
            .as_ref()
            .and_then(|project| project.content().items.get(index))
            .cloned()
        else {
            self.selected_item = None;
            return;
        };
        Self::put_text(&self.item_id, item.id.clone(), cx);
        Self::put_text(&self.item_name, item.name.clone(), cx);
        Self::put_text(&self.item_gold, item.gold_value.to_string(), cx);
        Self::put_text(
            &self.item_qty,
            item.qty.map(|qty| qty.to_string()).unwrap_or_default(),
            cx,
        );
        Self::put_text(&self.item_upgrade, item.upgrade.to_string(), cx);
        Self::put_text(&self.item_sockets, item.sockets.to_string(), cx);
        Self::put_text(
            &self.item_rune_sockets,
            item.rune_sockets
                .map(|value| value.to_string())
                .unwrap_or_default(),
            cx,
        );
        Self::put_text(
            &self.item_hands,
            item.hands
                .map(|value| value.to_string())
                .unwrap_or_default(),
            cx,
        );
        Self::put_text(
            &self.item_cooldown,
            item.cooldown_s.map(format_number).unwrap_or_default(),
            cx,
        );

        let stat = |key: &str| item.stats.get(key).map(text_value).unwrap_or_default();
        Self::put_text(&self.stat_dmg, stat("dmg"), cx);
        Self::put_text(&self.stat_armor, stat("armor"), cx);
        Self::put_text(&self.stat_magic, stat("magic"), cx);
        Self::put_text(&self.stat_heal, stat("heal"), cx);
        Self::put_text(&self.stat_quality, stat("quality"), cx);
        Self::put_text(&self.stat_damage, stat("damage"), cx);
        Self::put_text(&self.stat_chance, stat("chance"), cx);
        Self::put_text(&self.stat_charges, stat("charges"), cx);
        Self::put_text(&self.stat_max, stat("max"), cx);
        Self::put_text(&self.stat_cost, stat("cost"), cx);
        Self::put_text(&self.stat_glyph, stat("glyph"), cx);
        Self::put_text(&self.stat_spell, stat("spell"), cx);
        Self::put_text(&self.stat_value, stat("value"), cx);

        let param_text = self
            .selected_param
            .as_ref()
            .and_then(|param| item.stats.get("param").and_then(|params| params.get(param)))
            .map(text_value)
            .unwrap_or_default();
        Self::put_text(&self.param_value, param_text, cx);

        let buff = |slot: &str, key: &str| {
            item.stats
                .get(slot)
                .and_then(|buff| buff.get(key))
                .map(text_value)
                .unwrap_or_default()
        };
        Self::put_text(&self.buff_value, buff("buff", "value"), cx);
        Self::put_text(&self.buff_duration, buff("buff", "duration_s"), cx);
        Self::put_text(&self.buff_stacks, buff("buff", "max_stacks"), cx);
        Self::put_text(&self.buff2_value, buff("buff2", "value"), cx);
        Self::put_text(&self.buff2_duration, buff("buff2", "duration_s"), cx);
        Self::put_text(&self.buff2_stacks, buff("buff2", "max_stacks"), cx);

        let effect = |key: &str| {
            item.stats
                .get("effect")
                .and_then(|effect| effect.get(key))
                .map(text_value)
                .unwrap_or_default()
        };
        Self::put_text(&self.effect_value, effect("value"), cx);
        Self::put_text(&self.effect_duration, effect("duration_s"), cx);
        Self::put_text(&self.effect_period, effect("period_s"), cx);

        let bonus_text = self
            .selected_bonus
            .and_then(|bonus| item.bonuses.get(bonus))
            .map(|bonus| bonus.value.to_string())
            .unwrap_or_default();
        Self::put_text(&self.bonus_value, bonus_text, cx);

        let attr = |name: &str| {
            item.requires
                .as_ref()
                .and_then(|requires| requires.attr.get(name))
                .map(|value| value.to_string())
                .unwrap_or_default()
        };
        Self::put_text(&self.req_str, attr("str"), cx);
        Self::put_text(&self.req_dex, attr("dex"), cx);
        Self::put_text(&self.req_vit, attr("vit"), cx);
        Self::put_text(&self.req_int, attr("int"), cx);
        Self::put_text(
            &self.req_level,
            item.requires
                .as_ref()
                .and_then(|requires| requires.level)
                .map(|level| level.to_string())
                .unwrap_or_default(),
            cx,
        );
    }

    fn refresh_location_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_location else {
            return;
        };
        let Some(location) = self
            .project
            .as_ref()
            .and_then(|p| p.content().locations.get(index))
            .cloned()
        else {
            self.selected_location = None;
            return;
        };
        Self::put_text(&self.loc_name, location.name, cx);
        Self::put_text(&self.loc_text, location.text, cx);
        Self::put_text(&self.loc_flags, location.flags.join(", "), cx);
        Self::put_text(
            &self.loc_spawn,
            if location.spawn { "true" } else { "false" },
            cx,
        );
        if let Some(exit) = self.selected_exit {
            if exit >= location.exits.len() {
                self.selected_exit = None;
            }
        }
        if let Some(entity) = self.selected_entity {
            if entity >= location.entities.len() {
                self.selected_entity = None;
            }
        }
        self.refresh_exit_fields(cx);
        self.refresh_entity_fields(cx);
    }

    fn refresh_exit_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((location_index, exit_index)) = self.selected_location.zip(self.selected_exit)
        else {
            return;
        };
        let Some(exit) = self
            .project
            .as_ref()
            .and_then(|p| p.content().locations.get(location_index))
            .and_then(|location| location.exits.get(exit_index))
            .cloned()
        else {
            self.selected_exit = None;
            return;
        };
        Self::put_text(&self.exit_label, exit.label, cx);
        Self::put_text(&self.exit_dir, exit.dir.clone().unwrap_or_default(), cx);
        Self::put_text(&self.exit_to, exit.to, cx);
    }

    fn refresh_entity_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((location_index, entity_index)) = self.selected_location.zip(self.selected_entity)
        else {
            return;
        };
        let Some(entity) = self
            .project
            .as_ref()
            .and_then(|p| p.content().locations.get(location_index))
            .and_then(|location| location.entities.get(entity_index))
            .cloned()
        else {
            self.selected_entity = None;
            return;
        };
        Self::put_text(&self.ent_kind, kind_str(entity.kind), cx);
        Self::put_text(&self.ent_name, entity.name, cx);
        Self::put_text(
            &self.ent_extra,
            serde_json::to_string_pretty(&entity.extra).unwrap_or_default(),
            cx,
        );
    }

    fn refresh_all(&mut self, cx: &mut Context<'_, Self>) {
        match self.section {
            Section::Items => self.refresh_item_fields(cx),
            Section::Locations => self.refresh_location_fields(cx),
            // У карты нет полей формы; выбор узла уже в состоянии.
            Section::Map => {}
            Section::Quests => self.refresh_quest_fields(cx),
            Section::Recipes => self.refresh_recipe_fields(cx),
            Section::Skills => self.refresh_skill_fields(cx),
            Section::Professions => self.refresh_profession_fields(cx),
            Section::Dialogs => self.refresh_dialog_fields(cx),
            Section::Shops => self.refresh_shop_fields(cx),
            // У справки нет полей формы.
            Section::Help => {}
        }
    }

    // ─── применение форм ─────────────────────────────────────────────────

    fn apply(&mut self, kind: ApplyKind, cx: &mut Context<'_, Self>) {
        match kind {
            ApplyKind::Item => self.apply_item(cx),
            ApplyKind::ItemStats => self.apply_item_stats(cx),
            ApplyKind::ItemParam => self.apply_item_param(cx),
            ApplyKind::ItemBuff => self.apply_item_buff("buff", cx),
            ApplyKind::ItemBuff2 => self.apply_item_buff("buff2", cx),
            ApplyKind::ItemEffect => self.apply_item_effect(cx),
            ApplyKind::ItemBonus => self.apply_item_bonus(cx),
            ApplyKind::ItemRequires => self.apply_item_requires(cx),
            ApplyKind::Location => self.apply_location(cx),
            ApplyKind::Exit => self.apply_exit(cx),
            ApplyKind::Entity => self.apply_entity(cx),
            ApplyKind::Quests => self.apply_group(FieldGroup::Quests, cx),
            ApplyKind::Recipes => self.apply_group(FieldGroup::Recipes, cx),
            ApplyKind::Skills => self.apply_group(FieldGroup::Skills, cx),
            ApplyKind::Professions => self.apply_group(FieldGroup::Professions, cx),
            ApplyKind::Goal => self.apply_goal(cx),
            ApplyKind::RewardItem => self.apply_reward_item(cx),
            ApplyKind::RecipeInput => self.apply_recipe_input(cx),
            ApplyKind::Dialogs => self.apply_group(FieldGroup::Dialogs, cx),
            ApplyKind::Shops => self.apply_group(FieldGroup::Shops, cx),
            ApplyKind::DialogLine => self.apply_dialog_line(cx),
            ApplyKind::DialogReply => self.apply_dialog_reply(cx),
            ApplyKind::Training => self.apply_training(cx),
            ApplyKind::ShopCurrency => self.apply_currency(cx),
            ApplyKind::ShopItem => self.apply_shop_item(cx),
        }
    }

    fn apply_item(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            self.status = "не выбран предмет".into();
            cx.notify();
            return;
        };
        let id = self.field_text(&self.item_id, cx).trim().to_string();
        if id.is_empty() {
            self.status = "id предмета не может быть пустым".into();
            cx.notify();
            return;
        }
        let name = self.field_text(&self.item_name, cx).trim().to_string();
        if name.is_empty() {
            self.status = "имя предмета не может быть пустым".into();
            cx.notify();
            return;
        }
        let parse_int = |label: &str, text: String| -> Result<Option<i64>, String> {
            let text = text.trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            text.parse::<i64>()
                .map(Some)
                .map_err(|_| format!("{label}: нужно целое число (получено «{text}»)"))
        };
        let parse_float = |label: &str, text: String| -> Result<Option<f64>, String> {
            let text = text.trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            text.parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(Some)
                .ok_or_else(|| format!("{label}: нужно число (получено «{text}»)"))
        };
        let mut fields: Vec<(&str, Option<Value>)> = vec![
            ("id", Some(Value::from(id))),
            ("name", Some(Value::from(name))),
        ];
        for (key, label, text) in [
            ("gold_value", "Цена", self.field_text(&self.item_gold, cx)),
            ("qty", "Количество", self.field_text(&self.item_qty, cx)),
            (
                "upgrade",
                "Предел заточки",
                self.field_text(&self.item_upgrade, cx),
            ),
            (
                "sockets",
                "Сокетов",
                self.field_text(&self.item_sockets, cx),
            ),
            (
                "rune_sockets",
                "Из них под руны",
                self.field_text(&self.item_rune_sockets, cx),
            ),
            ("hands", "Рук", self.field_text(&self.item_hands, cx)),
        ] {
            match parse_int(label, text) {
                Ok(value) => fields.push((key, value.map(Value::from))),
                Err(err) => {
                    self.status = err;
                    cx.notify();
                    return;
                }
            }
        }
        match parse_float("Кулдаун", self.field_text(&self.item_cooldown, cx)) {
            Ok(value) => fields.push(("cooldown_s", value.map(Value::from))),
            Err(err) => {
                self.status = err;
                cx.notify();
                return;
            }
        }
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let mut error = None;
        for (key, value) in fields {
            if let Err(err) = project.set_item_field(index, key, value) {
                error = Some(err.to_string());
                break;
            }
        }
        self.status = error.unwrap_or_else(|| "основное: применено (Ctrl+S — сохранить)".into());
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    /// Применить статы `stats` (текстовые/числовые поля; Select — сразу).
    fn apply_item_stats(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            self.status = "не выбран предмет".into();
            cx.notify();
            return;
        };
        let mut edits: Vec<(&str, Option<Value>)> = Vec::new();
        for (key, input) in [
            ("dmg", &self.stat_dmg),
            ("glyph", &self.stat_glyph),
            ("spell", &self.stat_spell),
        ] {
            let text = self.field_text(input, cx).trim().to_string();
            edits.push((
                key,
                if text.is_empty() {
                    None
                } else {
                    Some(Value::from(text))
                },
            ));
        }
        for (key, label, input) in [
            ("armor", "Броня", &self.stat_armor),
            ("magic", "Магия", &self.stat_magic),
            ("heal", "Лечение", &self.stat_heal),
            ("quality", "Качество", &self.stat_quality),
            ("damage", "Урон заклинания", &self.stat_damage),
            ("charges", "Заряды", &self.stat_charges),
            ("max", "Макс. зарядов", &self.stat_max),
            ("cost", "Стоимость каста", &self.stat_cost),
            ("value", "Значение", &self.stat_value),
        ] {
            let text = self.field_text(input, cx).trim().to_string();
            if text.is_empty() {
                edits.push((key, None));
                continue;
            }
            match text.parse::<i64>() {
                Ok(value) => edits.push((key, Some(Value::from(value)))),
                Err(_) => {
                    self.status = format!("{label}: нужно целое число (получено «{text}»)");
                    cx.notify();
                    return;
                }
            }
        }
        let chance = self.field_text(&self.stat_chance, cx).trim().to_string();
        if chance.is_empty() {
            edits.push(("chance", None));
        } else {
            match chance.parse::<f64>() {
                Ok(value) if value.is_finite() => edits.push(("chance", Some(number_value(value)))),
                _ => {
                    self.status = format!("Шанс: нужно число (получено «{chance}»)");
                    cx.notify();
                    return;
                }
            }
        }
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let mut error = None;
        for (key, value) in edits {
            if let Err(err) = project.set_item_stat(index, key, value) {
                error = Some(err.to_string());
                break;
            }
        }
        self.status = error.unwrap_or_else(|| "статы: применены".into());
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    /// Применить значение выбранного параметра `stats.param.<id>`.
    fn apply_item_param(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let Some(param) = self.selected_param.clone() else {
            self.status = "не выбран параметр".into();
            cx.notify();
            return;
        };
        let text = self.field_text(&self.param_value, cx).trim().to_string();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let result = if text.is_empty() {
            project.set_item_param(index, &param, None)
        } else {
            match text.parse::<f64>().ok().filter(|value| value.is_finite()) {
                Some(number) => {
                    let current = project
                        .content()
                        .items
                        .get(index)
                        .and_then(|item| item.stats.get("param"))
                        .and_then(|params| params.get(&param))
                        .cloned();
                    let pct = matches!(&current, Some(Value::Object(map))
                        if map.get("pct").and_then(Value::as_bool).unwrap_or(false));
                    let value = if pct {
                        serde_json::json!({"value": number_value(number), "pct": true})
                    } else {
                        number_value(number)
                    };
                    project.set_item_param(index, &param, Some(value))
                }
                None => {
                    self.status = format!("параметр {param}: нужно число (получено «{text}»)");
                    cx.notify();
                    return;
                }
            }
        };
        self.item_apply_result(result, "параметр: применён", cx);
    }

    /// Применить автобафф `stats.buff`/`stats.buff2`.
    fn apply_item_buff(&mut self, slot: &str, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let (value_input, duration_input, stacks_input) = if slot == "buff" {
            (&self.buff_value, &self.buff_duration, &self.buff_stacks)
        } else {
            (&self.buff2_value, &self.buff2_duration, &self.buff2_stacks)
        };
        let value_text = self.field_text(value_input, cx).trim().to_string();
        let duration_text = self.field_text(duration_input, cx).trim().to_string();
        let stacks_text = self.field_text(stacks_input, cx).trim().to_string();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if value_text.is_empty() && duration_text.is_empty() && stacks_text.is_empty() {
            let result = project.remove_item_buff(index, slot);
            self.item_apply_result(result, "автобафф удалён", cx);
            return;
        }
        let value = match value_text.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(number_value(value)),
            _ if value_text.is_empty() => None,
            _ => {
                self.status = format!("бафф {slot}: значение — число");
                cx.notify();
                return;
            }
        };
        let duration = match duration_text.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(number_value(value)),
            _ if duration_text.is_empty() => None,
            _ => {
                self.status = format!("бафф {slot}: длительность — число");
                cx.notify();
                return;
            }
        };
        let stacks = match stacks_text.parse::<i64>() {
            Ok(value) => Some(Value::from(value)),
            Err(_) if stacks_text.is_empty() => None,
            Err(_) => {
                self.status = format!("бафф {slot}: стаки — целое число");
                cx.notify();
                return;
            }
        };
        let mut error = None;
        for (key, value) in [
            ("value", value),
            ("duration_s", duration),
            ("max_stacks", stacks),
        ] {
            if let Err(err) = project.set_item_buff_field(index, slot, key, value) {
                error = Some(err.to_string());
                break;
            }
        }
        self.status = error.unwrap_or_else(|| "автобафф: применён".into());
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    /// Применить эффект `stats.effect` (вид выбирается Select).
    fn apply_item_effect(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let parse_optional = |label: &str, text: String| -> Result<Option<Value>, String> {
            let text = text.trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            text.parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| Some(number_value(value)))
                .ok_or_else(|| format!("{label}: нужно число (получено «{text}»)"))
        };
        let value = match parse_optional("Урон эффекта", self.field_text(&self.effect_value, cx))
        {
            Ok(value) => value,
            Err(err) => {
                self.status = err;
                cx.notify();
                return;
            }
        };
        let duration =
            match parse_optional("Длительность", self.field_text(&self.effect_duration, cx))
            {
                Ok(value) => value,
                Err(err) => {
                    self.status = err;
                    cx.notify();
                    return;
                }
            };
        let period = match parse_optional("Период", self.field_text(&self.effect_period, cx))
        {
            Ok(value) => value,
            Err(err) => {
                self.status = err;
                cx.notify();
                return;
            }
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let mut error = None;
        for (key, value) in [
            ("value", value),
            ("duration_s", duration),
            ("period_s", period),
        ] {
            let Some(value) = value else {
                if let Err(err) = project.set_item_effect_field(index, key, None) {
                    error = Some(err.to_string());
                }
                continue;
            };
            if let Err(err) = project.set_item_effect_field(index, key, Some(value)) {
                error = Some(err.to_string());
                break;
            }
        }
        self.status = error.unwrap_or_else(|| "эффект: применён".into());
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    /// Применить значение выбранного бонуса (ADR-010).
    fn apply_item_bonus(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let Some(bonus_index) = self.selected_bonus else {
            self.status = "не выбран бонус".into();
            cx.notify();
            return;
        };
        let text = self.field_text(&self.bonus_value, cx).trim().to_string();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match text.parse::<i64>() {
            Ok(value) => {
                let result = project.set_item_bonus_field(
                    index,
                    bonus_index,
                    "value",
                    Some(Value::from(value)),
                );
                self.item_apply_result(result, "бонус: применён", cx);
            }
            Err(_) => {
                self.status = format!("бонус: значение — целое число (получено «{text}»)");
                cx.notify();
            }
        }
    }

    /// Применить требования `requires` (атрибуты и уровень).
    fn apply_item_requires(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let mut attr_values: Vec<(&str, &str, &Option<Entity<TextInput>>)> = vec![
            ("str", "str", &self.req_str),
            ("dex", "dex", &self.req_dex),
            ("vit", "vit", &self.req_vit),
            ("int", "int", &self.req_int),
        ];
        let mut values: Vec<(&str, Option<Value>)> = Vec::new();
        for (attr, label, input) in attr_values.drain(..) {
            let text = self.field_text(input, cx).trim().to_string();
            if text.is_empty() {
                values.push((attr, None));
                continue;
            }
            match text.parse::<i64>() {
                Ok(value) => values.push((attr, Some(Value::from(value)))),
                Err(_) => {
                    self.status = format!("requires.{label}: нужно целое число");
                    cx.notify();
                    return;
                }
            }
        }
        let level_text = self.field_text(&self.req_level, cx).trim().to_string();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let mut error = None;
        for (attr, value) in values {
            if let Err(err) = project.set_item_requires_attr(index, attr, value) {
                error = Some(err.to_string());
                break;
            }
        }
        if error.is_none() {
            let level = match level_text.parse::<i64>() {
                Ok(value) => Some(Value::from(value)),
                Err(_) if level_text.is_empty() => None,
                Err(_) => {
                    self.status = "requires.level: нужно целое число".into();
                    cx.notify();
                    return;
                }
            };
            if let Err(err) = project.set_item_requires_field(index, "level", level) {
                error = Some(err.to_string());
            }
        }
        self.status = error.unwrap_or_else(|| "требования: применены".into());
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    fn item_apply_result(
        &mut self,
        result: Result<(), crate::project::ProjectError>,
        ok: &str,
        cx: &mut Context<'_, Self>,
    ) {
        self.status = match result {
            Ok(()) => ok.to_string(),
            Err(err) => err.to_string(),
        };
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    fn apply_location(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_location else {
            self.status = "не выбрана локация".into();
            cx.notify();
            return;
        };
        let name = self.field_text(&self.loc_name, cx);
        let text = self.field_text(&self.loc_text, cx);
        let flags_text = self.field_text(&self.loc_flags, cx);
        let spawn_text = self.field_text(&self.loc_spawn, cx);
        let flags: Value = Value::Array(
            flags_text
                .split(',')
                .map(str::trim)
                .filter(|flag| !flag.is_empty())
                .map(Value::from)
                .collect(),
        );
        let spawn = match spawn_text.trim().to_lowercase().as_str() {
            "true" | "1" | "да" | "yes" => true,
            "false" | "0" | "нет" | "no" | "" => false,
            _ => {
                self.status = "spawn: ожидается true/false".into();
                cx.notify();
                return;
            }
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.set_location_fields(
            index,
            vec![
                ("name", Value::from(name)),
                ("text", Value::from(text)),
                ("flags", flags),
                ("spawn", Value::from(spawn)),
            ],
        ) {
            Ok(()) => self.status = "локация применена (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn apply_exit(&mut self, cx: &mut Context<'_, Self>) {
        let Some((location_index, exit_index)) = self.selected_location.zip(self.selected_exit)
        else {
            self.status = "не выбран выход".into();
            cx.notify();
            return;
        };
        let label = self.field_text(&self.exit_label, cx);
        let dir = self.field_text(&self.exit_dir, cx);
        let to = self.field_text(&self.exit_to, cx);
        let dir_value = if dir.trim().is_empty() {
            Value::Null
        } else {
            Value::from(dir.trim())
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_exit(
            location_index,
            exit_index,
            vec![
                ("label", Value::from(label)),
                ("dir", dir_value),
                ("to", Value::from(to.trim())),
            ],
        ) {
            Ok(()) => self.status = "выход применён (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn apply_entity(&mut self, cx: &mut Context<'_, Self>) {
        let Some((location_index, entity_index)) = self.selected_location.zip(self.selected_entity)
        else {
            self.status = "не выбрана сущность".into();
            cx.notify();
            return;
        };
        let kind = self.field_text(&self.ent_kind, cx);
        let name = self.field_text(&self.ent_name, cx);
        let extra_text = self.field_text(&self.ent_extra, cx);
        let extra: Value = match serde_json::from_str(if extra_text.trim().is_empty() {
            "{}"
        } else {
            &extra_text
        }) {
            Ok(value @ Value::Object(_)) => value,
            _ => {
                self.status = "extra: ожидается JSON-объект".into();
                cx.notify();
                return;
            }
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let Some(current) = project
            .content()
            .locations
            .get(location_index)
            .and_then(|location| location.entities.get(entity_index))
            .cloned()
        else {
            self.status = "сущность исчезла — обновите выбор".into();
            cx.notify();
            return;
        };
        // Сохраняем все известные поля сущности: берём сериализованный
        // объект, меняем kind/name и мержим extra (удалённые ключи extra
        // убираем — иначе форма не сможет очистить поле).
        let mut object = match serde_json::to_value(&current) {
            Ok(Value::Object(object)) => object,
            _ => {
                self.status = "не удалось сериализовать сущность".into();
                cx.notify();
                return;
            }
        };
        object.insert("kind".into(), Value::from(kind.trim()));
        object.insert("name".into(), Value::from(name));
        if let Value::Object(extra) = &extra {
            for key in current.extra.keys() {
                if !extra.contains_key(key) {
                    object.remove(key);
                }
            }
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        match project.set_entity_object(location_index, entity_index, Value::Object(object)) {
            Ok(()) => self.status = "сущность применена (Ctrl+S — сохранить)".into(),
            Err(err) => {
                self.status = err.to_string();
                // Некорректный kind — типизация откатит правку; вернуть форму.
                self.refresh_entity_fields(cx);
            }
        }
        cx.notify();
    }

    // ─── добавление/удаление ─────────────────────────────────────────────

    fn add_exit(&mut self, cx: &mut Context<'_, Self>) {
        let Some(location_index) = self.selected_location else {
            return;
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.add_exit(location_index) {
            Ok(index) => {
                self.selected_exit = Some(index);
                self.selected_entity = None;
                let Some(project) = self.project.as_ref() else {
                    return;
                };
                let count = project.content().locations[location_index].exits.len();
                self.status = format!("выход добавлен (всего {count}) — задайте to");
                self.refresh_exit_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_exit(
        &mut self,
        location_index: usize,
        exit_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.remove_exit(location_index, exit_index) {
            Ok(()) => {
                if self.selected_exit == Some(exit_index) {
                    self.selected_exit = None;
                } else if let Some(selected) = self.selected_exit {
                    if selected > exit_index {
                        self.selected_exit = Some(selected - 1);
                    }
                }
                self.status = "выход удалён".into();
                self.refresh_location_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_entity(&mut self, kind: &'static str, cx: &mut Context<'_, Self>) {
        let Some(location_index) = self.selected_location else {
            return;
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.add_entity(location_index, kind) {
            Ok(index) => {
                self.selected_entity = Some(index);
                self.selected_exit = None;
                self.status = format!("сущность добавлена ({kind}) — задайте поля");
                self.refresh_entity_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_entity(
        &mut self,
        location_index: usize,
        entity_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.remove_entity(location_index, entity_index) {
            Ok(()) => {
                if self.selected_entity == Some(entity_index) {
                    self.selected_entity = None;
                } else if let Some(selected) = self.selected_entity {
                    if selected > entity_index {
                        self.selected_entity = Some(selected - 1);
                    }
                }
                self.status = "сущность удалена".into();
                self.refresh_location_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    // ─── сохранение/история ──────────────────────────────────────────────

    fn save(&mut self, cx: &mut Context<'_, Self>) {
        let layout_status = match self.save_layout() {
            Ok(true) => "раскладка сохранена · ".to_string(),
            Ok(false) => String::new(),
            Err(err) => format!("раскладка: {err} · "),
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        self.status = format!(
            "{layout_status}{}",
            match project.save() {
                Ok(0) => "нечего сохранять".to_string(),
                Ok(written) => format!(
                    "сохранено файлов: {written} · версия {}",
                    short(project.version())
                ),
                Err(err) => err.to_string(),
            }
        );
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<'_, Self>) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if project.undo() {
            self.status = "undo".into();
            self.refresh_all(cx);
        } else {
            self.status = "нечего отменять".into();
        }
        cx.notify();
    }

    fn redo(&mut self, cx: &mut Context<'_, Self>) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if project.redo() {
            self.status = "redo".into();
            self.refresh_all(cx);
        } else {
            self.status = "нечего повторять".into();
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.reload() {
            Ok(()) => {
                self.status = "перечитано с диска (история сброшена)".into();
                self.refresh_all(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    // ─── E5: разделы записей и их формы ──────────────────────────────────

    /// Переключить раздел левой рейки.
    fn switch_section(&mut self, section: Section, cx: &mut Context<'_, Self>) {
        self.section = section;
        self.filter.clear();
        Self::put_text(&self.search, "", cx);
        match section {
            Section::Map => {
                self.ensure_map();
                if !self.map_centered {
                    self.center_map();
                }
                self.status =
                    "карта: колесо — зум, drag — панорама, ←↑→↓ — сдвиг узла, Ctrl+S — сохранить"
                        .into();
            }
            Section::Items => self.status = "раздел: предметы".into(),
            Section::Locations => self.status = "раздел: локации".into(),
            Section::Quests => self.status = "раздел: квесты".into(),
            Section::Recipes => self.status = "раздел: рецепты".into(),
            Section::Skills => self.status = "раздел: навыки".into(),
            Section::Professions => self.status = "раздел: профессии".into(),
            Section::Dialogs => self.status = "раздел: диалоги".into(),
            Section::Shops => self.status = "раздел: магазины".into(),
            Section::Help => self.status = "справка: горячие клавиши и поток работы".into(),
        }
        cx.notify();
    }

    /// Группа записей раздела (для разделов записей).
    fn section_group(section: Section) -> Option<FieldGroup> {
        match section {
            Section::Dialogs => Some(FieldGroup::Dialogs),
            Section::Shops => Some(FieldGroup::Shops),
            Section::Quests => Some(FieldGroup::Quests),
            Section::Recipes => Some(FieldGroup::Recipes),
            Section::Skills => Some(FieldGroup::Skills),
            Section::Professions => Some(FieldGroup::Professions),
            _ => None,
        }
    }

    fn group_specs(&self, group: FieldGroup) -> &[FieldSpec] {
        match group {
            FieldGroup::Dialogs => &self.dialog_fields,
            FieldGroup::Shops => &self.shop_fields,
            FieldGroup::Quests => &self.quest_fields,
            FieldGroup::Recipes => &self.recipe_fields,
            FieldGroup::Skills => &self.skill_fields,
            FieldGroup::Professions => &self.profession_fields,
        }
    }

    fn group_selected(&self, group: FieldGroup) -> Option<usize> {
        match group {
            FieldGroup::Dialogs => self.selected_dialog,
            FieldGroup::Shops => self.selected_shop,
            FieldGroup::Quests => self.selected_quest,
            FieldGroup::Recipes => self.selected_recipe,
            FieldGroup::Skills => self.selected_skill,
            FieldGroup::Professions => self.selected_profession,
        }
    }

    fn set_group_selected(&mut self, group: FieldGroup, value: Option<usize>) {
        match group {
            FieldGroup::Dialogs => {
                self.selected_dialog = value;
                self.selected_dialog_line = None;
                self.selected_dialog_reply = None;
                self.selected_training = None;
            }
            FieldGroup::Shops => {
                self.selected_shop = value;
                self.selected_currency = None;
                self.selected_shop_item = None;
            }
            FieldGroup::Quests => {
                self.selected_quest = value;
                self.selected_goal = None;
                self.selected_reward_item = None;
            }
            FieldGroup::Recipes => {
                self.selected_recipe = value;
                self.selected_recipe_input = None;
            }
            FieldGroup::Skills => self.selected_skill = value,
            FieldGroup::Professions => self.selected_profession = value,
        }
    }

    fn group_len(&self, group: FieldGroup) -> usize {
        let Some(project) = self.project.as_ref() else {
            return 0;
        };
        let content = project.content();
        match group {
            FieldGroup::Dialogs => project.dialogs().npcs.len(),
            FieldGroup::Shops => project.shops().npcs.len(),
            FieldGroup::Quests => content.quests.len(),
            FieldGroup::Recipes => content.recipes.len(),
            FieldGroup::Skills => content.skills.len(),
            FieldGroup::Professions => content.professions.len(),
        }
    }

    fn select_record(&mut self, section: Section, index: usize, cx: &mut Context<'_, Self>) {
        match section {
            Section::Quests => {
                self.set_group_selected(FieldGroup::Quests, Some(index));
                self.status = format!("выбран квест {}", self.record_id(FieldGroup::Quests, index));
            }
            Section::Recipes => {
                self.set_group_selected(FieldGroup::Recipes, Some(index));
                self.status = format!(
                    "выбран рецепт {}",
                    self.record_id(FieldGroup::Recipes, index)
                );
            }
            Section::Skills => {
                self.set_group_selected(FieldGroup::Skills, Some(index));
                self.status = format!("выбран навык {}", self.record_id(FieldGroup::Skills, index));
            }
            Section::Professions => {
                self.set_group_selected(FieldGroup::Professions, Some(index));
                self.status = format!(
                    "выбрана профессия {}",
                    self.record_id(FieldGroup::Professions, index)
                );
            }
            _ => return,
        }
        if let Some(group) = Self::section_group(section) {
            self.refresh_group(group, cx);
        }
        cx.notify();
    }

    /// id записи группы по индексу (для статуса/заголовков).
    fn record_id(&self, group: FieldGroup, index: usize) -> String {
        let Some(project) = self.project.as_ref() else {
            return String::new();
        };
        let content = project.content();
        match group {
            FieldGroup::Dialogs => project.dialogs().npcs.get(index).map(|npc| npc.id.clone()),
            FieldGroup::Shops => project.shops().npcs.get(index).map(|npc| npc.id.clone()),
            FieldGroup::Quests => content.quests.get(index).map(|q| q.id.clone()),
            FieldGroup::Recipes => content.recipes.get(index).map(|r| r.id.clone()),
            FieldGroup::Skills => content.skills.get(index).map(|s| s.id.clone()),
            FieldGroup::Professions => content.professions.get(index).map(|p| p.id.clone()),
        }
        .unwrap_or_default()
    }

    fn refresh_group(&mut self, group: FieldGroup, cx: &mut Context<'_, Self>) {
        match group {
            FieldGroup::Dialogs => self.refresh_dialog_fields(cx),
            FieldGroup::Shops => self.refresh_shop_fields(cx),
            FieldGroup::Quests => self.refresh_quest_fields(cx),
            FieldGroup::Recipes => self.refresh_recipe_fields(cx),
            FieldGroup::Skills => self.refresh_skill_fields(cx),
            FieldGroup::Professions => self.refresh_profession_fields(cx),
        }
    }

    fn clear_group_fields(&mut self, group: FieldGroup, cx: &mut Context<'_, Self>) {
        for spec in self.group_specs(group).to_vec() {
            Self::put_text(&Some(spec.input), "", cx);
        }
        match group {
            FieldGroup::Dialogs => {
                for slot in [
                    &self.line_id,
                    &self.line_text,
                    &self.line_quest,
                    &self.line_every,
                    &self.line_when,
                    &self.reply_id,
                    &self.reply_text,
                    &self.reply_next,
                    &self.reply_action,
                    &self.reply_when,
                    &self.train_skill,
                    &self.train_points,
                    &self.train_gold,
                ] {
                    Self::put_text(slot, "", cx);
                }
            }
            FieldGroup::Shops => {
                for slot in [&self.curr_currency, &self.curr_rate, &self.shop_item_id] {
                    Self::put_text(slot, "", cx);
                }
            }
            FieldGroup::Quests => {
                for slot in [
                    &self.goal_kind,
                    &self.goal_target,
                    &self.goal_need,
                    &self.goal_text,
                ] {
                    Self::put_text(slot, "", cx);
                }
                for slot in [&self.reward_type, &self.reward_qty] {
                    Self::put_text(slot, "", cx);
                }
            }
            FieldGroup::Recipes => {
                for slot in [&self.input_type, &self.input_qty] {
                    Self::put_text(slot, "", cx);
                }
            }
            _ => {}
        }
    }

    /// Перечитать поля группы полей из raw JSON записи.
    fn refresh_specs(
        &mut self,
        file: ContentFile,
        index: usize,
        specs: &[FieldSpec],
        cx: &mut Context<'_, Self>,
    ) {
        let values: Vec<(FieldKind, Entity<TextInput>, Option<Value>)> = {
            let Some(project) = self.project.as_ref() else {
                return;
            };
            specs
                .iter()
                .map(|spec| {
                    let mut path = vec![PathSeg::key(file.root_key()), PathSeg::index(index)];
                    path.extend(spec.path.iter().map(|key| PathSeg::key(*key)));
                    (spec.kind, spec.input.clone(), project.value_at(file, &path))
                })
                .collect()
        };
        for (kind, input, value) in values {
            Self::put_text(&Some(input), value_text(kind, value.as_ref()), cx);
        }
    }

    fn refresh_quest_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_quest else {
            return;
        };
        if index >= self.group_len(FieldGroup::Quests) {
            self.selected_quest = None;
            return;
        }
        let specs = self.quest_fields.clone();
        self.refresh_specs(ContentFile::Quests, index, &specs, cx);
        let (goals, items) = self
            .project
            .as_ref()
            .map(|project| {
                let quest = &project.content().quests[index];
                (quest.goals.len(), quest.reward.items.len())
            })
            .unwrap_or((0, 0));
        if self.selected_goal.is_some_and(|goal| goal >= goals) {
            self.selected_goal = None;
        }
        if self.selected_reward_item.is_some_and(|item| item >= items) {
            self.selected_reward_item = None;
        }
        self.refresh_goal_fields(cx);
        self.refresh_reward_item_fields(cx);
    }

    fn refresh_recipe_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_recipe else {
            return;
        };
        if index >= self.group_len(FieldGroup::Recipes) {
            self.selected_recipe = None;
            return;
        }
        let specs = self.recipe_fields.clone();
        self.refresh_specs(ContentFile::Recipes, index, &specs, cx);
        let inputs = self
            .project
            .as_ref()
            .map(|project| project.content().recipes[index].inputs.len())
            .unwrap_or(0);
        if self
            .selected_recipe_input
            .is_some_and(|input| input >= inputs)
        {
            self.selected_recipe_input = None;
        }
        self.refresh_recipe_input_fields(cx);
    }

    fn refresh_skill_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_skill else {
            return;
        };
        if index >= self.group_len(FieldGroup::Skills) {
            self.selected_skill = None;
            return;
        }
        let specs = self.skill_fields.clone();
        self.refresh_specs(ContentFile::Skills, index, &specs, cx);
    }

    fn refresh_profession_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_profession else {
            return;
        };
        if index >= self.group_len(FieldGroup::Professions) {
            self.selected_profession = None;
            return;
        }
        let specs = self.profession_fields.clone();
        self.refresh_specs(ContentFile::Professions, index, &specs, cx);
    }

    /// Поля подсписка квеста: цель (`goals`) или предмет награды.
    fn sub_values(&self, file: ContentFile, base: &[PathSeg], keys: &[&str]) -> Vec<Option<Value>> {
        let Some(project) = self.project.as_ref() else {
            return keys.iter().map(|_| None).collect();
        };
        keys.iter()
            .map(|key| {
                let mut path = base.to_vec();
                path.push(PathSeg::key(*key));
                project.value_at(file, &path)
            })
            .collect()
    }

    fn refresh_goal_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, goal_index)) = self.selected_quest.zip(self.selected_goal) else {
            for slot in [
                &self.goal_kind,
                &self.goal_target,
                &self.goal_need,
                &self.goal_text,
            ] {
                Self::put_text(slot, "", cx);
            }
            return;
        };
        let base = vec![
            PathSeg::key(ContentFile::Quests.root_key()),
            PathSeg::index(quest_index),
            PathSeg::key("goals"),
            PathSeg::index(goal_index),
        ];
        let values = self.sub_values(
            ContentFile::Quests,
            &base,
            &["type", "target", "need", "text"],
        );
        if values.len() < 4 {
            return;
        }
        Self::put_text(
            &self.goal_kind,
            value_text(FieldKind::Text, values[0].as_ref()),
            cx,
        );
        Self::put_text(
            &self.goal_target,
            value_text(FieldKind::Text, values[1].as_ref()),
            cx,
        );
        Self::put_text(
            &self.goal_need,
            value_text(FieldKind::Int, values[2].as_ref()),
            cx,
        );
        Self::put_text(
            &self.goal_text,
            value_text(FieldKind::Text, values[3].as_ref()),
            cx,
        );
    }

    fn refresh_reward_item_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, item_index)) = self.selected_quest.zip(self.selected_reward_item)
        else {
            Self::put_text(&self.reward_type, "", cx);
            Self::put_text(&self.reward_qty, "", cx);
            return;
        };
        let base = vec![
            PathSeg::key(ContentFile::Quests.root_key()),
            PathSeg::index(quest_index),
            PathSeg::key("reward"),
            PathSeg::key("items"),
            PathSeg::index(item_index),
        ];
        let values = self.sub_values(ContentFile::Quests, &base, &["type", "qty"]);
        if values.len() < 2 {
            return;
        }
        Self::put_text(
            &self.reward_type,
            value_text(FieldKind::Text, values[0].as_ref()),
            cx,
        );
        Self::put_text(
            &self.reward_qty,
            value_text(FieldKind::Int, values[1].as_ref()),
            cx,
        );
    }

    fn refresh_recipe_input_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((recipe_index, input_index)) =
            self.selected_recipe.zip(self.selected_recipe_input)
        else {
            Self::put_text(&self.input_type, "", cx);
            Self::put_text(&self.input_qty, "", cx);
            return;
        };
        let base = vec![
            PathSeg::key(ContentFile::Recipes.root_key()),
            PathSeg::index(recipe_index),
            PathSeg::key("inputs"),
            PathSeg::index(input_index),
        ];
        let values = self.sub_values(ContentFile::Recipes, &base, &["type", "qty"]);
        if values.len() < 2 {
            return;
        }
        Self::put_text(
            &self.input_type,
            value_text(FieldKind::Text, values[0].as_ref()),
            cx,
        );
        Self::put_text(
            &self.input_qty,
            value_text(FieldKind::Int, values[1].as_ref()),
            cx,
        );
    }

    /// Применить группу полей записи (E5).
    fn apply_group(&mut self, group: FieldGroup, cx: &mut Context<'_, Self>) {
        let Some(index) = self.group_selected(group) else {
            self.status = "запись не выбрана".into();
            cx.notify();
            return;
        };
        let file = group.file();
        let specs = self.group_specs(group).to_vec();
        let mut edits: Vec<Edit> = Vec::new();
        let mut failed: Option<String> = None;
        {
            let Some(project) = self.project.as_ref() else {
                return;
            };
            for spec in &specs {
                let text = self
                    .field_text(&Some(spec.input.clone()), cx)
                    .trim()
                    .to_string();
                let mut path = vec![PathSeg::key(file.root_key()), PathSeg::index(index)];
                path.extend(spec.path.iter().map(|key| PathSeg::key(*key)));
                if text.is_empty() {
                    if let Some(clear) = spec.clear {
                        let mut clear_path =
                            vec![PathSeg::key(file.root_key()), PathSeg::index(index)];
                        clear_path.extend(clear.iter().map(|key| PathSeg::key(*key)));
                        if project.value_at(file, &clear_path).is_some() {
                            edits.push(Edit::RemoveKey(clear_path));
                        }
                    } else if matches!(spec.kind, FieldKind::Text) {
                        failed = Some(format!("поле «{}» не может быть пустым", spec.label));
                    }
                    continue;
                }
                match value_from_text(spec.kind, &text) {
                    Ok(value) => edits.push(Edit::Set(path, value)),
                    Err(err) => {
                        failed = Some(format!("{}: {err}", spec.label));
                        break;
                    }
                }
            }
        }
        if let Some(err) = failed {
            self.status = err;
            cx.notify();
            return;
        }
        if edits.is_empty() {
            self.status = "нечего применять".into();
            cx.notify();
            return;
        }
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.apply_edits(file, edits) {
            Ok(()) => {
                self.status = format!("применено: {}", group.section().title());
                self.refresh_group(group, cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    /// Применить поля подсписка (цель/награда/ингредиент).
    fn apply_goal(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, goal_index)) = self.selected_quest.zip(self.selected_goal) else {
            self.status = "цель не выбрана".into();
            cx.notify();
            return;
        };
        let kind = self.field_text(&self.goal_kind, cx).trim().to_string();
        if kind.is_empty() {
            self.status = "тип цели не может быть пустым".into();
            cx.notify();
            return;
        }
        let target = self.field_text(&self.goal_target, cx).trim().to_string();
        let need = self.field_text(&self.goal_need, cx).trim().to_string();
        let text = self.field_text(&self.goal_text, cx).trim().to_string();
        let need_value = if need.is_empty() {
            1
        } else {
            match need.parse::<i64>() {
                Ok(value) => value,
                Err(_) => {
                    self.status = format!("need: не целое число: {need}");
                    cx.notify();
                    return;
                }
            }
        };
        let target_value = if target.is_empty() {
            Value::Null
        } else {
            Value::from(target)
        };
        let fields: Vec<(&str, Value)> = vec![
            ("type", Value::from(kind)),
            ("target", target_value),
            ("need", Value::from(need_value)),
            ("text", Value::from(text)),
        ];
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_quest_goal(quest_index, goal_index, fields) {
            Ok(()) => {
                self.status = "цель применена".into();
                self.refresh_quest_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn apply_reward_item(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, item_index)) = self.selected_quest.zip(self.selected_reward_item)
        else {
            self.status = "предмет награды не выбран".into();
            cx.notify();
            return;
        };
        let item_type = self.field_text(&self.reward_type, cx).trim().to_string();
        if item_type.is_empty() {
            self.status = "тип предмета не может быть пустым".into();
            cx.notify();
            return;
        }
        let qty_text = self.field_text(&self.reward_qty, cx).trim().to_string();
        let qty = if qty_text.is_empty() {
            1
        } else {
            match qty_text.parse::<i64>() {
                Ok(value) => value,
                Err(_) => {
                    self.status = format!("qty: не целое число: {qty_text}");
                    cx.notify();
                    return;
                }
            }
        };
        let fields: Vec<(&str, Value)> =
            vec![("type", Value::from(item_type)), ("qty", Value::from(qty))];
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_quest_reward_item(quest_index, item_index, fields) {
            Ok(()) => {
                self.status = "награда применена".into();
                self.refresh_quest_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn apply_recipe_input(&mut self, cx: &mut Context<'_, Self>) {
        let Some((recipe_index, input_index)) =
            self.selected_recipe.zip(self.selected_recipe_input)
        else {
            self.status = "ингредиент не выбран".into();
            cx.notify();
            return;
        };
        let item_type = self.field_text(&self.input_type, cx).trim().to_string();
        if item_type.is_empty() {
            self.status = "тип ингредиента не может быть пустым".into();
            cx.notify();
            return;
        }
        let qty_text = self.field_text(&self.input_qty, cx).trim().to_string();
        let qty = if qty_text.is_empty() {
            1
        } else {
            match qty_text.parse::<i64>() {
                Ok(value) => value,
                Err(_) => {
                    self.status = format!("qty: не целое число: {qty_text}");
                    cx.notify();
                    return;
                }
            }
        };
        let fields: Vec<(&str, Value)> =
            vec![("type", Value::from(item_type)), ("qty", Value::from(qty))];
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_recipe_input(recipe_index, input_index, fields) {
            Ok(()) => {
                self.status = "ингредиент применён".into();
                self.refresh_recipe_fields(cx);
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    // ─── E5: добавление/удаление записей и строк подсписков ─────────────

    fn new_record_click(&mut self, section: Section, cx: &mut Context<'_, Self>) {
        let Some(group) = Self::section_group(section) else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            match group {
                FieldGroup::Dialogs => project.add_dialog_npc(),
                FieldGroup::Shops => project.add_shop_npc(),
                FieldGroup::Quests => project.add_quest(),
                FieldGroup::Recipes => project.add_recipe(),
                FieldGroup::Skills => project.add_skill(),
                FieldGroup::Professions => project.add_profession(),
            }
        };
        match result {
            Ok(index) => {
                self.set_group_selected(group, Some(index));
                self.filter.clear();
                Self::put_text(&self.search, "", cx);
                self.refresh_group(group, cx);
                self.status = format!("добавлено: {}", group.section().title());
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_record(&mut self, group: FieldGroup, cx: &mut Context<'_, Self>) {
        let Some(index) = self.group_selected(group) else {
            self.status = "запись не выбрана".into();
            cx.notify();
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            match group {
                FieldGroup::Dialogs => project.remove_dialog_npc(index),
                FieldGroup::Shops => project.remove_shop_npc(index),
                FieldGroup::Quests => project.remove_quest(index),
                FieldGroup::Recipes => project.remove_recipe(index),
                FieldGroup::Skills => project.remove_skill(index),
                FieldGroup::Professions => project.remove_profession(index),
            }
        };
        match result {
            Ok(()) => {
                self.set_group_selected(group, None);
                self.clear_group_fields(group, cx);
                self.status = format!("удалено: {}", group.section().title());
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_goal_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(quest_index) = self.selected_quest else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_quest_goal(quest_index)
        };
        match result {
            Ok(index) => {
                self.selected_goal = Some(index);
                self.refresh_quest_fields(cx);
                self.status = "цель добавлена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_goal(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, goal_index)) = self.selected_quest.zip(self.selected_goal) else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_quest_goal(quest_index, goal_index)
        };
        match result {
            Ok(()) => {
                self.selected_goal = None;
                self.refresh_quest_fields(cx);
                self.status = "цель удалена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_reward_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(quest_index) = self.selected_quest else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_quest_reward_item(quest_index)
        };
        match result {
            Ok(index) => {
                self.selected_reward_item = Some(index);
                self.refresh_quest_fields(cx);
                self.status = "предмет награды добавлен".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_reward_item(&mut self, cx: &mut Context<'_, Self>) {
        let Some((quest_index, item_index)) = self.selected_quest.zip(self.selected_reward_item)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_quest_reward_item(quest_index, item_index)
        };
        match result {
            Ok(()) => {
                self.selected_reward_item = None;
                self.refresh_quest_fields(cx);
                self.status = "предмет награды удалён".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_input_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(recipe_index) = self.selected_recipe else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_recipe_input(recipe_index)
        };
        match result {
            Ok(index) => {
                self.selected_recipe_input = Some(index);
                self.refresh_recipe_fields(cx);
                self.status = "ингредиент добавлен".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_input(&mut self, cx: &mut Context<'_, Self>) {
        let Some((recipe_index, input_index)) =
            self.selected_recipe.zip(self.selected_recipe_input)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_recipe_input(recipe_index, input_index)
        };
        match result {
            Ok(()) => {
                self.selected_recipe_input = None;
                self.refresh_recipe_fields(cx);
                self.status = "ингредиент удалён".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn select_goal(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_goal = Some(index);
        self.refresh_goal_fields(cx);
        cx.notify();
    }

    fn select_reward_item(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_reward_item = Some(index);
        self.refresh_reward_item_fields(cx);
        cx.notify();
    }

    fn select_recipe_input(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_recipe_input = Some(index);
        self.refresh_recipe_input_fields(cx);
        cx.notify();
    }

    // ─── E5: пикеры ссылок ────────────────────────────────────────────────

    fn open_picker(
        &mut self,
        kind: RefKind,
        target: Entity<TextInput>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(search) = self.picker_search.clone() else {
            return;
        };
        search.update(cx, |input, cx| input.set_text("", cx));
        self.picker = Some(PickerState {
            kind,
            title: ref_kind_title(kind),
            target,
            filter: String::new(),
            search,
        });
        self.status = format!("выбор: {}", ref_kind_title(kind));
        cx.notify();
    }

    fn cancel_picker(&mut self, cx: &mut Context<'_, Self>) {
        self.picker = None;
        cx.notify();
    }

    fn choose_picker(&mut self, id: String, cx: &mut Context<'_, Self>) {
        if let Some(picker) = self.picker.take() {
            let target = picker.target;
            target.update(cx, |input, cx| input.set_text(id, cx));
            self.status = "ссылка выбрана".into();
        }
        cx.notify();
    }

    /// Строки формы группы: подпись + поле + кнопка пикера.
    fn spec_rows(&self, group: FieldGroup, cx: &mut Context<'_, Self>) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::new();
        for spec in self.group_specs(group).iter().cloned() {
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(spec.label),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Input::new(&spec.input)),
                );
            if let Some(kind) = spec.picker {
                let target = spec.input.clone();
                let picker_label = format!("… {}", ref_kind_title(kind));
                row = row.child(
                    div()
                        .id(SharedString::from(format!(
                            "pick-{}-{}",
                            group.section().title(),
                            spec.label
                        )))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(PANEL_ALT))
                        .text_size(px(11.0))
                        .cursor_pointer()
                        .child(picker_label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_picker(kind, target.clone(), cx)
                        })),
                );
            }
            rows.push(row.into_any_element());
        }
        rows
    }

    /// Панель пикера вместо инспектора.
    fn picker_panel(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        let Some(picker) = self.picker.as_ref() else {
            return div().into_any_element();
        };
        let filter = picker.filter.trim().to_lowercase();
        let mut choices = self
            .project
            .as_ref()
            .map(|project| project.choices(picker.kind))
            .unwrap_or_default();
        if !filter.is_empty() {
            choices.retain(|choice| {
                choice.id.to_lowercase().contains(&filter)
                    || choice.label.to_lowercase().contains(&filter)
            });
        }
        let count = choices.len();
        let mut list = div()
            .id("picker-list")
            .flex_1()
            .overflow_y_scroll()
            .min_h_0();
        for (row_index, choice) in choices.into_iter().take(300).enumerate() {
            let id = choice.id.clone();
            list = list.child(
                div()
                    .id(("picker-row", row_index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(PANEL_ALT)))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_picker(id.clone(), cx)))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(SharedString::from(choice.label)),
                    ),
            );
        }
        div()
            .id("inspector-picker")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(15.0))
                            .child(format!("Выбор: {}", picker.title)),
                    )
                    .child(
                        div()
                            .id("picker-cancel")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .text_size(px(11.0))
                            .cursor_pointer()
                            .child("× отмена")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_picker(cx))),
                    ),
            )
            .child(Input::new(&picker.search))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!("{count} вариантов")),
            )
            .child(list)
            .into_any_element()
    }

    // ─── E5: списки и инспекторы записей ─────────────────────────────────

    /// Строки списка раздела записей: (индекс, id, имя, третья колонка).
    fn record_rows(&self, section: Section) -> Vec<(usize, String, String, String)> {
        let filter = self.filter.trim().to_lowercase();
        let Some(project) = self.project.as_ref() else {
            return Vec::new();
        };
        let content = project.content();
        let matches = |id: &str, name: &str| {
            filter.is_empty()
                || id.to_lowercase().contains(&filter)
                || name.to_lowercase().contains(&filter)
        };
        let mut rows = Vec::new();
        match section {
            Section::Dialogs => {
                for (index, npc) in project.dialogs().npcs.iter().enumerate() {
                    let root = npc.root.clone().unwrap_or_default();
                    if !matches(&npc.id, &root) {
                        continue;
                    }
                    rows.push((
                        index,
                        npc.id.clone(),
                        format!("root: {root}"),
                        format!("{} стр. · {} обуч.", npc.lines.len(), npc.training.len()),
                    ));
                }
            }
            Section::Shops => {
                for (index, npc) in project.shops().npcs.iter().enumerate() {
                    if !matches(&npc.id, "") {
                        continue;
                    }
                    let goods: usize = npc.currencies.iter().map(|c| c.items.len()).sum();
                    rows.push((
                        index,
                        npc.id.clone(),
                        format!("markup {:.2}", npc.markup),
                        format!("{} валют · {} товаров", npc.currencies.len(), goods),
                    ));
                }
            }
            Section::Quests => {
                for (index, quest) in content.quests.iter().enumerate() {
                    if !matches(&quest.id, &quest.title) {
                        continue;
                    }
                    rows.push((
                        index,
                        quest.id.clone(),
                        quest.title.clone(),
                        format!("{} цел.", quest.goals.len()),
                    ));
                }
            }
            Section::Recipes => {
                for (index, recipe) in content.recipes.iter().enumerate() {
                    if !matches(&recipe.id, &recipe.name) {
                        continue;
                    }
                    rows.push((
                        index,
                        recipe.id.clone(),
                        recipe.name.clone(),
                        format!("{} · {} ингр.", recipe.profession, recipe.inputs.len()),
                    ));
                }
            }
            Section::Skills => {
                for (index, skill) in content.skills.iter().enumerate() {
                    if !matches(&skill.id, &skill.name) {
                        continue;
                    }
                    rows.push((
                        index,
                        skill.id.clone(),
                        skill.name.clone(),
                        skill.group.clone(),
                    ));
                }
            }
            Section::Professions => {
                for (index, profession) in content.professions.iter().enumerate() {
                    if !matches(&profession.id, &profession.name) {
                        continue;
                    }
                    rows.push((
                        index,
                        profession.id.clone(),
                        profession.name.clone(),
                        profession.prof_type.clone(),
                    ));
                }
            }
            _ => {}
        }
        rows
    }

    fn records_list(&self, section: Section, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        let rows = self.record_rows(section);
        let count = rows.len();
        let selected = Self::section_group(section).and_then(|group| self.group_selected(group));
        let mut list = div()
            .id("records-list")
            .flex_1()
            .overflow_y_scroll()
            .min_h_0();
        for (index, id, name, meta) in rows {
            list = list.child(
                div()
                    .id(("record-row", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected == Some(index), |d| d.bg(rgb(PANEL_ALT)))
                    .hover(|d| d.bg(rgb(PANEL_ALT)))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_record(section, index, cx)),
                    )
                    .child(
                        div()
                            .w(px(130.0))
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(id),
                    )
                    .child(div().flex_1().text_size(px(12.0)).child(name))
                    .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(meta)),
            );
        }
        let new_label = match section {
            Section::Dialogs => "+ диалог",
            Section::Shops => "+ магазин",
            Section::Quests => "+ квест",
            Section::Recipes => "+ рецепт",
            Section::Skills => "+ навык",
            Section::Professions => "+ профессия",
            _ => "+",
        };
        div()
            .w(px(400.0))
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(Input::new(
                        self.search.as_ref().expect("поле поиска создано"),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED))
                                    .child(format!("{count} записей")),
                            )
                            .child(
                                div()
                                    .id("new-record")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .bg(rgb(PANEL_ALT))
                                    .text_size(px(11.0))
                                    .cursor_pointer()
                                    .child(new_label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.new_record_click(section, cx)
                                    })),
                            ),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn inspector_panel(&self, id: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .overflow_y_scroll()
    }

    /// Кнопки «Применить/Удалить» для записи группы.
    fn record_buttons(&self, group: FieldGroup, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let apply_label = format!("Применить ({})", group.section().title().to_lowercase());
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("btn-apply-record")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_ALT))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .child(apply_label)
                    .on_click(cx.listener(move |this, _, _, cx| this.apply_group(group, cx))),
            )
            .child(
                self.button("btn-delete-record", "Удалить")
                    .on_click(cx.listener(move |this, _, _, cx| this.delete_record(group, cx))),
            )
    }

    fn quests_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        if self.picker.is_some() {
            return self.picker_panel(cx);
        }
        let mut panel = self.inspector_panel("inspector-quests");
        let Some(project) = self.project.as_ref() else {
            return panel
                .child(
                    div().text_color(rgb(ERR)).child(
                        self.open_error
                            .clone()
                            .unwrap_or_else(|| "проект не открыт".into()),
                    ),
                )
                .into_any_element();
        };
        let Some(index) = self.selected_quest else {
            return panel
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("Выберите квест в списке (E5: форма, цели, награда)."),
                )
                .into_any_element();
        };
        let Some(quest) = project.content().quests.get(index) else {
            return panel
                .child(div().text_color(rgb(ERR)).child("квест исчез"))
                .into_any_element();
        };
        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · {}", quest.id, quest.title)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.spec_rows(FieldGroup::Quests, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child("giver — ещё и локация:"),
                    )
                    .child(
                        div()
                            .id("pick-quest-giver-location")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(PANEL_ALT))
                            .text_size(px(11.0))
                            .cursor_pointer()
                            .child("… локация")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let Some(target) = this.q_giver.clone() else {
                                    return;
                                };
                                this.open_picker(RefKind::Location, target, cx);
                            })),
                    ),
            )
            .child(self.record_buttons(FieldGroup::Quests, cx));

        // ── цели ──
        let mut goals = div().flex().flex_col().gap_1();
        goals = goals.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Цели ({})", quest.goals.len())),
                )
                .child(
                    self.button("btn-add-goal", "+ цель")
                        .on_click(cx.listener(|this, _, _, cx| this.add_goal_click(cx))),
                ),
        );
        let selected_goal = self.selected_goal;
        for (goal_index, goal) in quest.goals.iter().enumerate() {
            let mut row = div()
                .id(("goal-row", goal_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected_goal == Some(goal_index), |d| d.bg(rgb(PANEL_ALT)))
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(cx.listener(move |this, _, _, cx| this.select_goal(goal_index, cx)))
                .child(div().flex_1().text_size(px(12.0)).child(format!(
                    "{} · {} × {}",
                    goal.goal_type,
                    goal.target.clone().unwrap_or_else(|| "—".into()),
                    goal.need
                )));
            row = row.child(
                div()
                    .id(("goal-del", goal_index))
                    .px_1()
                    .cursor_pointer()
                    .text_color(rgb(ERR))
                    .text_size(px(13.0))
                    .child("×")
                    .on_click(cx.listener(|this, _, _, cx| this.delete_goal(cx))),
            );
            goals = goals.child(row);
        }
        if self.selected_goal.is_some() {
            goals = goals
                .child(self.text_field("type (kill)", &self.goal_kind))
                .child(self.text_field("target (id монстра)", &self.goal_target))
                .child(
                    div().flex().items_center().gap_2().child(
                        div()
                            .id("pick-goal-target")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(PANEL_ALT))
                            .text_size(px(11.0))
                            .cursor_pointer()
                            .child("… монстр")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let Some(target) = this.goal_target.clone() else {
                                    return;
                                };
                                this.open_picker(RefKind::Monster, target, cx);
                            })),
                    ),
                )
                .child(self.text_field("need", &self.goal_need))
                .child(self.text_field("text", &self.goal_text))
                .child(
                    self.button("btn-apply-goal", "Применить цель")
                        .on_click(cx.listener(|this, _, _, cx| this.apply_goal(cx))),
                );
        }
        panel = panel.child(goals);

        // ── награда ──
        let mut reward = div().flex().flex_col().gap_1();
        reward = reward.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Предметы награды ({})", quest.reward.items.len())),
                )
                .child(
                    self.button("btn-add-reward", "+ предмет")
                        .on_click(cx.listener(|this, _, _, cx| this.add_reward_click(cx))),
                ),
        );
        let selected_reward = self.selected_reward_item;
        for (item_index, item) in quest.reward.items.iter().enumerate() {
            let mut row = div()
                .id(("reward-row", item_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected_reward == Some(item_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_reward_item(item_index, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(format!("{} × {}", item.item_type, item.qty)),
                );
            row = row.child(
                div()
                    .id(("reward-del", item_index))
                    .px_1()
                    .cursor_pointer()
                    .text_color(rgb(ERR))
                    .text_size(px(13.0))
                    .child("×")
                    .on_click(cx.listener(|this, _, _, cx| this.delete_reward_item(cx))),
            );
            reward = reward.child(row);
        }
        if self.selected_reward_item.is_some() {
            reward = reward
                .child(self.text_field("type (id предмета)", &self.reward_type))
                .child(self.text_field("qty", &self.reward_qty))
                .child(
                    self.button("btn-apply-reward", "Применить награду")
                        .on_click(cx.listener(|this, _, _, cx| this.apply_reward_item(cx))),
                );
        }
        panel = panel.child(reward);

        panel = panel.child(self.actions_row(cx, project.can_undo(), project.can_redo()));
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(3) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("⚠ {warning}")),
            );
        }
        panel.into_any_element()
    }

    fn recipes_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        if self.picker.is_some() {
            return self.picker_panel(cx);
        }
        let mut panel = self.inspector_panel("inspector-recipes");
        let Some(project) = self.project.as_ref() else {
            return panel
                .child(
                    div().text_color(rgb(ERR)).child(
                        self.open_error
                            .clone()
                            .unwrap_or_else(|| "проект не открыт".into()),
                    ),
                )
                .into_any_element();
        };
        let Some(index) = self.selected_recipe else {
            return panel
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("Выберите рецепт в списке (E5: форма, ингредиенты)."),
                )
                .into_any_element();
        };
        let Some(recipe) = project.content().recipes.get(index) else {
            return panel
                .child(div().text_color(rgb(ERR)).child("рецепт исчез"))
                .into_any_element();
        };
        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · {}", recipe.id, recipe.name)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.spec_rows(FieldGroup::Recipes, cx)),
            )
            .child(self.record_buttons(FieldGroup::Recipes, cx));

        let mut inputs = div().flex().flex_col().gap_1();
        inputs = inputs.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Ингредиенты ({})", recipe.inputs.len())),
                )
                .child(
                    self.button("btn-add-input", "+ ингредиент")
                        .on_click(cx.listener(|this, _, _, cx| this.add_input_click(cx))),
                ),
        );
        let selected_input = self.selected_recipe_input;
        for (input_index, input) in recipe.inputs.iter().enumerate() {
            let mut row = div()
                .id(("input-row", input_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected_input == Some(input_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_recipe_input(input_index, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(format!("{} × {}", input.item_type, input.qty)),
                );
            row = row.child(
                div()
                    .id(("input-del", input_index))
                    .px_1()
                    .cursor_pointer()
                    .text_color(rgb(ERR))
                    .text_size(px(13.0))
                    .child("×")
                    .on_click(cx.listener(|this, _, _, cx| this.delete_input(cx))),
            );
            inputs = inputs.child(row);
        }
        if self.selected_recipe_input.is_some() {
            inputs = inputs
                .child(self.text_field("type (id/тип предмета)", &self.input_type))
                .child(self.text_field("qty", &self.input_qty))
                .child(
                    self.button("btn-apply-input", "Применить ингредиент")
                        .on_click(cx.listener(|this, _, _, cx| this.apply_recipe_input(cx))),
                );
        }
        panel = panel.child(inputs);
        panel = panel.child(self.actions_row(cx, project.can_undo(), project.can_redo()));
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(3) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("⚠ {warning}")),
            );
        }
        panel.into_any_element()
    }

    fn skills_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        self.simple_record_inspector(
            FieldGroup::Skills,
            "inspector-skills",
            "Выберите навык в списке (E5).",
            cx,
        )
    }

    fn professions_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        self.simple_record_inspector(
            FieldGroup::Professions,
            "inspector-professions",
            "Выберите профессию в списке (E5).",
            cx,
        )
    }

    /// Инспектор группы без подсписков (навыки, профессии).
    fn simple_record_inspector(
        &self,
        group: FieldGroup,
        id: &'static str,
        hint: &'static str,
        cx: &mut Context<'_, Self>,
    ) -> gpui::AnyElement {
        if self.picker.is_some() {
            return self.picker_panel(cx);
        }
        let mut panel = self.inspector_panel(id);
        let Some(project) = self.project.as_ref() else {
            return panel
                .child(
                    div().text_color(rgb(ERR)).child(
                        self.open_error
                            .clone()
                            .unwrap_or_else(|| "проект не открыт".into()),
                    ),
                )
                .into_any_element();
        };
        let Some(index) = self.group_selected(group) else {
            return panel
                .child(div().text_color(rgb(MUTED)).child(hint))
                .into_any_element();
        };
        if index >= self.group_len(group) {
            return panel
                .child(div().text_color(rgb(ERR)).child("запись исчезла"))
                .into_any_element();
        }
        let title = format!(
            "{} · {}",
            self.record_id(group, index),
            match group {
                FieldGroup::Skills => project.content().skills[index].name.clone(),
                FieldGroup::Professions => project.content().professions[index].name.clone(),
                _ => String::new(),
            }
        );
        panel = panel
            .child(div().text_size(px(15.0)).child(title))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.spec_rows(group, cx)),
            )
            .child(self.record_buttons(group, cx))
            .child(self.actions_row(cx, project.can_undo(), project.can_redo()));
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(3) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("⚠ {warning}")),
            );
        }
        panel.into_any_element()
    }

    // ─── элементы UI ─────────────────────────────────────────────────────
    // ─── элементы UI ─────────────────────────────────────────────────────

    fn button(&self, id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_ALT))
            .text_size(px(12.0))
            .cursor_pointer()
            .child(label)
    }

    fn text_field(
        &self,
        label: &'static str,
        input: &Option<Entity<TextInput>>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(label),
            )
            .child(match input {
                Some(input) => Input::new(input).into_any_element(),
                None => div().h(px(28.0)).into_any_element(),
            })
    }

    fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (title, meta) = match &self.project {
            None => ("ld2-editor — проект не открыт".to_string(), String::new()),
            Some(project) => (
                format!("ld2-editor — {}", project.root().display()),
                format!(
                    "контент: {} · версия {} · локаций {} · предметов {} · Ctrl+P — переход · F1 — помощь",
                    project.content_dir().display(),
                    short(project.version()),
                    project.content().locations.len(),
                    project.content().items.len(),
                ),
            ),
        };
        let seed_label = if self.seed_running { "Seed…" } else { "Seed" };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(16.0)).child(title))
                    .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(meta)),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        self.button("btn-seed", seed_label)
                            .on_click(cx.listener(|this, _, _, cx| this.seed_click(cx))),
                    )
                    .child(self.button("btn-help", "Помощь").on_click(
                        cx.listener(|this, _, _, cx| this.switch_section(Section::Help, cx)),
                    )),
            )
    }

    // ─── E5.2: диалоги ───────────────────────────────────────────────────

    fn refresh_dialog_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_dialog else {
            return;
        };
        let Some((line_count, training_count, reply_count)) = self
            .project
            .as_ref()
            .and_then(|project| project.dialogs().npcs.get(index))
            .map(|npc| {
                let replies = self
                    .selected_dialog_line
                    .and_then(|line| npc.lines.get(line))
                    .map(|line| line.replies.len())
                    .unwrap_or(0);
                (npc.lines.len(), npc.training.len(), replies)
            })
        else {
            self.selected_dialog = None;
            return;
        };
        if let Some(line) = self.selected_dialog_line {
            if line >= line_count {
                self.selected_dialog_line = None;
                self.selected_dialog_reply = None;
            }
        }
        if let Some(reply) = self.selected_dialog_reply {
            if reply >= reply_count {
                self.selected_dialog_reply = None;
            }
        }
        if let Some(training) = self.selected_training {
            if training >= training_count {
                self.selected_training = None;
            }
        }
        let specs = self.dialog_fields.clone();
        self.refresh_specs(ContentFile::Dialogs, index, &specs, cx);
        self.refresh_dialog_line_fields(cx);
        self.refresh_training_fields(cx);
    }

    fn refresh_dialog_line_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index)) = self.selected_dialog.zip(self.selected_dialog_line)
        else {
            return;
        };
        let Some(line) = self
            .project
            .as_ref()
            .and_then(|project| project.dialogs().npcs.get(npc_index))
            .and_then(|npc| npc.lines.get(line_index))
            .cloned()
        else {
            self.selected_dialog_line = None;
            return;
        };
        Self::put_text(&self.line_id, line.id, cx);
        Self::put_text(&self.line_text, line.text, cx);
        Self::put_text(&self.line_quest, line.quest.unwrap_or_default(), cx);
        Self::put_text(
            &self.line_every,
            line.every_s
                .map(|value| value.to_string())
                .unwrap_or_default(),
            cx,
        );
        Self::put_text(
            &self.line_when,
            line.when
                .and_then(|when| serde_json::to_string_pretty(&when).ok())
                .unwrap_or_default(),
            cx,
        );
        self.refresh_dialog_reply_fields(cx);
    }

    fn refresh_dialog_reply_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index, reply_index)) = self
            .selected_dialog
            .zip(self.selected_dialog_line)
            .zip(self.selected_dialog_reply)
            .map(|((npc, line), reply)| (npc, line, reply))
        else {
            return;
        };
        let Some(reply) = self
            .project
            .as_ref()
            .and_then(|project| project.dialogs().npcs.get(npc_index))
            .and_then(|npc| npc.lines.get(line_index))
            .and_then(|line| line.replies.get(reply_index))
            .cloned()
        else {
            self.selected_dialog_reply = None;
            return;
        };
        Self::put_text(&self.reply_id, reply.id, cx);
        Self::put_text(&self.reply_text, reply.text, cx);
        Self::put_text(&self.reply_next, reply.next.unwrap_or_default(), cx);
        Self::put_text(&self.reply_action, reply.action.unwrap_or_default(), cx);
        Self::put_text(
            &self.reply_when,
            reply
                .when
                .and_then(|when| serde_json::to_string_pretty(&when).ok())
                .unwrap_or_default(),
            cx,
        );
    }

    fn refresh_training_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, entry_index)) = self.selected_dialog.zip(self.selected_training)
        else {
            return;
        };
        let Some(entry) = self
            .project
            .as_ref()
            .and_then(|project| project.dialogs().npcs.get(npc_index))
            .and_then(|npc| npc.training.get(entry_index))
            .cloned()
        else {
            self.selected_training = None;
            return;
        };
        Self::put_text(&self.train_skill, entry.skill, cx);
        Self::put_text(&self.train_points, entry.points.to_string(), cx);
        Self::put_text(&self.train_gold, entry.gold.to_string(), cx);
    }

    fn select_dialog_line(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_dialog_line = Some(index);
        self.selected_dialog_reply = None;
        self.refresh_dialog_line_fields(cx);
        cx.notify();
    }

    fn select_dialog_reply(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_dialog_reply = Some(index);
        self.refresh_dialog_reply_fields(cx);
        cx.notify();
    }

    fn select_training(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_training = Some(index);
        self.refresh_training_fields(cx);
        cx.notify();
    }

    fn apply_dialog_line(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index)) = self.selected_dialog.zip(self.selected_dialog_line)
        else {
            self.status = "не выбрана строка диалога".into();
            cx.notify();
            return;
        };
        let id = self.field_text(&self.line_id, cx);
        let text = self.field_text(&self.line_text, cx);
        let quest = self.field_text(&self.line_quest, cx);
        let every_text = self.field_text(&self.line_every, cx);
        let when_text = self.field_text(&self.line_when, cx);
        let quest = if quest.trim().is_empty() {
            Value::Null
        } else {
            Value::from(quest.trim())
        };
        let every = match every_text.trim() {
            "" => Value::Null,
            value => match value.parse::<i64>() {
                Ok(number) => Value::from(number),
                Err(_) => {
                    self.status = format!("every_s: нужно целое число (получено «{value}»)");
                    cx.notify();
                    return;
                }
            },
        };
        let when = match when_text.trim() {
            "" => Value::Null,
            value => match serde_json::from_str::<Value>(value) {
                Ok(Value::Object(map)) => Value::Object(map),
                _ => {
                    self.status = "when: ожидается JSON-объект".into();
                    cx.notify();
                    return;
                }
            },
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_dialog_line(
            npc_index,
            line_index,
            vec![
                ("id", Value::from(id)),
                ("text", Value::from(text)),
                ("quest", quest),
                ("every_s", every),
                ("when", when),
            ],
        ) {
            Ok(()) => self.status = "строка диалога применена (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_dialog_fields(cx);
        cx.notify();
    }

    fn apply_dialog_reply(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index, reply_index)) = self
            .selected_dialog
            .zip(self.selected_dialog_line)
            .zip(self.selected_dialog_reply)
            .map(|((npc, line), reply)| (npc, line, reply))
        else {
            self.status = "не выбрана реплика".into();
            cx.notify();
            return;
        };
        let id = self.field_text(&self.reply_id, cx);
        let text = self.field_text(&self.reply_text, cx);
        let next = self.field_text(&self.reply_next, cx);
        let action = self.field_text(&self.reply_action, cx);
        let when_text = self.field_text(&self.reply_when, cx);
        let next = if next.trim().is_empty() {
            Value::Null
        } else {
            Value::from(next.trim())
        };
        let action = if action.trim().is_empty() {
            Value::Null
        } else {
            Value::from(action.trim())
        };
        let when = match when_text.trim() {
            "" => Value::Null,
            value => match serde_json::from_str::<Value>(value) {
                Ok(Value::Object(map)) => Value::Object(map),
                _ => {
                    self.status = "when: ожидается JSON-объект".into();
                    cx.notify();
                    return;
                }
            },
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_dialog_reply(
            npc_index,
            line_index,
            reply_index,
            vec![
                ("id", Value::from(id)),
                ("text", Value::from(text)),
                ("next", next),
                ("action", action),
                ("when", when),
            ],
        ) {
            Ok(()) => self.status = "реплика применена (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_dialog_fields(cx);
        cx.notify();
    }

    fn apply_training(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, entry_index)) = self.selected_dialog.zip(self.selected_training)
        else {
            self.status = "не выбрана запись обучения".into();
            cx.notify();
            return;
        };
        let skill = self.field_text(&self.train_skill, cx);
        let points_text = self.field_text(&self.train_points, cx);
        let gold_text = self.field_text(&self.train_gold, cx);
        let points = match points_text.trim() {
            "" => 0,
            value => match value.parse::<i64>() {
                Ok(number) => number,
                Err(_) => {
                    self.status = format!("points: нужно целое число (получено «{value}»)");
                    cx.notify();
                    return;
                }
            },
        };
        let gold = match gold_text.trim() {
            "" => 0,
            value => match value.parse::<i64>() {
                Ok(number) => number,
                Err(_) => {
                    self.status = format!("gold: нужно целое число (получено «{value}»)");
                    cx.notify();
                    return;
                }
            },
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_training_entry(
            npc_index,
            entry_index,
            vec![
                ("skill", Value::from(skill)),
                ("points", Value::from(points)),
                ("gold", Value::from(gold)),
            ],
        ) {
            Ok(()) => self.status = "обучение применено (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_dialog_fields(cx);
        cx.notify();
    }

    fn add_dialog_line_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(npc_index) = self.selected_dialog else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_dialog_line(npc_index)
        };
        match result {
            Ok(index) => {
                self.selected_dialog_line = Some(index);
                self.selected_dialog_reply = None;
                self.refresh_dialog_fields(cx);
                self.status = "строка добавлена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_dialog_line_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index)) = self.selected_dialog.zip(self.selected_dialog_line)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_dialog_line(npc_index, line_index)
        };
        match result {
            Ok(()) => {
                self.selected_dialog_line = None;
                self.selected_dialog_reply = None;
                self.refresh_dialog_fields(cx);
                self.status = "строка удалена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_reply_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index)) = self.selected_dialog.zip(self.selected_dialog_line)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_dialog_reply(npc_index, line_index)
        };
        match result {
            Ok(index) => {
                self.selected_dialog_reply = Some(index);
                self.refresh_dialog_fields(cx);
                self.status = "реплика добавлена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_reply_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, line_index, reply_index)) = self
            .selected_dialog
            .zip(self.selected_dialog_line)
            .zip(self.selected_dialog_reply)
            .map(|((npc, line), reply)| (npc, line, reply))
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_dialog_reply(npc_index, line_index, reply_index)
        };
        match result {
            Ok(()) => {
                self.selected_dialog_reply = None;
                self.refresh_dialog_fields(cx);
                self.status = "реплика удалена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_training_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(npc_index) = self.selected_dialog else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_training_entry(npc_index)
        };
        match result {
            Ok(index) => {
                self.selected_training = Some(index);
                self.refresh_dialog_fields(cx);
                self.status = "запись обучения добавлена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_training_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((npc_index, entry_index)) = self.selected_dialog.zip(self.selected_training)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_training_entry(npc_index, entry_index)
        };
        match result {
            Ok(()) => {
                self.selected_training = None;
                self.refresh_dialog_fields(cx);
                self.status = "запись обучения удалена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    // ─── E5.2: магазины ──────────────────────────────────────────────────

    fn refresh_shop_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_shop else {
            return;
        };
        let Some((currency_count, item_count)) = self
            .project
            .as_ref()
            .and_then(|project| project.shops().npcs.get(index))
            .map(|npc| {
                let items = self
                    .selected_currency
                    .and_then(|currency| npc.currencies.get(currency))
                    .map(|currency| currency.items.len())
                    .unwrap_or(0);
                (npc.currencies.len(), items)
            })
        else {
            self.selected_shop = None;
            return;
        };
        if let Some(currency) = self.selected_currency {
            if currency >= currency_count {
                self.selected_currency = None;
                self.selected_shop_item = None;
            }
        }
        if let Some(item) = self.selected_shop_item {
            if item >= item_count {
                self.selected_shop_item = None;
            }
        }
        let specs = self.shop_fields.clone();
        self.refresh_specs(ContentFile::Shops, index, &specs, cx);
        self.refresh_currency_fields(cx);
        self.refresh_shop_item_fields(cx);
    }

    fn refresh_currency_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some((shop_index, currency_index)) = self.selected_shop.zip(self.selected_currency)
        else {
            return;
        };
        let Some(currency) = self
            .project
            .as_ref()
            .and_then(|project| project.shops().npcs.get(shop_index))
            .and_then(|npc| npc.currencies.get(currency_index))
            .cloned()
        else {
            self.selected_currency = None;
            return;
        };
        Self::put_text(&self.curr_currency, currency.currency, cx);
        Self::put_text(&self.curr_rate, currency.rate.to_string(), cx);
        self.refresh_shop_item_fields(cx);
    }

    fn refresh_shop_item_fields(&mut self, cx: &mut Context<'_, Self>) {
        let Some(((shop_index, currency_index), item_index)) = self
            .selected_shop
            .zip(self.selected_currency)
            .zip(self.selected_shop_item)
        else {
            return;
        };
        let Some(item) = self
            .project
            .as_ref()
            .and_then(|project| project.shops().npcs.get(shop_index))
            .and_then(|npc| npc.currencies.get(currency_index))
            .and_then(|currency| currency.items.get(item_index))
            .cloned()
        else {
            self.selected_shop_item = None;
            return;
        };
        Self::put_text(&self.shop_item_id, item, cx);
    }

    fn select_currency(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_currency = Some(index);
        self.selected_shop_item = None;
        self.refresh_currency_fields(cx);
        cx.notify();
    }

    fn select_shop_item(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.selected_shop_item = Some(index);
        self.refresh_shop_item_fields(cx);
        cx.notify();
    }

    fn apply_currency(&mut self, cx: &mut Context<'_, Self>) {
        let Some((shop_index, currency_index)) = self.selected_shop.zip(self.selected_currency)
        else {
            self.status = "не выбрана валютная вкладка".into();
            cx.notify();
            return;
        };
        let currency = self.field_text(&self.curr_currency, cx);
        let rate_text = self.field_text(&self.curr_rate, cx);
        let rate = match rate_text.trim() {
            "" => 1.0,
            value => match value.parse::<f64>() {
                Ok(number) if number > 0.0 && number.is_finite() => number,
                _ => {
                    self.status = format!("rate: нужно положительное число (получено «{value}»)");
                    cx.notify();
                    return;
                }
            },
        };
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.update_shop_currency(
            shop_index,
            currency_index,
            vec![
                ("currency", Value::from(currency)),
                ("rate", Value::from(rate)),
            ],
        ) {
            Ok(()) => self.status = "валюта применена (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_shop_fields(cx);
        cx.notify();
    }

    fn apply_shop_item(&mut self, cx: &mut Context<'_, Self>) {
        let Some(((shop_index, currency_index), item_index)) = self
            .selected_shop
            .zip(self.selected_currency)
            .zip(self.selected_shop_item)
        else {
            self.status = "не выбран товар".into();
            cx.notify();
            return;
        };
        let item = self.field_text(&self.shop_item_id, cx);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let result = if item.trim().is_empty() {
            project.remove_shop_item(shop_index, currency_index, item_index)
        } else {
            project.update_shop_item(
                shop_index,
                currency_index,
                item_index,
                item.trim().to_string(),
            )
        };
        match result {
            Ok(()) => self.status = "товар применён (Ctrl+S — сохранить)".into(),
            Err(err) => self.status = err.to_string(),
        }
        self.refresh_shop_fields(cx);
        cx.notify();
    }

    fn add_currency_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(shop_index) = self.selected_shop else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_shop_currency(shop_index)
        };
        match result {
            Ok(index) => {
                self.selected_currency = Some(index);
                self.selected_shop_item = None;
                self.refresh_shop_fields(cx);
                self.status = "валютная вкладка добавлена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_currency_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((shop_index, currency_index)) = self.selected_shop.zip(self.selected_currency)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_shop_currency(shop_index, currency_index)
        };
        match result {
            Ok(()) => {
                self.selected_currency = None;
                self.selected_shop_item = None;
                self.refresh_shop_fields(cx);
                self.status = "валютная вкладка удалена".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn add_shop_item_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some((shop_index, currency_index)) = self.selected_shop.zip(self.selected_currency)
        else {
            self.status = "сначала выберите валютную вкладку".into();
            cx.notify();
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_shop_item(shop_index, currency_index)
        };
        match result {
            Ok(index) => {
                self.selected_shop_item = Some(index);
                self.refresh_shop_fields(cx);
                self.status = "товар добавлен".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn delete_shop_item_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(((shop_index, currency_index), item_index)) = self
            .selected_shop
            .zip(self.selected_currency)
            .zip(self.selected_shop_item)
        else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_shop_item(shop_index, currency_index, item_index)
        };
        match result {
            Ok(()) => {
                self.selected_shop_item = None;
                self.refresh_shop_fields(cx);
                self.status = "товар удалён".into();
            }
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    // ─── E5.2: инспекторы диалогов и магазинов ───────────────────────────

    fn dialogs_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        if self.picker.is_some() {
            return self.picker_panel(cx);
        }
        let mut panel = self.inspector_panel("inspector-dialogs");
        let Some(project) = self.project.as_ref() else {
            return panel
                .child(
                    div().text_color(rgb(ERR)).child(
                        self.open_error
                            .clone()
                            .unwrap_or_else(|| "проект не открыт".into()),
                    ),
                )
                .into_any_element();
        };
        let Some(index) = self.selected_dialog else {
            return panel
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("Выберите диалог в списке (E5.2: строки, реплики, обучение)."),
                )
                .into_any_element();
        };
        let Some(npc) = project.dialogs().npcs.get(index) else {
            return panel
                .child(div().text_color(rgb(ERR)).child("диалог исчез"))
                .into_any_element();
        };
        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · диалог", npc.id)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("root — id стартовой строки; Enter в поле — применить формы"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.spec_rows(FieldGroup::Dialogs, cx)),
            )
            .child(self.record_buttons(FieldGroup::Dialogs, cx))
            .child(self.actions_row(cx, project.can_undo(), project.can_redo()));

        // ── обучение ──
        let mut training = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Обучение ({})", npc.training.len())),
                )
                .child(
                    self.button("btn-add-training", "+ тренировка")
                        .on_click(cx.listener(|this, _, _, cx| this.add_training_click(cx))),
                ),
        );
        for (entry_index, entry) in npc.training.iter().enumerate() {
            let row = div()
                .id(("training-row", entry_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.selected_training == Some(entry_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(cx.listener(move |this, _, _, cx| this.select_training(entry_index, cx)))
                .child(div().flex_1().text_size(px(12.0)).child(format!(
                    "{} · {} очк. · {} зол.",
                    entry.skill, entry.points, entry.gold
                )))
                .child(
                    div()
                        .id(("training-del", entry_index))
                        .px_1()
                        .cursor_pointer()
                        .text_color(rgb(ERR))
                        .text_size(px(13.0))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| this.delete_training_click(cx))),
                );
            training = training.child(row);
        }
        if self.selected_training.is_some() {
            training = training
                .child(self.text_field("skill", &self.train_skill))
                .child(self.text_field("points (целое)", &self.train_points))
                .child(self.text_field("gold (целое)", &self.train_gold))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("pick-training-skill")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(BORDER))
                                .bg(rgb(PANEL_ALT))
                                .text_size(px(11.0))
                                .cursor_pointer()
                                .child("… навык")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(target) = this.train_skill.clone() {
                                        this.open_picker(RefKind::Skill, target, cx);
                                    }
                                })),
                        )
                        .child(
                            self.button("btn-apply-training", "Применить обучение")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_training(cx))),
                        ),
                );
        }
        panel = panel.child(training);

        // ── строки диалога ──
        let mut lines = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Строки ({})", npc.lines.len())),
                )
                .child(
                    self.button("btn-add-line", "+ строка")
                        .on_click(cx.listener(|this, _, _, cx| this.add_dialog_line_click(cx))),
                ),
        );
        for (line_index, line) in npc.lines.iter().enumerate() {
            let row = div()
                .id(("dialog-line-row", line_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.selected_dialog_line == Some(line_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_dialog_line(line_index, cx)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(line.id.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(clip(&line.text, 46)),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(format!("{} отв.", line.replies.len())),
                )
                .child(
                    div()
                        .id(("dialog-line-del", line_index))
                        .px_1()
                        .cursor_pointer()
                        .text_color(rgb(ERR))
                        .text_size(px(13.0))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| this.delete_dialog_line_click(cx))),
                );
            lines = lines.child(row);
        }
        if self.selected_dialog_line.is_some() {
            lines = lines
                .child(self.text_field("id строки", &self.line_id))
                .child(self.text_field("текст", &self.line_text))
                .child(self.text_field("quest (id квеста)", &self.line_quest))
                .child(self.text_field("every_s (целое)", &self.line_every))
                .child(self.text_field("when — JSON", &self.line_when))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("pick-line-quest")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(BORDER))
                                .bg(rgb(PANEL_ALT))
                                .text_size(px(11.0))
                                .cursor_pointer()
                                .child("… квест")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(target) = this.line_quest.clone() {
                                        this.open_picker(RefKind::Quest, target, cx);
                                    }
                                })),
                        )
                        .child(
                            self.button("btn-apply-line", "Применить строку")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_dialog_line(cx))),
                        ),
                );

            // реплики выбранной строки
            if let Some(line) = self
                .selected_dialog_line
                .and_then(|line_index| npc.lines.get(line_index))
            {
                let mut replies = div().flex().flex_col().gap_1().child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(MUTED))
                                .child(format!("Реплики ({})", line.replies.len())),
                        )
                        .child(
                            self.button("btn-add-reply", "+ реплика")
                                .on_click(cx.listener(|this, _, _, cx| this.add_reply_click(cx))),
                        ),
                );
                for (reply_index, reply) in line.replies.iter().enumerate() {
                    let row = div()
                        .id(("reply-row", reply_index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(self.selected_dialog_reply == Some(reply_index), |d| {
                            d.bg(rgb(PANEL_ALT))
                        })
                        .hover(|d| d.bg(rgb(PANEL_ALT)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_dialog_reply(reply_index, cx)
                        }))
                        .child(
                            div()
                                .w(px(90.0))
                                .text_size(px(11.0))
                                .text_color(rgb(MUTED))
                                .child(reply.id.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.0))
                                .child(clip(&reply.text, 36)),
                        )
                        .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                            match (&reply.next, &reply.action) {
                                (_, Some(action)) => format!("→ {action}"),
                                (Some(next), None) => format!("→ {next}"),
                                (None, None) => "—".into(),
                            },
                        ))
                        .child(
                            div()
                                .id(("reply-del", reply_index))
                                .px_1()
                                .cursor_pointer()
                                .text_color(rgb(ERR))
                                .text_size(px(13.0))
                                .child("×")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.delete_reply_click(cx)),
                                ),
                        );
                    replies = replies.child(row);
                }
                if self.selected_dialog_reply.is_some() {
                    replies = replies
                        .child(self.text_field("id реплики", &self.reply_id))
                        .child(self.text_field("текст", &self.reply_text))
                        .child(self.text_field("next (id строки)", &self.reply_next))
                        .child(self.text_field("action (shop/train)", &self.reply_action))
                        .child(self.text_field("when — JSON", &self.reply_when))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id("pick-reply-next")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(BORDER))
                                        .bg(rgb(PANEL_ALT))
                                        .text_size(px(11.0))
                                        .cursor_pointer()
                                        .child("… строка")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(target) = this.reply_next.clone() {
                                                this.open_picker(RefKind::DialogLine, target, cx);
                                            }
                                        })),
                                )
                                .child(
                                    self.button("btn-apply-reply", "Применить реплику")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.apply_dialog_reply(cx)
                                        })),
                                ),
                        );
                }
                lines = lines.child(replies);
            }
        }
        panel = panel.child(lines);
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("! {warning}")),
            );
        }
        panel.into_any_element()
    }

    fn shops_inspector(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        if self.picker.is_some() {
            return self.picker_panel(cx);
        }
        let mut panel = self.inspector_panel("inspector-shops");
        let Some(project) = self.project.as_ref() else {
            return panel
                .child(
                    div().text_color(rgb(ERR)).child(
                        self.open_error
                            .clone()
                            .unwrap_or_else(|| "проект не открыт".into()),
                    ),
                )
                .into_any_element();
        };
        let Some(index) = self.selected_shop else {
            return panel
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("Выберите магазин в списке (E5.2: валюты и товары)."),
                )
                .into_any_element();
        };
        let Some(npc) = project.shops().npcs.get(index) else {
            return panel
                .child(div().text_color(rgb(ERR)).child("магазин исчез"))
                .into_any_element();
        };
        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · магазин", npc.id)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.spec_rows(FieldGroup::Shops, cx)),
            )
            .child(self.record_buttons(FieldGroup::Shops, cx))
            .child(self.actions_row(cx, project.can_undo(), project.can_redo()));

        // ── валюты ──
        let mut currencies = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Валюты ({})", npc.currencies.len())),
                )
                .child(
                    self.button("btn-add-currency", "+ валюта")
                        .on_click(cx.listener(|this, _, _, cx| this.add_currency_click(cx))),
                ),
        );
        for (currency_index, currency) in npc.currencies.iter().enumerate() {
            let row = div()
                .id(("currency-row", currency_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.selected_currency == Some(currency_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_currency(currency_index, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(format!("{} · rate {}", currency.currency, currency.rate)),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(format!("{} товаров", currency.items.len())),
                )
                .child(
                    div()
                        .id(("currency-del", currency_index))
                        .px_1()
                        .cursor_pointer()
                        .text_color(rgb(ERR))
                        .text_size(px(13.0))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| this.delete_currency_click(cx))),
                );
            currencies = currencies.child(row);
        }
        if self.selected_currency.is_some() {
            currencies = currencies
                .child(self.text_field("currency (id предмета)", &self.curr_currency))
                .child(self.text_field("rate (число)", &self.curr_rate))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("pick-currency")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(BORDER))
                                .bg(rgb(PANEL_ALT))
                                .text_size(px(11.0))
                                .cursor_pointer()
                                .child("… предмет")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(target) = this.curr_currency.clone() {
                                        this.open_picker(RefKind::Item, target, cx);
                                    }
                                })),
                        )
                        .child(
                            self.button("btn-apply-currency", "Применить валюту")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_currency(cx))),
                        ),
                );

            // товары выбранной валюты
            if let Some(currency) = self
                .selected_currency
                .and_then(|currency_index| npc.currencies.get(currency_index))
            {
                let mut items =
                    div().flex().flex_col().gap_1().child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(MUTED))
                                    .child(format!("Товары ({})", currency.items.len())),
                            )
                            .child(self.button("btn-add-shop-item", "+ товар").on_click(
                                cx.listener(|this, _, _, cx| this.add_shop_item_click(cx)),
                            )),
                    );
                for (item_index, item) in currency.items.iter().enumerate() {
                    let row =
                        div()
                            .id(("shop-item-row", item_index))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.selected_shop_item == Some(item_index), |d| {
                                d.bg(rgb(PANEL_ALT))
                            })
                            .hover(|d| d.bg(rgb(PANEL_ALT)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_shop_item(item_index, cx)
                            }))
                            .child(div().flex_1().text_size(px(12.0)).child(item.clone()))
                            .child(
                                div()
                                    .id(("shop-item-del", item_index))
                                    .px_1()
                                    .cursor_pointer()
                                    .text_color(rgb(ERR))
                                    .text_size(px(13.0))
                                    .child("×")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.delete_shop_item_click(cx)
                                    })),
                            );
                    items = items.child(row);
                }
                if self.selected_shop_item.is_some() {
                    items = items
                        .child(self.text_field("id товара", &self.shop_item_id))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id("pick-shop-item")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(BORDER))
                                        .bg(rgb(PANEL_ALT))
                                        .text_size(px(11.0))
                                        .cursor_pointer()
                                        .child("… предмет")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(target) = this.shop_item_id.clone() {
                                                this.open_picker(RefKind::Item, target, cx);
                                            }
                                        })),
                                )
                                .child(
                                    self.button("btn-apply-shop-item", "Применить товар")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.apply_shop_item(cx)),
                                        ),
                                ),
                        );
                }
                currencies = currencies.child(items);
            }
        }
        panel = panel.child(currencies);
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("! {warning}")),
            );
        }
        panel.into_any_element()
    }

    // ─── E6: быстрый переход, справка, seed ─────────────────────────────

    /// Все записи контента для палитры быстрого перехода.
    fn quick_entries(&self) -> Vec<QuickEntry> {
        let mut entries = Vec::new();
        for section in [
            Section::Map,
            Section::Locations,
            Section::Items,
            Section::Dialogs,
            Section::Shops,
            Section::Quests,
            Section::Recipes,
            Section::Skills,
            Section::Professions,
            Section::Help,
        ] {
            entries.push(QuickEntry {
                section,
                index: None,
                label: format!("Раздел · {}", section.title()),
            });
        }
        let Some(project) = self.project.as_ref() else {
            return entries;
        };
        let content = project.content();
        for (index, record) in content.locations.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Locations,
                index: Some(index),
                label: format!("Локация · {} — {}", record.id, record.name),
            });
        }
        for (index, record) in content.items.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Items,
                index: Some(index),
                label: format!("Предмет · {} — {}", record.id, record.name),
            });
        }
        for (index, record) in project.dialogs().npcs.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Dialogs,
                index: Some(index),
                label: format!("Диалог · {}", record.id),
            });
        }
        for (index, record) in project.shops().npcs.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Shops,
                index: Some(index),
                label: format!("Магазин · {}", record.id),
            });
        }
        for (index, record) in content.quests.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Quests,
                index: Some(index),
                label: format!("Квест · {} — {}", record.id, record.title),
            });
        }
        for (index, record) in content.recipes.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Recipes,
                index: Some(index),
                label: format!("Рецепт · {} — {}", record.id, record.name),
            });
        }
        for (index, record) in content.skills.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Skills,
                index: Some(index),
                label: format!("Навык · {} — {}", record.id, record.name),
            });
        }
        for (index, record) in content.professions.iter().enumerate() {
            entries.push(QuickEntry {
                section: Section::Professions,
                index: Some(index),
                label: format!("Профессия · {} — {}", record.id, record.name),
            });
        }
        entries
    }

    /// Отфильтрованный список палитры (до 40 строк).
    fn quick_matches(&self) -> Vec<QuickEntry> {
        let filter = self.quick_filter.trim().to_lowercase();
        self.quick_entries()
            .into_iter()
            .filter(|entry| filter.is_empty() || entry.label.to_lowercase().contains(&filter))
            .take(40)
            .collect()
    }

    fn quick_move(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        let count = self.quick_matches().len();
        if count == 0 {
            return;
        }
        let current = self.quick_selected.min(count - 1) as isize;
        self.quick_selected = (current + delta).clamp(0, count as isize - 1) as usize;
        cx.notify();
    }

    fn quick_activate_selected(&mut self, cx: &mut Context<'_, Self>) {
        let matches = self.quick_matches();
        match matches.get(self.quick_selected).cloned() {
            Some(entry) => self.quick_activate(entry, cx),
            None => {
                self.quick_open = false;
                cx.notify();
            }
        }
    }

    fn quick_activate(&mut self, entry: QuickEntry, cx: &mut Context<'_, Self>) {
        self.quick_open = false;
        self.quick_filter.clear();
        self.quick_selected = 0;
        self.switch_section(entry.section, cx);
        if let Some(index) = entry.index {
            match entry.section {
                Section::Items => self.select_item(index, cx),
                Section::Locations => self.select_location(index, cx),
                _ => self.select_record(entry.section, index, cx),
            }
            self.status = format!("переход: {}", entry.label);
            cx.notify();
        }
    }

    fn quick_palette(&self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        if !self.quick_open {
            return div().into_any_element();
        }
        let matches = self.quick_matches();
        let count = matches.len();
        let mut rows = div()
            .id("quick-rows")
            .flex()
            .flex_col()
            .gap_0p5()
            .max_h(px(420.0))
            .overflow_y_scroll();
        for (row, entry) in matches.into_iter().enumerate() {
            let selected = row == self.quick_selected;
            let id = format!("quick-row-{row}");
            let mut item = div()
                .id(SharedString::from(id))
                .px_3()
                .py_1()
                .rounded_md()
                .text_size(px(12.0))
                .cursor_pointer()
                .child(entry.label.clone());
            if selected {
                item = item.bg(rgb(ACCENT)).text_color(rgb(0x101010));
            } else {
                item = item.hover(|style| style.bg(rgb(PANEL_ALT)));
            }
            rows = rows.child(item.on_click(
                cx.listener(move |this, _, _, cx| this.quick_activate(entry.clone(), cx)),
            ));
        }
        let filter_color = if self.quick_filter.is_empty() {
            rgb(MUTED)
        } else {
            rgb(TEXT)
        };
        let filter = if self.quick_filter.is_empty() {
            "начните вводить id или имя".to_string()
        } else {
            self.quick_filter.clone()
        };
        let panel = div()
            .id("quick-panel")
            .w(px(620.0))
            .mt(px(90.0))
            .p_3()
            .rounded_lg()
            .border(px(1.0))
            .border_color(rgb(ACCENT))
            .bg(rgb(PANEL))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(13.0)).child("Быстрый переход"))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(format!("{count} записей")),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(PANEL_ALT))
                    .text_size(px(12.0))
                    .text_color(filter_color)
                    .child(filter),
            )
            .child(rows)
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("↑↓ выбор · Enter — перейти · Esc — закрыть"),
            );
        div()
            .id("quick-overlay")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .justify_center()
            .items_start()
            .child(panel)
            .into_any_element()
    }

    fn help_body(&self) -> impl IntoElement {
        let row = |key: &'static str, text: &'static str| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(190.0))
                        .text_size(px(12.0))
                        .text_color(rgb(ACCENT))
                        .child(key),
                )
                .child(div().flex_1().text_size(px(12.0)).child(text))
        };
        div()
            .id("help-scroll")
            .flex_1()
            .min_h_0()
            .p_5()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scroll()
            .child(div().text_size(px(20.0)).child("ld2-editor — помощь"))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED))
                    .child("Контент — файлы content/*.json; скрытого состояния нет, всё хранится в git."),
            )
            .child(div().mt_3().text_size(px(14.0)).child("Горячие клавиши"))
            .child(row("Ctrl/Cmd + P", "быстрый переход по разделам и записям"))
            .child(row("F1", "эта справка"))
            .child(row("Ctrl/Cmd + S", "сохранить изменённые файлы"))
            .child(row("Ctrl/Cmd + Z", "отменить последнюю правку"))
            .child(row("Ctrl/Cmd + Shift + Z / Y", "повторить правку"))
            .child(row("Ctrl/Cmd + R", "перечитать файлы с диска"))
            .child(row("Карта: стрелки, +/- и L", "сдвинуть узел, зум, режим связей"))
            .child(div().mt_3().text_size(px(14.0)).child("Поток работы"))
            .child(
                div()
                    .text_size(px(12.0))
                    .child("1. Правьте контент в разделах; сохранение — кнопка «Сохранить» или Ctrl+S."),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .child("2. Красные ошибки блокируют сохранение; жёлтые предупреждения — висячие ссылки, их можно оставить."),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .child("3. Отправьте изменённые файлы в git и примените на сервере кнопкой Seed (или `ld2-server seed`)."),
            )
            .child(div().mt_3().text_size(px(14.0)).child("Ограничения"))
            .child(
                div()
                    .text_size(px(12.0))
                    .child("Порядок ключей сохраняется; незнакомые поля не теряются."),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .child("Seed использует конфиг по умолчанию; свой — LD2_EDITOR_SEED_CMD или `ld2-server --config … seed`."),
            )
    }

    fn seed_click(&mut self, cx: &mut Context<'_, Self>) {
        if self.seed_running {
            return;
        }
        let Some(project) = self.project.as_ref() else {
            self.status = "seed: проект не открыт".into();
            cx.notify();
            return;
        };
        let root = project.root().to_path_buf();
        self.seed_running = true;
        self.seed_log = Some("запуск…".into());
        self.status = "seed: запуск ld2-server seed…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let command = std::env::var("LD2_EDITOR_SEED_CMD")
                .unwrap_or_else(|_| "cargo run -q -p ld2-server -- seed".to_string());
            let output = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(windows)]
                    let result = std::process::Command::new("cmd")
                        .args(["/C", &command])
                        .current_dir(&root)
                        .output();
                    #[cfg(not(windows))]
                    let result = std::process::Command::new("sh")
                        .args(["-c", &command])
                        .current_dir(&root)
                        .output();
                    result
                })
                .await;
            let text = match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    let code = out.status.code().unwrap_or(-1);
                    let mut text = format!("seed: код {code}");
                    if !stdout.is_empty() {
                        text.push_str(" · ");
                        text.push_str(&stdout);
                    }
                    if !stderr.is_empty() {
                        text.push_str(" · ");
                        text.push_str(&stderr);
                    }
                    text
                }
                Err(err) => format!("seed: ошибка запуска: {err}"),
            };
            this.update(cx, |this, cx| {
                this.seed_running = false;
                this.seed_log = Some(text.clone());
                this.status = text;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rail(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let active = self.section;
        let mut rail = div()
            .w(px(150.0))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_r_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL));
        for target in [
            Section::Map,
            Section::Locations,
            Section::Items,
            Section::Dialogs,
            Section::Shops,
            Section::Quests,
            Section::Recipes,
            Section::Skills,
            Section::Professions,
            Section::Help,
        ] {
            let id = match target {
                Section::Map => "section-map",
                Section::Locations => "section-locations",
                Section::Items => "section-items",
                Section::Dialogs => "section-dialogs",
                Section::Shops => "section-shops",
                Section::Quests => "section-quests",
                Section::Recipes => "section-recipes",
                Section::Skills => "section-skills",
                Section::Professions => "section-professions",
                Section::Help => "section-help",
            };
            let mut item = div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .child(target.title());
            if active == target {
                item = item.bg(rgb(ACCENT)).text_color(rgb(0x101010));
            }
            rail = rail.child(
                item.on_click(cx.listener(move |this, _, _, cx| this.switch_section(target, cx))),
            );
        }
        rail.child(
            div()
                .mt_2()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child("E0–E4: каркас,\nконтент, предметы,\nлокации, карта.\nE5: диалоги, магазины,\nквесты, рецепты,\nнавыки, профессии.\nE6: переход, справка,\nseed."),
        )
    }

    fn items_list(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let filter = self.filter.trim().to_lowercase();
        let mut rows: Vec<(usize, SharedString, SharedString, SharedString)> = Vec::new();
        if let Some(project) = self.project.as_ref() {
            for (index, item) in project.content().items.iter().enumerate() {
                if !filter.is_empty()
                    && !item.id.to_lowercase().contains(&filter)
                    && !item.name.to_lowercase().contains(&filter)
                {
                    continue;
                }
                rows.push((
                    index,
                    SharedString::from(item.id.clone()),
                    SharedString::from(item.name.clone()),
                    SharedString::from(item.rarity.clone()),
                ));
            }
        }
        let count = rows.len();
        let selected = self.selected_item;
        let mut list = div()
            .id("items-list")
            .flex_1()
            .overflow_y_scroll()
            .min_h_0();
        for (index, id, name, rarity) in rows {
            list = list.child(
                div()
                    .id(("item-row", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected == Some(index), |d| d.bg(rgb(PANEL_ALT)))
                    .hover(|d| d.bg(rgb(PANEL_ALT)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_item(index, cx)))
                    .child(
                        div()
                            .w(px(130.0))
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(id),
                    )
                    .child(div().flex_1().text_size(px(12.0)).child(name))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(rarity),
                    ),
            );
        }
        div()
            .w(px(400.0))
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(Input::new(
                        self.search.as_ref().expect("поле поиска создано"),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED))
                                    .child(format!("{count} предметов")),
                            )
                            .child(
                                self.button("btn-add-item", "+ предмет").on_click(
                                    cx.listener(|this, _, _, cx| this.add_item_click(cx)),
                                ),
                            ),
                    ),
            )
            .child(list)
    }

    fn locations_list(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let filter = self.filter.trim().to_lowercase();
        let mut rows: Vec<(usize, SharedString, SharedString, SharedString)> = Vec::new();
        if let Some(project) = self.project.as_ref() {
            for (index, location) in project.content().locations.iter().enumerate() {
                if !filter.is_empty()
                    && !location.id.to_lowercase().contains(&filter)
                    && !location.name.to_lowercase().contains(&filter)
                {
                    continue;
                }
                rows.push((
                    index,
                    SharedString::from(location.id.clone()),
                    SharedString::from(location.name.clone()),
                    SharedString::from(format!(
                        "→{} · ⚔{}",
                        location.exits.len(),
                        location.entities.len()
                    )),
                ));
            }
        }
        let count = rows.len();
        let total = self
            .project
            .as_ref()
            .map(|project| project.content().locations.len())
            .unwrap_or(0);
        let selected = self.selected_location;
        let this = cx.entity();
        // Виртуализация (E6.1): рисуется только видимый диапазон строк —
        // 1189 локаций ведут себя как ~20. Строки фиксированной высоты,
        // длинные id/имена обрезаются многоточием.
        let list = if count == 0 {
            div()
                .flex_1()
                .min_h_0()
                .px_2()
                .py_2()
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .child("ничего не найдено — измените поиск")
                .into_any_element()
        } else {
            let rows = std::rc::Rc::new(rows);
            let row_data = rows.clone();
            uniform_list("locations-rows", count, move |range, _window, _acx| {
                range
                    .map(|row| {
                        let (index, id, name, meta) = row_data[row].clone();
                        let this = this.clone();
                        div()
                            .id(("location-row", index))
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(26.0))
                            .px_2()
                            .cursor_pointer()
                            .when(selected == Some(index), |d| d.bg(rgb(PANEL_ALT)))
                            .hover(|d| d.bg(rgb(PANEL_ALT)))
                            .on_click(move |_, _, acx: &mut gpui::App| {
                                this.update(acx, |this, cx| this.select_location(index, cx));
                            })
                            .child(
                                div()
                                    .w(px(150.0))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED))
                                    .child(id),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(12.0))
                                    .child(name),
                            )
                            .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(meta))
                    })
                    .collect::<Vec<gpui::Stateful<gpui::Div>>>()
            })
            .track_scroll(&self.locations_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let counter = if count == total {
            format!("{count} локаций")
        } else {
            format!("{count} из {total} локаций")
        };
        div()
            .w(px(400.0))
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(Input::new(
                        self.search.as_ref().expect("поле поиска создано"),
                    ))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(counter),
                    ),
            )
            .child(list)
    }

    fn actions_row(
        &self,
        cx: &mut Context<'_, Self>,
        can_undo: bool,
        can_redo: bool,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .mt_1()
            .child(
                self.button("btn-apply", "Применить")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_current(cx))),
            )
            .child(
                self.button("btn-save", "Сохранить")
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
            .child(
                self.button("btn-undo", "Undo")
                    .when(!can_undo, |d| d.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
            )
            .child(
                self.button("btn-redo", "Redo")
                    .when(!can_redo, |d| d.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| this.redo(cx))),
            )
            .child(
                self.button("btn-reload", "Перечитать")
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }

    fn apply_current(&mut self, cx: &mut Context<'_, Self>) {
        let kind = match self.section {
            Section::Items => ApplyKind::Item,
            Section::Locations if self.selected_entity.is_some() => ApplyKind::Entity,
            Section::Locations if self.selected_exit.is_some() => ApplyKind::Exit,
            Section::Locations => ApplyKind::Location,
            Section::Quests => ApplyKind::Quests,
            Section::Recipes => ApplyKind::Recipes,
            Section::Skills => ApplyKind::Skills,
            Section::Professions => ApplyKind::Professions,
            Section::Dialogs => ApplyKind::Dialogs,
            Section::Shops => ApplyKind::Shops,
            Section::Help => {
                self.status = "справка: формы нет".into();
                cx.notify();
                return;
            }
            Section::Map => {
                self.status = "карта: формы нет — выберите узел и «В форму»".into();
                cx.notify();
                return;
            }
        };
        self.apply(kind, cx);
    }

    // ─── E6.2: структурный редактор предмета ─────────────────────────────

    fn item_set_select(
        &mut self,
        field: &'static str,
        value: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_field(index, field, value.map(Value::String))
        };
        self.item_apply_result(result, "выбор применён", cx);
    }

    fn item_set_stat_select(
        &mut self,
        stat: &'static str,
        value: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_stat(index, stat, value.map(Value::String))
        };
        self.item_apply_result(result, "характеристика применена", cx);
    }

    fn item_set_buff_select(
        &mut self,
        slot: &'static str,
        key: &'static str,
        value: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_buff_field(index, slot, key, value.map(Value::String))
        };
        self.item_apply_result(result, "бафф применён", cx);
    }

    fn item_set_effect_kind(&mut self, value: Option<String>, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_effect_field(index, "kind", value.map(Value::String))
        };
        self.item_apply_result(result, "вид эффекта применён", cx);
    }

    fn item_set_bonus_select(
        &mut self,
        key: &'static str,
        value: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(index), Some(bonus)) = (self.selected_item, self.selected_bonus) else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_bonus_field(index, bonus, key, value.map(Value::String))
        };
        self.item_apply_result(result, "бонус применён", cx);
    }

    fn item_set_requires_profession(&mut self, value: Option<String>, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_requires_field(index, "profession", value.map(Value::String))
        };
        self.item_apply_result(result, "требование применено", cx);
    }

    fn item_toggle_param_pct(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(index), Some(param)) = (self.selected_item, self.selected_param.clone()) else {
            self.status = "параметр не выбран".into();
            cx.notify();
            return;
        };
        let current = self
            .project
            .as_ref()
            .and_then(|project| project.content().items.get(index))
            .and_then(|item| item.stats.get("param"))
            .and_then(|params| params.get(&param))
            .cloned();
        let (value, to_pct) = match current {
            Some(Value::Object(map)) => (
                map.get("value").and_then(Value::as_i64).unwrap_or(1),
                !map.get("pct").and_then(Value::as_bool).unwrap_or(false),
            ),
            Some(value) => (value.as_i64().unwrap_or(1), true),
            None => (1, true),
        };
        let value = if to_pct {
            serde_json::json!({"value": value, "pct": true})
        } else {
            Value::from(value)
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_param(index, &param, Some(value))
        };
        self.item_apply_result(result, "pct параметра переключён", cx);
    }

    fn item_toggle_buff_pct(&mut self, slot: &'static str, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let enabled = self
            .project
            .as_ref()
            .and_then(|project| project.content().items.get(index))
            .and_then(|item| item.stats.get(slot))
            .and_then(|buff| buff.get("pct"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let value = if enabled {
            None
        } else {
            Some(Value::Bool(true))
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_buff_field(index, slot, "pct", value)
        };
        self.item_apply_result(result, "pct баффа переключён", cx);
    }

    fn item_toggle_bonus_pct(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(index), Some(bonus)) = (self.selected_item, self.selected_bonus) else {
            return;
        };
        let enabled = self
            .project
            .as_ref()
            .and_then(|project| project.item_bonuses(index).get(bonus).cloned())
            .and_then(|bonus| bonus.get("pct").and_then(Value::as_bool))
            .unwrap_or(false);
        let value = if enabled {
            None
        } else {
            Some(Value::Bool(true))
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_bonus_field(index, bonus, "pct", value)
        };
        self.item_apply_result(result, "pct бонуса переключён", cx);
    }

    fn item_add_buff(&mut self, slot: &'static str, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            let mut result = Ok(());
            for (key, value) in [
                ("param", Value::from(ITEM_PARAMS[0])),
                ("value", Value::from(1)),
                ("duration_s", Value::from(5)),
                ("max_stacks", Value::from(1)),
            ] {
                if let Err(err) = project.set_item_buff_field(index, slot, key, Some(value)) {
                    result = Err(err);
                    break;
                }
            }
            result
        };
        self.item_apply_result(result, "бафф добавлен", cx);
    }

    fn item_delete_buff(&mut self, slot: &'static str, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_item_buff(index, slot)
        };
        self.item_apply_result(result, "бафф удалён", cx);
    }

    fn item_add_effect(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            let mut result = Ok(());
            for (key, value) in [
                ("kind", Value::from(EFFECT_KINDS[0])),
                ("value", Value::from(1)),
                ("duration_s", Value::from(3)),
                ("period_s", Value::from(1)),
            ] {
                if let Err(err) = project.set_item_effect_field(index, key, Some(value)) {
                    result = Err(err);
                    break;
                }
            }
            result
        };
        self.item_apply_result(result, "эффект добавлен", cx);
    }

    fn item_delete_effect(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_item_effect(index)
        };
        self.item_apply_result(result, "эффект удалён", cx);
    }

    fn item_select_param(&mut self, param: String, cx: &mut Context<'_, Self>) {
        self.selected_param = Some(param);
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    fn item_add_param_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let added = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_item_param(index)
        };
        match added {
            Ok(param) => {
                self.selected_param = Some(param);
                self.item_apply_result(Ok(()), "параметр добавлен", cx);
            }
            Err(err) => self.item_apply_result(Err(err), "", cx),
        }
    }

    fn item_delete_param_click(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(index), Some(param)) = (self.selected_item, self.selected_param.clone()) else {
            self.status = "параметр не выбран".into();
            cx.notify();
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.set_item_param(index, &param, None)
        };
        if result.is_ok() {
            self.selected_param = None;
        }
        self.item_apply_result(result, "параметр удалён", cx);
    }

    fn item_select_bonus(&mut self, bonus: usize, cx: &mut Context<'_, Self>) {
        self.selected_bonus = Some(bonus);
        self.refresh_item_zone_fields(cx);
        cx.notify();
    }

    fn item_add_bonus_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let added = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_item_bonus(index)
        };
        match added {
            Ok(bonus) => {
                self.selected_bonus = Some(bonus);
                self.item_apply_result(Ok(()), "бонус добавлен", cx);
            }
            Err(err) => self.item_apply_result(Err(err), "", cx),
        }
    }

    fn item_delete_bonus_click(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(index), Some(bonus)) = (self.selected_item, self.selected_bonus) else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_item_bonus(index, bonus)
        };
        if result.is_ok() {
            self.selected_bonus = None;
        }
        self.item_apply_result(result, "бонус удалён", cx);
    }

    fn item_add_requires_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_item_requires(index)
        };
        self.item_apply_result(result, "требования добавлены", cx);
    }

    fn item_delete_requires_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_item_requires(index)
        };
        self.item_apply_result(result, "требования удалены", cx);
    }

    fn add_item_click(&mut self, cx: &mut Context<'_, Self>) {
        let added = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.add_item()
        };
        match added {
            Ok(index) => {
                self.selected_item = Some(index);
                self.selected_param = None;
                self.selected_bonus = None;
                self.refresh_item_fields(cx);
                self.status = "предмет добавлен".into();
                cx.notify();
            }
            Err(err) => {
                self.status = err.to_string();
                cx.notify();
            }
        }
    }

    fn delete_item_click(&mut self, cx: &mut Context<'_, Self>) {
        let Some(index) = self.selected_item else {
            self.status = "не выбран предмет".into();
            cx.notify();
            return;
        };
        let result = {
            let Some(project) = self.project.as_mut() else {
                return;
            };
            project.remove_item(index)
        };
        if result.is_ok() {
            self.selected_item = None;
            self.selected_param = None;
            self.selected_bonus = None;
        }
        match result {
            Ok(()) => self.status = "предмет удалён · Ctrl+S — сохранить".into(),
            Err(err) => self.status = err.to_string(),
        }
        cx.notify();
    }

    fn group_box(&self, title: &'static str, body: impl IntoElement) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(ACCENT))
                    .child(title),
            )
            .child(body)
    }

    fn field_cell(
        &self,
        label: &'static str,
        input: &Option<Entity<TextInput>>,
    ) -> impl IntoElement {
        div().w(px(190.0)).child(self.text_field(label, input))
    }

    fn select_control<H>(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        choices: Vec<Choice>,
        selected: Option<String>,
        handler: H,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement
    where
        H: Fn(&mut Self, Option<String>, &mut Context<'_, Self>) + 'static,
    {
        let mut select = Select::new(id, choices).placeholder("—");
        if let Some(value) = selected {
            select = select.selected(value);
        }
        div()
            .w(px(190.0))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(label),
            )
            .child(
                select.on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                    let value = value.to_string();
                    let value = if value.is_empty() { None } else { Some(value) };
                    handler(this, value, cx);
                })),
            )
    }

    fn chip(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(if active { ACCENT } else { BORDER }))
            .bg(rgb(if active { ACCENT } else { PANEL_ALT }))
            .text_color(rgb(if active { 0x101010 } else { TEXT }))
            .text_size(px(11.0))
            .cursor_pointer()
            .child(label)
    }

    fn items_inspector(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let mut panel = div()
            .id("inspector-items")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .overflow_y_scroll();
        let Some(project) = self.project.as_ref() else {
            return panel.child(
                div().text_color(rgb(ERR)).child(
                    self.open_error
                        .clone()
                        .unwrap_or_else(|| "проект не открыт".into()),
                ),
            );
        };
        let Some(index) = self.selected_item else {
            return panel.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("Выберите предмет в списке."),
            );
        };
        let Some(item) = project.content().items.get(index).cloned() else {
            return panel.child(div().text_color(rgb(ERR)).child("предмет исчез"));
        };
        let can_undo = project.can_undo();
        let can_redo = project.can_redo();
        let param_keys = project.item_param_keys(index);
        let bonuses = project.item_bonuses(index);
        let mut types: Vec<String> = project
            .content()
            .items
            .iter()
            .map(|item| item.item_type.clone())
            .collect();
        types.sort();
        types.dedup();
        let skill_ids: Vec<String> = project
            .content()
            .skills
            .iter()
            .map(|skill| skill.id.clone())
            .collect();
        let profession_ids: Vec<String> = project
            .content()
            .professions
            .iter()
            .map(|profession| profession.id.clone())
            .collect();
        let type_choices: Vec<Choice> = types
            .iter()
            .map(|item_type| Choice::new(item_type.clone(), item_type.clone()))
            .collect();
        let skill_refs: Vec<&str> = skill_ids.iter().map(String::as_str).collect();
        let profession_refs: Vec<&str> = profession_ids.iter().map(String::as_str).collect();
        let skill_choices = choices_with_none(&skill_refs);
        let profession_choices = choices_with_none(&profession_refs);

        // ── основное ──
        let mut main_row = div().flex().flex_wrap().gap_2();
        main_row = main_row
            .child(self.field_cell("id", &self.item_id))
            .child(self.field_cell("Имя", &self.item_name))
            .child(self.select_control(
                "item-type",
                "Тип",
                type_choices,
                Some(item.item_type.clone()),
                |this, value, cx| this.item_set_select("type", value, cx),
                cx,
            ))
            .child(self.select_control(
                "item-rarity",
                "Редкость",
                choices(&ITEM_RARITIES),
                Some(item.rarity.clone()),
                |this, value, cx| this.item_set_select("rarity", value, cx),
                cx,
            ))
            .child(self.select_control(
                "item-slot",
                "Слот",
                choices_with_none(&ITEM_SLOTS),
                item.slot.clone(),
                |this, value, cx| this.item_set_select("slot", value, cx),
                cx,
            ))
            .child(self.select_control(
                "item-skill",
                "Навык",
                skill_choices.clone(),
                item.skill.clone(),
                |this, value, cx| this.item_set_select("skill", value, cx),
                cx,
            ))
            .child(self.field_cell("Цена (золото)", &self.item_gold))
            .child(self.field_cell("Количество", &self.item_qty))
            .child(self.field_cell("Заточка (upgrade)", &self.item_upgrade))
            .child(self.field_cell("Сокетов", &self.item_sockets))
            .child(self.field_cell("Под руны", &self.item_rune_sockets))
            .child(self.field_cell("Рук", &self.item_hands))
            .child(self.field_cell("Кулдаун, с", &self.item_cooldown))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        self.button("btn-apply-item", "Применить основное")
                            .on_click(cx.listener(|this, _, _, cx| this.apply_item(cx))),
                    )
                    .child(
                        self.button("btn-delete-item", "Удалить предмет")
                            .on_click(cx.listener(|this, _, _, cx| this.delete_item_click(cx))),
                    ),
            );

        // ── характеристики (stats) ──
        let mut stats_row = div().flex().flex_wrap().gap_2();
        stats_row = stats_row
            .child(self.field_cell("Урон (dmg)", &self.stat_dmg))
            .child(self.field_cell("Броня (armor)", &self.stat_armor))
            .child(self.field_cell("Магия (magic)", &self.stat_magic))
            .child(self.field_cell("Лечение (heal)", &self.stat_heal))
            .child(self.field_cell("Качество (quality)", &self.stat_quality))
            .child(self.field_cell("Урон (damage)", &self.stat_damage))
            .child(self.field_cell("Шанс (chance)", &self.stat_chance))
            .child(self.field_cell("Заряды (charges)", &self.stat_charges))
            .child(self.field_cell("Макс. зарядов (max)", &self.stat_max))
            .child(self.field_cell("Стоимость каста (cost)", &self.stat_cost))
            .child(self.field_cell("Глиф (glyph)", &self.stat_glyph))
            .child(self.field_cell("Заклинание (spell)", &self.stat_spell))
            .child(self.field_cell("Значение (value)", &self.stat_value))
            .child(
                self.select_control(
                    "stat-school",
                    "Школа",
                    choices(&SPELL_SCHOOLS),
                    item.stats
                        .get("school")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    |this, value, cx| this.item_set_stat_select("school", value, cx),
                    cx,
                ),
            )
            .child(
                self.select_control(
                    "stat-class",
                    "Класс",
                    choices(&ITEM_CLASSES),
                    item.stats
                        .get("class")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    |this, value, cx| this.item_set_stat_select("class", value, cx),
                    cx,
                ),
            )
            .child(
                self.select_control(
                    "stat-attr",
                    "Атрибут",
                    choices(&ITEM_ATTRS),
                    item.stats
                        .get("attr")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    |this, value, cx| this.item_set_stat_select("attr", value, cx),
                    cx,
                ),
            )
            .child(
                div().flex().items_end().gap_2().child(
                    self.button("btn-apply-stats", "Применить характеристики")
                        .on_click(cx.listener(|this, _, _, cx| this.apply_item_stats(cx))),
                ),
            );

        // ── параметры stats.param ──
        let param_map = item
            .stats
            .get("param")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let selected_param = self
            .selected_param
            .clone()
            .filter(|param| param_map.contains_key(param));
        let param_pct = selected_param
            .as_ref()
            .and_then(|param| param_map.get(param))
            .and_then(|value| value.get("pct"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut params_body = div().flex().flex_col().gap_1();
        for (param, value) in param_map.iter() {
            let selected = selected_param.as_deref() == Some(param.as_str());
            let pct = value.get("pct").and_then(Value::as_bool).unwrap_or(false);
            let shown = text_value(value);
            let click_param = param.clone();
            let row_id = SharedString::from(format!("item-param-{param}"));
            params_body = params_body.child(
                div()
                    .id(row_id)
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(PANEL_ALT)))
                    .hover(|d| d.bg(rgb(PANEL_ALT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.item_select_param(click_param.clone(), cx)
                    }))
                    .child(div().w(px(200.0)).text_size(px(12.0)).child(param.clone()))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(shown),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(if pct { ACCENT } else { MUTED }))
                            .child(if pct { "pct" } else { "" }),
                    ),
            );
        }
        if let Some(param) = &selected_param {
            params_body =
                params_body.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap_2()
                        .child(self.field_cell("Значение", &self.param_value))
                        .child(
                            self.chip(
                                "chip-param-pct",
                                if param_pct { "pct ✓" } else { "pct" },
                                param_pct,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.item_toggle_param_pct(cx))),
                        )
                        .child(
                            self.button("btn-apply-param", "Применить")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_item_param(cx))),
                        )
                        .child(self.button("btn-del-param", "Удалить").on_click(
                            cx.listener(|this, _, _, cx| this.item_delete_param_click(cx)),
                        ))
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(MUTED))
                                .child(format!("param: {param}")),
                        ),
                );
        } else {
            params_body = params_body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("Выберите параметр или добавьте новый."),
            );
        }
        params_body = params_body.child(
            self.button("btn-add-param", "+ параметр")
                .on_click(cx.listener(|this, _, _, cx| this.item_add_param_click(cx))),
        );

        // ── автобаффы buff/buff2 ──
        let mut buffs_body = div().flex().flex_col().gap_2();
        for slot in ["buff", "buff2"] {
            let current = item.stats.get(slot).and_then(Value::as_object).cloned();
            let title = if slot == "buff" {
                "Автобафф (buff)"
            } else {
                "Автобафф 2 (buff2)"
            };
            let mut block = div().flex().flex_col().gap_2().child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(title),
            );
            match current {
                Some(map) => {
                    let param = map
                        .get("param")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let pct = map.get("pct").and_then(Value::as_bool).unwrap_or(false);
                    let value_input = if slot == "buff" {
                        &self.buff_value
                    } else {
                        &self.buff2_value
                    };
                    let duration_input = if slot == "buff" {
                        &self.buff_duration
                    } else {
                        &self.buff2_duration
                    };
                    let stacks_input = if slot == "buff" {
                        &self.buff_stacks
                    } else {
                        &self.buff2_stacks
                    };
                    let apply_button = if slot == "buff" {
                        self.button("btn-apply-buff", "Применить").on_click(
                            cx.listener(|this, _, _, cx| this.apply_item_buff("buff", cx)),
                        )
                    } else {
                        self.button("btn-apply-buff2", "Применить").on_click(
                            cx.listener(|this, _, _, cx| this.apply_item_buff("buff2", cx)),
                        )
                    };
                    let delete_button = if slot == "buff" {
                        self.button("btn-del-buff", "Удалить бафф").on_click(
                            cx.listener(|this, _, _, cx| this.item_delete_buff("buff", cx)),
                        )
                    } else {
                        self.button("btn-del-buff2", "Удалить бафф").on_click(
                            cx.listener(|this, _, _, cx| this.item_delete_buff("buff2", cx)),
                        )
                    };
                    block = block.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_end()
                            .gap_2()
                            .child(self.select_control(
                                SharedString::from(format!("select-buff-param-{slot}")),
                                "Параметр",
                                choices(&ITEM_PARAMS),
                                Some(param),
                                move |this, value, cx| {
                                    this.item_set_buff_select(slot, "param", value, cx)
                                },
                                cx,
                            ))
                            .child(self.field_cell("Значение", value_input))
                            .child(self.field_cell("Длительность, с", duration_input))
                            .child(self.field_cell("Макс. стаков", stacks_input))
                            .child(
                                self.chip(
                                    SharedString::from(format!("chip-buff-pct-{slot}")),
                                    if pct { "pct ✓" } else { "pct" },
                                    pct,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.item_toggle_buff_pct(slot, cx),
                                )),
                            )
                            .child(apply_button)
                            .child(delete_button),
                    );
                }
                None => {
                    let add_button = if slot == "buff" {
                        self.button("btn-add-buff", "+ бафф")
                            .on_click(cx.listener(|this, _, _, cx| this.item_add_buff("buff", cx)))
                    } else {
                        self.button("btn-add-buff2", "+ бафф 2")
                            .on_click(cx.listener(|this, _, _, cx| this.item_add_buff("buff2", cx)))
                    };
                    block = block.child(add_button);
                }
            }
            buffs_body = buffs_body.child(block);
        }

        // ── эффект stats.effect ──
        let effect = item.stats.get("effect").and_then(Value::as_object).cloned();
        let effect_body = match effect {
            Some(map) => {
                let kind = map
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                div().flex().flex_col().gap_2().child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap_2()
                        .child(self.select_control(
                            "select-effect-kind",
                            "Вид эффекта",
                            choices(&EFFECT_KINDS),
                            Some(kind),
                            |this, value, cx| this.item_set_effect_kind(value, cx),
                            cx,
                        ))
                        .child(self.field_cell("Урон эффекта", &self.effect_value))
                        .child(self.field_cell("Длительность, с", &self.effect_duration))
                        .child(self.field_cell("Период, с", &self.effect_period))
                        .child(
                            self.button("btn-apply-effect", "Применить")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_item_effect(cx))),
                        )
                        .child(
                            self.button("btn-del-effect", "Удалить эффект").on_click(
                                cx.listener(|this, _, _, cx| this.item_delete_effect(cx)),
                            ),
                        ),
                )
            }
            None => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child("Эффект не задан."),
                )
                .child(
                    self.button("btn-add-effect", "+ эффект")
                        .on_click(cx.listener(|this, _, _, cx| this.item_add_effect(cx))),
                ),
        };

        // ── бонусы ADR-010 ──
        let mut bonus_body = div().flex().flex_col().gap_1();
        for (bonus_index, bonus) in bonuses.iter().enumerate() {
            let selected = self.selected_bonus == Some(bonus_index);
            let kind = bonus
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = bonus
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let value = bonus.get("value").and_then(Value::as_i64).unwrap_or(0);
            let pct = bonus.get("pct").and_then(Value::as_bool).unwrap_or(false);
            bonus_body = bonus_body.child(
                div()
                    .id(SharedString::from(format!("item-bonus-{bonus_index}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(PANEL_ALT)))
                    .hover(|d| d.bg(rgb(PANEL_ALT)))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.item_select_bonus(bonus_index, cx)),
                    )
                    .child(
                        div()
                            .w(px(90.0))
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(kind),
                    )
                    .child(div().w(px(220.0)).text_size(px(12.0)).child(id))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(format!("{value}{}", if pct { "%" } else { "" })),
                    ),
            );
        }
        let selected_bonus_value = self
            .selected_bonus
            .and_then(|bonus| bonuses.get(bonus).cloned());
        if let Some(bonus) = &selected_bonus_value {
            let kind = bonus
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("attr")
                .to_string();
            let id = bonus
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let pct = bonus.get("pct").and_then(Value::as_bool).unwrap_or(false);
            let id_choices: Vec<Choice> = match kind.as_str() {
                "param" => choices(&ITEM_PARAMS),
                "skill" => skill_choices.clone(),
                _ => choices(&ITEM_ATTRS),
            };
            let delete_button = self
                .button("btn-del-bonus", "Удалить бонус")
                .on_click(cx.listener(|this, _, _, cx| this.item_delete_bonus_click(cx)));
            bonus_body = bonus_body.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap_2()
                    .child(self.select_control(
                        "select-bonus-kind",
                        "Вид",
                        choices(&["attr", "param", "skill"]),
                        Some(kind),
                        |this, value, cx| this.item_set_bonus_select("kind", value, cx),
                        cx,
                    ))
                    .child(self.select_control(
                        "select-bonus-id",
                        "Идентификатор",
                        id_choices,
                        Some(id),
                        |this, value, cx| this.item_set_bonus_select("id", value, cx),
                        cx,
                    ))
                    .child(self.field_cell("Значение", &self.bonus_value))
                    .child(
                        self.chip("chip-bonus-pct", if pct { "pct ✓" } else { "pct" }, pct)
                            .on_click(cx.listener(|this, _, _, cx| this.item_toggle_bonus_pct(cx))),
                    )
                    .child(
                        self.button("btn-apply-bonus", "Применить")
                            .on_click(cx.listener(|this, _, _, cx| this.apply_item_bonus(cx))),
                    )
                    .child(delete_button),
            );
        } else {
            bonus_body = bonus_body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("Выберите бонус или добавьте новый."),
            );
        }
        bonus_body = bonus_body.child(
            self.button("btn-add-bonus", "+ бонус")
                .on_click(cx.listener(|this, _, _, cx| this.item_add_bonus_click(cx))),
        );

        // ── требования requires ──
        let requires_body = match &item.requires {
            Some(requires) => div().flex().flex_col().gap_2().child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap_2()
                    .child(self.select_control(
                        "req-profession",
                        "Профессия",
                        profession_choices,
                        requires.profession.clone(),
                        |this, value, cx| this.item_set_requires_profession(value, cx),
                        cx,
                    ))
                    .child(self.field_cell("str", &self.req_str))
                    .child(self.field_cell("dex", &self.req_dex))
                    .child(self.field_cell("vit", &self.req_vit))
                    .child(self.field_cell("int", &self.req_int))
                    .child(self.field_cell("Уровень", &self.req_level))
                    .child(
                        self.button("btn-apply-requires", "Применить")
                            .on_click(cx.listener(|this, _, _, cx| this.apply_item_requires(cx))),
                    )
                    .child(
                        self.button("btn-del-requires", "Удалить требования")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.item_delete_requires_click(cx)),
                            ),
                    ),
            ),
            None => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child("Требований нет."),
                )
                .child(
                    self.button("btn-add-requires", "+ требования")
                        .on_click(cx.listener(|this, _, _, cx| this.item_add_requires_click(cx))),
                ),
        };

        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · {}", item.id, item.name)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "type: {} · rarity: {} · бонусов: {} · параметров: {}",
                        item.item_type,
                        item.rarity,
                        bonuses.len(),
                        param_keys.len()
                    )),
            )
            .child(self.group_box("Основное", main_row))
            .child(self.group_box("Характеристики (stats)", stats_row))
            .child(self.group_box("Параметры (stats.param)", params_body))
            .child(self.group_box("Автобаффы (stats.buff / buff2)", buffs_body))
            .child(self.group_box("Эффект (stats.effect)", effect_body))
            .child(self.group_box("Бонусы (ADR-010)", bonus_body))
            .child(self.group_box("Требования (requires)", requires_body))
            .child(self.actions_row(cx, can_undo, can_redo));
        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(3) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("⚠ {warning}")),
            );
        }
        panel
    }

    fn locations_inspector(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let mut panel = div()
            .id("inspector-locations")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .overflow_y_scroll();
        let Some(project) = self.project.as_ref() else {
            return panel.child(
                div().text_color(rgb(ERR)).child(
                    self.open_error
                        .clone()
                        .unwrap_or_else(|| "проект не открыт".into()),
                ),
            );
        };
        let Some(location_index) = self.selected_location else {
            return panel.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("Выберите локацию в списке (E3: форма, выходы, сущности)."),
            );
        };
        let Some(location) = project.content().locations.get(location_index) else {
            return panel.child(div().text_color(rgb(ERR)).child("локация исчезла"));
        };

        // ── форма локации ──
        panel = panel
            .child(
                div()
                    .text_size(px(15.0))
                    .child(format!("{} · {}", location.id, location.name)),
            )
            .child(self.text_field("Имя", &self.loc_name))
            .child(self.text_field("Текст", &self.loc_text))
            .child(self.text_field("Флаги (через запятую)", &self.loc_flags))
            .child(self.text_field("spawn (true/false)", &self.loc_spawn))
            .child(
                self.button("btn-apply-location", "Применить локацию")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_location(cx))),
            );

        // ── выходы ──
        let mut exits = div().flex().flex_col().gap_1();
        exits = exits.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Выходы ({})", location.exits.len())),
                )
                .child(
                    self.button("btn-add-exit", "+ выход")
                        .on_click(cx.listener(|this, _, _, cx| this.add_exit(cx))),
                ),
        );
        let selected_exit = self.selected_exit;
        for (exit_index, exit) in location.exits.iter().enumerate() {
            let mut row = div()
                .id(("exit-row", exit_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected_exit == Some(exit_index), |d| d.bg(rgb(PANEL_ALT)))
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(cx.listener(move |this, _, _, cx| this.select_exit(exit_index, cx)))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(format!("{} → {}", exit.label, exit.to)),
                );
            row = row.child(
                div()
                    .id(("exit-del-hit", exit_index))
                    .px_1()
                    .cursor_pointer()
                    .text_color(rgb(ERR))
                    .text_size(px(13.0))
                    .child("×")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_exit(location_index, exit_index, cx);
                    })),
            );
            exits = exits.child(row);
        }
        if self.selected_exit.is_some() {
            exits = exits
                .child(self.text_field("Подпись", &self.exit_label))
                .child(self.text_field("dir", &self.exit_dir))
                .child(self.text_field("to (id локации)", &self.exit_to))
                .child(
                    self.button("btn-apply-exit", "Применить выход")
                        .on_click(cx.listener(|this, _, _, cx| this.apply_exit(cx))),
                );
        }
        panel = panel.child(exits);

        // ── сущности ──
        let mut entities = div().flex().flex_col().gap_1();
        entities = entities.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(format!("Сущности ({})", location.entities.len())),
                )
                .child(
                    self.button("btn-add-monster", "+ монстр")
                        .on_click(cx.listener(|this, _, _, cx| this.add_entity("monster", cx))),
                )
                .child(
                    self.button("btn-add-npc", "+ НПС")
                        .on_click(cx.listener(|this, _, _, cx| this.add_entity("npc", cx))),
                )
                .child(
                    self.button("btn-add-object", "+ объект")
                        .on_click(cx.listener(|this, _, _, cx| this.add_entity("object", cx))),
                )
                .child(
                    self.button("btn-add-resource", "+ ресурс")
                        .on_click(cx.listener(|this, _, _, cx| this.add_entity("resource", cx))),
                ),
        );
        let selected_entity = self.selected_entity;
        for (entity_index, entity) in location.entities.iter().enumerate() {
            let row = div()
                .id(("entity-row", entity_index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected_entity == Some(entity_index), |d| {
                    d.bg(rgb(PANEL_ALT))
                })
                .hover(|d| d.bg(rgb(PANEL_ALT)))
                .on_click(cx.listener(move |this, _, _, cx| this.select_entity(entity_index, cx)))
                .child(
                    div()
                        .w(px(70.0))
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(kind_str(entity.kind)),
                )
                .child(
                    div()
                        .w(px(120.0))
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(entity.id.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.0))
                        .child(entity.name.clone()),
                )
                .child(
                    div()
                        .id(("entity-del", entity_index))
                        .px_1()
                        .cursor_pointer()
                        .text_color(rgb(ERR))
                        .text_size(px(13.0))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_entity(location_index, entity_index, cx);
                        })),
                );
            entities = entities.child(row);
        }
        if self.selected_entity.is_some() {
            if let Some(entity) = self
                .selected_entity
                .and_then(|index| location.entities.get(index))
            {
                entities = entities
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(format!("id: {}", entity.id)),
                    )
                    .child(self.text_field("kind", &self.ent_kind))
                    .child(self.text_field("Имя", &self.ent_name))
                    .child(self.text_field("extra (JSON-объект)", &self.ent_extra))
                    .child(
                        self.button("btn-apply-entity", "Применить сущность")
                            .on_click(cx.listener(|this, _, _, cx| this.apply_entity(cx))),
                    );
            }
        }
        panel = panel.child(entities);
        panel = panel.child(self.actions_row(cx, project.can_undo(), project.can_redo()));

        for error in project.report().errors.iter().take(5) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(ERR))
                    .child(format!("✗ {error}")),
            );
        }
        for warning in project.report().warnings.iter().take(3) {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(WARN))
                    .child(format!("⚠ {warning}")),
            );
        }
        panel
    }

    fn status_bar(&self) -> impl IntoElement {
        let (label, color) = match &self.project {
            None => (
                self.open_error
                    .clone()
                    .unwrap_or_else(|| "проект не открыт".into()),
                ERR,
            ),
            Some(project) => {
                let dirty = if project.is_dirty() {
                    format!("не сохранено: {}", project.dirty_files().len())
                } else {
                    "сохранено".to_string()
                };
                let errors = project.report().errors.len();
                let warnings = project.report().warnings.len();
                let color = if errors > 0 {
                    ERR
                } else if warnings > 0 {
                    WARN
                } else {
                    OK
                };
                (
                    format!(
                        "ошибки: {errors} · предупреждения: {warnings} · {dirty} · {}",
                        self.status
                    ),
                    color,
                )
            }
        };
        let mut label = label;
        if let Some(log) = &self.seed_log {
            label.push_str(" · ");
            label.push_str(log);
        }
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_size(px(12.0))
            .text_color(rgb(color))
            .child(label)
    }
}

/// Метка drag-жеста панорамы карты (gpui `on_drag`/`on_drag_move`).
struct MapPan;

/// Метка drag узла карты (E4.2): перенос либо связывание.
#[derive(Clone)]
struct MapNodeDrag {
    id: String,
}

/// Drag-ghost узла: сам узел на полотне остаётся и двигается.
struct MapDragGhost;

impl Render for MapDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size(px(18.0))
            .rounded_full()
            .bg(rgb(ACCENT))
            .opacity(0.55)
    }
}

/// E4: карта-полотно — раскладка, зум/панорама, выбор узла, sidecar.
impl EditorApp {
    /// Загрузить раскладку карты, если ещё не загружена.
    fn ensure_map(&mut self) {
        if self.map.is_some() {
            return;
        }
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let mut layout = MapLayout::load(project.content_dir())
            .ok()
            .flatten()
            .unwrap_or_default();
        layout.merge_auto(&project.content().locations);
        self.map = Some(layout);
    }

    /// Центрировать карту на выбранном узле (иначе — спавн/первый узел).
    fn center_map(&mut self) {
        let target = {
            let (Some(project), Some(map)) = (self.project.as_ref(), self.map.as_ref()) else {
                return;
            };
            let id = self
                .selected_location
                .and_then(|index| project.content().locations.get(index))
                .map(|location| location.id.clone())
                .or_else(|| {
                    project
                        .content()
                        .locations
                        .iter()
                        .find(|location| location.spawn)
                        .map(|location| location.id.clone())
                })
                .or_else(|| map.nodes.keys().next().cloned());
            let Some(id) = id else {
                return;
            };
            let Some(cell) = map.nodes.get(&id) else {
                return;
            };
            Some(*cell)
        };
        let Some(cell) = target else {
            return;
        };
        let cell_px = 46.0 * self.map_zoom;
        let (vw, vh) = self.map_viewport;
        // Узел рисуется центром в (cell.x*cell + pan.x, cell.y*cell + pan.y).
        self.map_pan = (
            vw / 2.0 - cell.x as f32 * cell_px,
            vh / 2.0 - cell.y as f32 * cell_px,
        );
        self.map_centered = true;
    }

    /// Зум колесом (якорь — центр области карты).
    fn zoom_map(&mut self, delta_y: f32) {
        let old = self.map_zoom;
        let new = (old * (1.0 - delta_y * 0.0015)).clamp(0.15, 3.0);
        if (new - old).abs() < f32::EPSILON {
            return;
        }
        self.apply_zoom(new, old);
    }

    /// Зум кнопками/клавишами на заданный множитель.
    fn zoom_step(&mut self, factor: f32, cx: &mut Context<'_, Self>) {
        let old = self.map_zoom;
        let new = (old * factor).clamp(0.15, 3.0);
        if (new - old).abs() >= f32::EPSILON {
            self.apply_zoom(new, old);
        }
        cx.notify();
    }

    fn apply_zoom(&mut self, new: f32, old: f32) {
        let k = new / old;
        let (vw, vh) = self.map_viewport;
        self.map_pan = (
            vw / 2.0 - (vw / 2.0 - self.map_pan.0) * k,
            vh / 2.0 - (vh / 2.0 - self.map_pan.1) * k,
        );
        self.map_zoom = new;
    }

    fn drag_map_pan(&mut self, position: gpui::Point<gpui::Pixels>) {
        let point = (f32::from(position.x), f32::from(position.y));
        if let Some(last) = self.map_pan_last {
            self.map_pan = (
                self.map_pan.0 + point.0 - last.0,
                self.map_pan.1 + point.1 - last.1,
            );
        }
        self.map_pan_last = Some(point);
    }

    fn select_map_location(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        let index = self.project.as_ref().and_then(|project| {
            project
                .content()
                .locations
                .iter()
                .position(|location| location.id == id)
        });
        let Some(index) = index else {
            return;
        };
        self.selected_location = Some(index);
        self.selected_exit = None;
        self.selected_entity = None;
        self.refresh_location_fields(cx);
        self.status = format!("узел {id}: ←↑→↓ — сдвиг, «В форму» — редактирование");
        cx.notify();
    }

    fn move_map_node(&mut self, dx: i32, dy: i32, cx: &mut Context<'_, Self>) {
        let id = self.project.as_ref().and_then(|project| {
            self.selected_location
                .and_then(|index| project.content().locations.get(index))
                .map(|location| location.id.clone())
        });
        let Some(id) = id else {
            self.status = "карта: выберите узел (клик), чтобы сдвинуть стрелками".into();
            cx.notify();
            return;
        };
        let Some(map) = self.map.as_mut() else {
            return;
        };
        let Some(cell) = map.nodes.get_mut(&id) else {
            return;
        };
        cell.x += dx;
        cell.y += dy;
        let (x, y) = (cell.x, cell.y);
        self.map_dirty = true;
        self.status = format!("узел {id} → ({x}, {y}) · Ctrl+S — сохранить раскладку");
        cx.notify();
    }

    fn auto_layout_map(&mut self, cx: &mut Context<'_, Self>) {
        let layout = self
            .project
            .as_ref()
            .map(|project| auto_layout(&project.content().locations));
        let Some(layout) = layout else {
            return;
        };
        self.map = Some(layout);
        self.map_dirty = true;
        self.map_centered = false;
        self.center_map();
        self.status = "авто-раскладка построена · Ctrl+S — сохранить".into();
        cx.notify();
    }

    /// Сохранить sidecar-раскладку (`content/editor/layout.json`).
    /// `Ok(false)` — нечего сохранять.
    fn save_layout(&mut self) -> Result<bool, String> {
        if !self.map_dirty {
            return Ok(false);
        }
        let (Some(project), Some(map)) = (self.project.as_ref(), self.map.as_ref()) else {
            return Ok(false);
        };
        let dir = project.content_dir().to_path_buf();
        map.save(&dir)?;
        self.map_dirty = false;
        Ok(true)
    }

    fn save_layout_click(&mut self, cx: &mut Context<'_, Self>) {
        self.status = match self.save_layout() {
            Ok(true) => "раскладка сохранена".into(),
            Ok(false) => "раскладка не менялась".into(),
            Err(err) => err,
        };
        cx.notify();
    }

    fn zoom_click(&mut self, factor: f32, cx: &mut Context<'_, Self>) {
        self.zoom_step(factor, cx);
    }

    fn open_selected_in_form(&mut self, cx: &mut Context<'_, Self>) {
        if self.selected_location.is_none() {
            self.status = "карта: сначала выберите узел".into();
        } else {
            self.section = Section::Locations;
            self.filter.clear();
            Self::put_text(&self.search, "", cx);
            self.status = "локация открыта в форме".into();
        }
        cx.notify();
    }

    /// Полотно карты: линии-выходы на canvas + узлы/панель инструментов.
    /// Индекс локации по id (для drag-связывания).
    fn location_index_of(&self, id: &str) -> Option<usize> {
        self.project
            .as_ref()?
            .content()
            .locations
            .iter()
            .position(|location| location.id == id)
    }

    /// Переключить режим «Связать» (drag узла на узел создаёт/удаляет выход).
    fn toggle_link_mode(&mut self, cx: &mut Context<'_, Self>) {
        self.map_link = !self.map_link;
        self.status = if self.map_link {
            "режим связи: перетащите узел на узел — создать/удалить выход".into()
        } else {
            "перенос: drag узла двигает его по сетке".into()
        };
        cx.notify();
    }

    /// Движение при перетаскивании узла: перенос по сетке (обычный режим)
    /// либо без движения (режим связи). Отпускание вне drop-цели — конец.
    fn on_node_drag_move(
        &mut self,
        local: (f32, f32),
        pressed: Option<gpui::MouseButton>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(id) = self.map_dragging.clone() else {
            return;
        };
        if pressed != Some(gpui::MouseButton::Left) {
            self.map_dragging = None;
            return;
        }
        if self.map_link {
            return; // в режиме связи узлы не двигаются
        }
        let cell_px = 46.0 * self.map_zoom;
        let size = node_size(cell_px);
        let cell = snap_cell(local, self.map_pan, cell_px, self.map_drag_grab, size);
        let Some(map) = self.map.as_mut() else {
            return;
        };
        let Some(pos) = map.nodes.get_mut(&id) else {
            return;
        };
        if pos.x == cell.x && pos.y == cell.y {
            return;
        }
        *pos = cell;
        self.map_dirty = true;
        self.status = format!("узел {id} → ({}, {}) · Ctrl+S — сохранить", cell.x, cell.y);
        cx.notify();
    }

    /// Отпускание узла на другом узле (E4.2): в режиме связи — создаёт
    /// выход `from → to`, а если он уже есть — удаляет (toggle).
    fn handle_node_drop(&mut self, from_id: &str, to_id: &str, cx: &mut Context<'_, Self>) {
        self.map_dragging = None;
        if !self.map_link {
            cx.notify();
            return;
        }
        if from_id == to_id {
            self.status = "нельзя связать локацию с самой собой".into();
            cx.notify();
            return;
        }
        let (Some(from_index), Some(to_index)) = (
            self.location_index_of(from_id),
            self.location_index_of(to_id),
        ) else {
            self.status = "узел не найден в контенте".into();
            cx.notify();
            return;
        };
        let (from_name, to_name) = {
            let Some(project) = self.project.as_ref() else {
                return;
            };
            let content = project.content();
            (
                content.locations[from_index].name.clone(),
                content.locations[to_index].name.clone(),
            )
        };
        let dir = self
            .map
            .as_ref()
            .and_then(|map| match (map.nodes.get(from_id), map.nodes.get(to_id)) {
                (Some(from_cell), Some(to_cell)) => compass_dir(*from_cell, *to_cell),
                _ => None,
            })
            .map(str::to_string);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.toggle_exit(from_index, to_index, dir) {
            Ok(created) => {
                self.selected_location = Some(from_index);
                self.selected_exit = None;
                self.selected_entity = None;
                self.status = if created {
                    format!("выход «{from_name}» → «{to_name}» создан")
                } else {
                    format!("выход «{from_name}» → «{to_name}» удалён")
                };
            }
            Err(err) => self.status = format!("связать: {err}"),
        }
        cx.notify();
    }

    fn map_body(&mut self, cx: &mut Context<'_, Self>) -> gpui::AnyElement {
        self.ensure_map();
        if !self.map_centered {
            self.center_map();
        }
        let cell = 46.0 * self.map_zoom;
        let (px_off, py_off) = self.map_pan;
        let (vw, vh) = self.map_viewport;

        // Владеемые данные для отрисовки (без заимствований project/map).
        let (nodes, edges) = {
            let Some(project) = self.project.as_ref() else {
                return div()
                    .flex_1()
                    .p_4()
                    .text_color(rgb(ERR))
                    .child("проект не открыт")
                    .into_any_element();
            };
            let Some(map) = self.map.as_ref() else {
                return div()
                    .flex_1()
                    .p_4()
                    .text_color(rgb(MUTED))
                    .child("нет раскладки карты")
                    .into_any_element();
            };
            let selected_id = self
                .selected_location
                .and_then(|index| project.content().locations.get(index))
                .map(|location| location.id.clone());
            let mut nodes: Vec<(String, f32, f32, bool, bool, String)> = Vec::new();
            let mut ids: Vec<(&String, &crate::map::Cell)> = map.nodes.iter().collect();
            ids.sort_by(|a, b| a.0.cmp(b.0));
            for (id, pos) in ids {
                let x = pos.x as f32 * cell + px_off;
                let y = pos.y as f32 * cell + py_off;
                if x < -cell || y < -cell || x > vw + cell || y > vh + cell {
                    continue;
                }
                let location = project
                    .content()
                    .locations
                    .iter()
                    .find(|location| location.id == *id);
                let name = location
                    .map(|location| location.name.clone())
                    .unwrap_or_default();
                let spawn = location.map(|location| location.spawn).unwrap_or(false);
                let selected = selected_id.as_deref() == Some(id.as_str());
                nodes.push((id.clone(), x, y, selected, spawn, name));
            }
            let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
            for location in &project.content().locations {
                let Some(from) = map.nodes.get(&location.id) else {
                    continue;
                };
                let fx = from.x as f32 * cell + px_off;
                let fy = from.y as f32 * cell + py_off;
                for exit in &location.exits {
                    let Some(to) = map.nodes.get(&exit.to) else {
                        continue;
                    };
                    let tx = to.x as f32 * cell + px_off;
                    let ty = to.y as f32 * cell + py_off;
                    let outside =
                        |x: f32, y: f32| x < -cell || y < -cell || x > vw + cell || y > vh + cell;
                    if outside(fx, fy) && outside(tx, ty) {
                        continue;
                    }
                    edges.push((fx, fy, tx, ty));
                }
            }
            (nodes, edges)
        };

        // Линии-выходы: canvas + paint_path (как клиентская карта M26).
        let lines = edges;
        let canvas = gpui::canvas(
            |_bounds, _window, _cx| {},
            move |bounds, _state, window, _cx| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let line_color: gpui::Hsla = gpui::rgba(0x4a_4f_58_ff).into();
                for (x1, y1, x2, y2) in &lines {
                    let (ax, ay) = (x1 + ox, y1 + oy);
                    let (bx, by) = (x2 + ox, y2 + oy);
                    let (dx, dy) = (bx - ax, by - ay);
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 1.0 {
                        continue;
                    }
                    let (nx, ny) = (-dy / len, dx / len);
                    let mut path = gpui::Path::new(gpui::point(px(ax + nx), px(ay + ny)));
                    path.line_to(gpui::point(px(bx + nx), px(by + ny)));
                    path.line_to(gpui::point(px(bx - nx), px(by - ny)));
                    path.line_to(gpui::point(px(ax - nx), px(ay - ny)));
                    window.paint_path(path, line_color);
                }
            },
        );

        // Узлы: кружки; спавн — ★; выбранный — акцент и плашка имени.
        let node_size = node_size(cell);
        let link_mode = self.map_link;
        let mut node_els: Vec<gpui::AnyElement> = Vec::new();
        for (id, x, y, selected, spawn, name) in nodes {
            let left = x - node_size / 2.0;
            let top = y - node_size / 2.0;
            let click_id = id.clone();
            let drag_entity = cx.entity();
            let drop_entity = cx.entity();
            let drag_id = id.clone();
            let drop_id = id.clone();
            let mut node = div()
                .id(SharedString::from(format!("map-node-{id}")))
                .absolute()
                .left(px(left))
                .top(px(top))
                .size(px(node_size))
                .rounded(px(node_size / 2.0))
                .border(px(2.0))
                .border_color(rgb(if selected {
                    ACCENT
                } else if spawn {
                    OK
                } else if link_mode {
                    WARN
                } else {
                    BORDER
                }))
                .bg(rgb(if selected { PANEL_ALT } else { PANEL }))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_map_location(&click_id, cx)),
                )
                .on_drag(
                    MapNodeDrag { id: drag_id },
                    move |drag, offset, _window, acx| {
                        let grabbed = drag.id.clone();
                        let grab = (offset.x.as_f32(), offset.y.as_f32());
                        drag_entity.update(acx, |this, cx| {
                            if let Some(project) = this.project.as_ref() {
                                if let Some(index) = project
                                    .content()
                                    .locations
                                    .iter()
                                    .position(|location| location.id == grabbed)
                                {
                                    this.selected_location = Some(index);
                                    this.selected_exit = None;
                                    this.selected_entity = None;
                                }
                            }
                            this.map_dragging = Some(grabbed);
                            this.map_drag_grab = grab;
                            cx.notify();
                        });
                        acx.new(|_| MapDragGhost)
                    },
                )
                .on_drop::<MapNodeDrag>(move |drag: &MapNodeDrag, _window, acx| {
                    let from = drag.id.clone();
                    let to = drop_id.clone();
                    drop_entity.update(acx, |this, cx| this.handle_node_drop(&from, &to, cx));
                });
            if spawn {
                node = node.child(div().text_size(px(9.0)).text_color(rgb(OK)).child("★"));
            }
            node_els.push(node.into_any_element());
            if selected {
                node_els.push(
                    div()
                        .absolute()
                        .left(px(left + node_size + 4.0))
                        .top(px(top))
                        .px_1()
                        .rounded(px(4.0))
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(ACCENT))
                        .text_size(px(11.0))
                        .child(name)
                        .into_any_element(),
                );
            }
        }

        // Панель инструментов поверх полотна.
        let has_selection = self.selected_location.is_some();
        let dirty = if self.map_dirty {
            " · раскладка изменена"
        } else {
            ""
        };
        let zoom = self.map_zoom;
        let toolbar = div()
            .absolute()
            .left(px(10.0))
            .top(px(10.0))
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .rounded_md()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(BORDER))
            .child(
                self.button("map-auto", "Авто-раскладка")
                    .on_click(cx.listener(|this, _, _, cx| this.auto_layout_map(cx))),
            )
            .child(
                self.button("map-center", "Центр")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.center_map();
                        cx.notify();
                    })),
            )
            .child(
                self.button("map-save", "Сохранить раскладку")
                    .on_click(cx.listener(|this, _, _, cx| this.save_layout_click(cx))),
            )
            .child(
                self.button("map-open", "В форму")
                    .when(!has_selection, |d| d.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| this.open_selected_in_form(cx))),
            )
            .child(
                self.button(
                    "map-link",
                    if link_mode {
                        "Связать ✓"
                    } else {
                        "Связать"
                    },
                )
                .when(link_mode, |d| {
                    d.border_color(rgb(ACCENT)).bg(rgb(PANEL_ALT))
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_link_mode(cx))),
            )
            .child(
                self.button("map-zoom-in", "+")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_click(1.2, cx))),
            )
            .child(
                self.button("map-zoom-out", "−")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_click(1.0 / 1.2, cx))),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!("зум {zoom:.2}{dirty}")),
            );

        // Полотно: колесо — зум, drag — панорама.
        let shell_entity = cx.entity();
        let scroll_entity = shell_entity.clone();
        let drag_entity = shell_entity.clone();
        let move_entity = shell_entity.clone();
        let node_move_entity = shell_entity.clone();
        div()
            .id("map-shell")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0x17181a))
            .on_scroll(move |event, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if delta.y == gpui::Pixels::ZERO {
                    return;
                }
                scroll_entity.update(cx, |this, cx| {
                    this.zoom_map(f32::from(delta.y));
                    cx.notify();
                });
            })
            .on_drag(MapPan, move |_, _, _, acx| {
                drag_entity.update(acx, |this, _| this.map_pan_last = None);
                acx.new(|_| EmptyView)
            })
            .on_drag_move::<MapPan>(move |event, _, cx| {
                let position = event.event.position;
                move_entity.update(cx, |this, cx| {
                    this.drag_map_pan(position);
                    cx.notify();
                });
            })
            .on_drag_move::<MapNodeDrag>(move |event, _, cx| {
                let local = (
                    (event.event.position.x - event.bounds.origin.x).as_f32(),
                    (event.event.position.y - event.bounds.origin.y).as_f32(),
                );
                let pressed = event.event.pressed_button;
                node_move_entity.update(cx, |this, cx| this.on_node_drag_move(local, pressed, cx));
            })
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .size_full()
                    .child(canvas.w_full().h_full()),
            )
            .children(node_els)
            .child(toolbar)
            .into_any_element()
    }
}

/// Первые `max` символов строки (для строк списков).
/// Текстовое представление значения stats для поля ввода.
fn text_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

/// Значение из числа для JSON: целые пишутся без `.0`.
fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

/// Число для поля ввода: целые — без `.0`.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

/// Список вариантов Select из фиксированных id (значение = подпись).
fn choices(items: &[&str]) -> Vec<Choice> {
    items
        .iter()
        .map(|item| Choice::new((*item).to_string(), (*item).to_string()))
        .collect()
}

/// То же, но с пустым вариантом «—» в начале (значение `""` → удалить поле).
fn choices_with_none(items: &[&str]) -> Vec<Choice> {
    let mut result = vec![Choice::new("", "—")];
    result.extend(choices(items));
    result
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

fn short(version: &str) -> &str {
    version.get(..12).unwrap_or(version)
}

/// Строковое имя вида сущности (как в контенте).
fn kind_str(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Player => "player",
        EntityKind::Monster => "monster",
        EntityKind::Npc => "npc",
        EntityKind::Corpse => "corpse",
        EntityKind::Object => "object",
        EntityKind::Resource => "resource",
        EntityKind::Container => "container",
        EntityKind::Unknown => "unknown",
    }
}

/// Что применяет кнопка «Применить» / Enter в группе полей.
#[derive(Clone, Copy)]
enum ApplyKind {
    Item,
    ItemStats,
    ItemParam,
    ItemBuff,
    ItemBuff2,
    ItemEffect,
    ItemBonus,
    ItemRequires,
    Location,
    Exit,
    Entity,
    Quests,
    Recipes,
    Skills,
    Professions,
    Goal,
    RewardItem,
    RecipeInput,
    Dialogs,
    Shops,
    DialogLine,
    DialogReply,
    Training,
    ShopCurrency,
    ShopItem,
}

impl Render for EditorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.init_inputs(window, cx);
        let size = window.viewport_size();
        self.map_viewport = (
            (f32::from(size.width) - 150.0).max(200.0),
            (f32::from(size.height) - 110.0).max(200.0),
        );
        let body = match self.section {
            Section::Items => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.items_list(cx))
                .child(self.items_inspector(cx)),
            Section::Locations => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.locations_list(cx))
                .child(self.locations_inspector(cx)),
            Section::Map => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.map_body(cx)),
            Section::Dialogs => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Dialogs, cx))
                .child(self.dialogs_inspector(cx)),
            Section::Shops => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Shops, cx))
                .child(self.shops_inspector(cx)),
            Section::Quests => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Quests, cx))
                .child(self.quests_inspector(cx)),
            Section::Recipes => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Recipes, cx))
                .child(self.recipes_inspector(cx)),
            Section::Skills => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Skills, cx))
                .child(self.skills_inspector(cx)),
            Section::Professions => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.records_list(Section::Professions, cx))
                .child(self.professions_inspector(cx)),
            Section::Help => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.rail(cx))
                .child(self.help_body()),
        };
        div()
            .size_full()
            .relative()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .flex()
            .flex_col()
            .child(self.header(cx))
            .child(body)
            .child(self.status_bar())
            .child(self.quick_palette(cx))
    }
}
