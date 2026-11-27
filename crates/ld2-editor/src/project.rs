//! Проект контента: открытие `content/`, правки, undo/redo, сохранение.
//!
//! Источник правды — файлы в git. Правки применяются к сырым JSON-значениям
//! (`RawContent`, порядок ключей сохраняется), затем пересобирается
//! типизированное дерево ([`GameContent`]) — валидация/версия/UI.
//! Сохранение атомарное (`tmp + rename`), только изменённые файлы;
//! при фатальных ошибках валидации сохранение блокируется.
//!
//! Undo/redo — стек инверсных правок (`Set`/`Push`/`Remove` + путь и
//! значение), а не снапшоты: правка локации/предмета стоит килобайты,
//! а не мегабайты. Одна пачка правок = одна запись undo.

use std::path::{Path, PathBuf};

use ld2_content::{
    validate_items, validate_npc_content, validate_references, validate_report, version_raw,
    ContentDialogs, ContentError, ContentFile, ContentShops, GameContent, RawContent,
    ValidationReport, ITEM_ATTRS, ITEM_PARAMS,
};
use serde_json::{json, Value};

const ITEM_WEAPON: &str = "weapon";
const ITEM_MONEY: &str = "money";
const RARITY_COMMON: &str = "common";
const DEMO_DIALOG_LINE: &str = "l_hello";
const PROJECT_SEARCH_DEPTH: usize = 6;
const DIALOG_LABEL_MAX: usize = 40;

/// Предел истории undo (правок, не мегабайт — история лёгкая).
const UNDO_LIMIT: usize = 200;

/// Сегмент пути в JSON-дереве файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    /// Ключ объекта.
    Key(String),
    /// Индекс массива.
    Index(usize),
}

impl PathSeg {
    pub fn key(key: impl Into<String>) -> Self {
        Self::Key(key.into())
    }

    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

/// Правка сырого JSON-файла.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Установить значение по пути (создание ключа разрешено).
    Set(Vec<PathSeg>, Value),
    /// Добавить значение в массив по пути (путь указывает на массив).
    Push(Vec<PathSeg>, Value),
    /// Удалить элемент массива (путь указывает на элемент).
    Remove(Vec<PathSeg>),
    /// Удалить поле объекта (для необязательных полей: `giver`, `tool`, …).
    RemoveKey(Vec<PathSeg>),
}

/// Ссылочная категория пикера (E5): что искать по контенту.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// Предмет по id (ингредиент/награда/результат/валюта).
    Item,
    /// Тип предмета (`flask`, `chisel`, …) — инструмент рецепта.
    ItemType,
    /// Задел E5.2: пикер требований рецепта.
    #[allow(dead_code)]
    Skill,
    Profession,
    Location,
    Npc,
    Monster,
    /// Станок (`extra.station` объекта локации).
    Station,
    /// Квест (гейт строки диалога `quest`).
    Quest,
    /// Строка диалога (переход `next` у реплики).
    DialogLine,
}

/// Вариант выбора для пикера-ссылки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    /// Подпись для списка (`<id> — <имя> (…)`).
    pub label: String,
}

/// Ошибка проекта (открытие/правка/сохранение).
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("контент не найден: ожидался {0}/content/items.json (или каталог content)")]
    NotFound(PathBuf),
    #[error(transparent)]
    Content(#[from] ContentError),
    #[error("правка не применима: {0}")]
    Patch(String),
    #[error("контент не прошёл валидацию: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

/// Одна инверсная правка.
#[derive(Debug, Clone)]
enum Change {
    Set {
        file: ContentFile,
        path: Vec<PathSeg>,
        old: Option<Value>,
        new: Value,
    },
    Push {
        file: ContentFile,
        path: Vec<PathSeg>,
        new: Value,
    },
    Remove {
        file: ContentFile,
        path: Vec<PathSeg>,
        old: Value,
    },
    RemoveKey {
        file: ContentFile,
        path: Vec<PathSeg>,
        old: Option<Value>,
    },
}

impl Change {
    fn file(&self) -> ContentFile {
        match self {
            Change::Set { file, .. }
            | Change::Push { file, .. }
            | Change::Remove { file, .. }
            | Change::RemoveKey { file, .. } => *file,
        }
    }
}

/// Открытый проект контента.
pub struct EditorProject {
    root: PathBuf,
    content_dir: PathBuf,
    raw: RawContent,
    baseline: RawContent,
    content: GameContent,
    dialogs: ContentDialogs,
    shops: ContentShops,
    report: ValidationReport,
    version: String,
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
}

impl EditorProject {
    /// Открыть проект: `root` — корень репозитория (внутри `content/`)
    /// или сам каталог контента.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let root = root.as_ref();
        let (repo_root, content_dir) =
            resolve_dirs(root).ok_or_else(|| ProjectError::NotFound(root.to_path_buf()))?;
        let raw = RawContent::load(&content_dir)?;
        let content = raw.typed()?;
        let dialogs = raw.typed_dialogs()?;
        let shops = raw.typed_shops()?;
        let (report, version) = build_report_and_version(&raw, &content, &dialogs, &shops)?;
        Ok(Self {
            root: repo_root,
            content_dir,
            baseline: raw.clone(),
            raw,
            content,
            dialogs,
            shops,
            report,
            version,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// Найти проект вверх от `start` (CWD редактора — обычно корень репо).
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let start = start.as_ref();
        for dir in start.ancestors().take(PROJECT_SEARCH_DEPTH) {
            if resolve_dirs(dir).is_some() {
                return Self::open(dir);
            }
        }
        Err(ProjectError::NotFound(start.to_path_buf()))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn content_dir(&self) -> &Path {
        &self.content_dir
    }

    pub fn content(&self) -> &GameContent {
        &self.content
    }

    /// Типизированные диалоги (вьюха; правки — через сырой JSON).
    pub fn dialogs(&self) -> &ContentDialogs {
        &self.dialogs
    }

    /// Типизированные магазины (вьюха; правки — через сырой JSON).
    pub fn shops(&self) -> &ContentShops {
        &self.shops
    }

    /// Отчёт валидации (ошибки блокируют сохранение, предупреждения — нет).
    pub fn report(&self) -> &ValidationReport {
        &self.report
    }

    /// sha256-версия текущего (в памяти) контента.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Файлы, отличающиеся от состояния на диске.
    pub fn dirty_files(&self) -> Vec<ContentFile> {
        ContentFile::ALL
            .iter()
            .copied()
            .filter(|file| self.raw.get(*file) != self.baseline.get(*file))
            .collect()
    }

    pub fn is_dirty(&self) -> bool {
        ContentFile::ALL
            .iter()
            .any(|file| self.raw.get(*file) != self.baseline.get(*file))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Применить пачку правок (одна запись undo). При ошибке разбора
    /// (например, текст в числовое поле) — все правки откатываются.
    pub fn apply_edits(&mut self, file: ContentFile, edits: Vec<Edit>) -> Result<(), ProjectError> {
        if edits.is_empty() {
            return Ok(());
        }
        let mut changes = Vec::with_capacity(edits.len());
        for edit in edits {
            match edit {
                Edit::Set(path, new) => {
                    let old = get_path(self.raw.get(file), &path).cloned();
                    set_path(self.raw.get_mut(file), &path, new.clone())?;
                    changes.push(Change::Set {
                        file,
                        path,
                        old,
                        new,
                    });
                }
                Edit::Push(path, new) => {
                    push_at(self.raw.get_mut(file), &path, new.clone())?;
                    changes.push(Change::Push { file, path, new });
                }
                Edit::Remove(path) => {
                    let old = remove_at(self.raw.get_mut(file), &path)?;
                    changes.push(Change::Remove { file, path, old });
                }
                Edit::RemoveKey(path) => {
                    let old = get_path(self.raw.get(file), &path).cloned();
                    remove_path(self.raw.get_mut(file), &path);
                    changes.push(Change::RemoveKey { file, path, old });
                }
            }
        }
        match self.rebuild() {
            Ok(()) => {
                self.push_undo(changes);
                Ok(())
            }
            Err(err) => {
                // Откат: тип-несовместимая правка не должна остаться в дереве.
                for change in changes.iter().rev() {
                    undo_change(&mut self.raw, change);
                }
                let _ = self.rebuild();
                Err(ProjectError::Content(err))
            }
        }
    }

    /// Установить поля по пути (обёртка над [`Self::apply_edits`]).
    #[allow(dead_code)] // используется тестами и старым API
    pub fn apply(
        &mut self,
        edits: Vec<(ContentFile, Vec<PathSeg>, Value)>,
    ) -> Result<(), ProjectError> {
        let mut by_file: Vec<(ContentFile, Vec<Edit>)> = Vec::new();
        for (file, path, value) in edits {
            match by_file.iter_mut().find(|(f, _)| *f == file) {
                Some((_, edits)) => edits.push(Edit::Set(path, value)),
                None => by_file.push((file, vec![Edit::Set(path, value)])),
            }
        }
        for (file, edits) in by_file {
            self.apply_edits(file, edits)?;
        }
        Ok(())
    }

    /// Правка полей предмета по имени (UI-форма): `name`, `rarity`,
    /// `gold_value`, `stats` (объект), …
    #[allow(dead_code)] // используется тестами; UI пишет через set_item_field
    pub fn set_item_fields(
        &mut self,
        index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| {
                (
                    ContentFile::Items,
                    vec![
                        PathSeg::key(ContentFile::Items.root_key()),
                        PathSeg::index(index),
                        PathSeg::key(key),
                    ],
                    value,
                )
            })
            .collect();
        self.apply(edits)
    }

    // ─── E6.2: полный редактор предмета (структурные зоны) ────────────────

    /// Путь к элементу массива предметов: `[items, index]`.
    fn item_base(index: usize) -> Vec<PathSeg> {
        record_path(ContentFile::Items, index)
    }

    /// Полный путь к относительному пути внутри предмета.
    fn item_path(index: usize, rel: &[PathSeg]) -> Vec<PathSeg> {
        let mut path = Self::item_base(index);
        path.extend_from_slice(rel);
        path
    }

    /// Установить/удалить значение по относительному пути внутри предмета.
    /// `None` — удаляет ключ (для необязательных полей).
    pub fn item_edit(
        &mut self,
        index: usize,
        rel: Vec<PathSeg>,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        let path = Self::item_path(index, &rel);
        let edit = match value {
            Some(value) => Edit::Set(path, value),
            None => Edit::RemoveKey(path),
        };
        self.apply_edits(ContentFile::Items, vec![edit])
    }

    /// Поле верхнего уровня предмета (`name`, `type`, `rarity`, `slot`,
    /// `gold_value`, `upgrade`, `qty`, `sockets`, `rune_sockets`, `hands`,
    /// `cooldown_s`, `skill`, …); `None` — удалить необязательное поле.
    pub fn set_item_field(
        &mut self,
        index: usize,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(index, vec![PathSeg::key(key)], value)
    }

    /// Поле `stats`: `set_item_stat(0, "dmg", Some("6..9".into()))`.
    pub fn set_item_stat(
        &mut self,
        index: usize,
        stat: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![PathSeg::key("stats"), PathSeg::key(stat)],
            value,
        )
    }

    /// Параметр `stats.param.<param>`: значение-число или `{value, pct}`.
    pub fn set_item_param(
        &mut self,
        index: usize,
        param: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![
                PathSeg::key("stats"),
                PathSeg::key("param"),
                PathSeg::key(param),
            ],
            value,
        )
    }

    /// Ключи `stats.param` предмета в порядке файла.
    pub fn item_param_keys(&self, index: usize) -> Vec<String> {
        self.value_at(
            ContentFile::Items,
            &Self::item_path(index, &[PathSeg::key("stats"), PathSeg::key("param")]),
        )
        .and_then(|value| value.as_object().cloned())
        .map(|params| params.keys().cloned().collect())
        .unwrap_or_default()
    }

    /// Добавить первый свободный параметр в `stats.param` (значение 1).
    pub fn add_item_param(&mut self, index: usize) -> Result<String, ProjectError> {
        let used = self.item_param_keys(index);
        let param = ITEM_PARAMS
            .iter()
            .find(|param| !used.iter().any(|used| used == *param))
            .ok_or_else(|| ProjectError::Patch("все параметры уже заданы".into()))?;
        self.set_item_param(index, param, Some(Value::from(1)))?;
        Ok((*param).to_string())
    }

    /// Поле автобаффа `stats.buff`/`stats.buff2` (`param`, `value`,
    /// `duration_s`, `max_stacks`, `pct`); `None` — удалить поле.
    pub fn set_item_buff_field(
        &mut self,
        index: usize,
        buff: &str,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![PathSeg::key("stats"), PathSeg::key(buff), PathSeg::key(key)],
            value,
        )
    }

    /// Удалить автобафф `stats.buff`/`stats.buff2`.
    pub fn remove_item_buff(&mut self, index: usize, buff: &str) -> Result<(), ProjectError> {
        self.item_edit(index, vec![PathSeg::key("stats"), PathSeg::key(buff)], None)
    }

    /// Поле `stats.effect` (`kind`, `value`, `duration_s`, `period_s`).
    pub fn set_item_effect_field(
        &mut self,
        index: usize,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![
                PathSeg::key("stats"),
                PathSeg::key("effect"),
                PathSeg::key(key),
            ],
            value,
        )
    }

    /// Удалить эффект `stats.effect`.
    pub fn remove_item_effect(&mut self, index: usize) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![PathSeg::key("stats"), PathSeg::key("effect")],
            None,
        )
    }

    /// Бонусы предмета (ADR-010) из сырого JSON.
    pub fn item_bonuses(&self, index: usize) -> Vec<Value> {
        self.value_at(
            ContentFile::Items,
            &Self::item_path(index, &[PathSeg::key("bonuses")]),
        )
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
    }

    /// Добавить бонус (ADR-010): первый свободный атрибут/параметр.
    /// Возвращает индекс нового бонуса.
    pub fn add_item_bonus(&mut self, index: usize) -> Result<usize, ProjectError> {
        let bonuses = self.item_bonuses(index);
        let used: Vec<&str> = bonuses
            .iter()
            .filter_map(|bonus| bonus.get("id").and_then(Value::as_str))
            .collect();
        let (kind, id) = ITEM_ATTRS
            .iter()
            .map(|id| ("attr", *id))
            .chain(ITEM_PARAMS.iter().map(|id| ("param", *id)))
            .find(|(_, id)| !used.contains(id))
            .ok_or_else(|| ProjectError::Patch("все атрибуты и параметры уже заданы".into()))?;
        self.apply_edits(
            ContentFile::Items,
            vec![Edit::Push(
                Self::item_path(index, &[PathSeg::key("bonuses")]),
                serde_json::json!({"kind": kind, "id": id, "value": 1}),
            )],
        )?;
        Ok(bonuses.len())
    }

    /// Поле бонуса по индексу: `kind`, `id`, `value`, `pct`; `None` —
    /// удалить необязательное поле (`pct`).
    pub fn set_item_bonus_field(
        &mut self,
        index: usize,
        bonus_index: usize,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![
                PathSeg::key("bonuses"),
                PathSeg::Index(bonus_index),
                PathSeg::key(key),
            ],
            value,
        )
    }

    /// Удалить бонус по индексу.
    pub fn remove_item_bonus(
        &mut self,
        index: usize,
        bonus_index: usize,
    ) -> Result<(), ProjectError> {
        self.apply_edits(
            ContentFile::Items,
            vec![Edit::Remove(Self::item_path(
                index,
                &[PathSeg::key("bonuses"), PathSeg::Index(bonus_index)],
            ))],
        )
    }

    /// Создать пустые требования `requires` (`{}`).
    pub fn add_item_requires(&mut self, index: usize) -> Result<(), ProjectError> {
        self.set_item_field(index, "requires", Some(Value::Object(Default::default())))
    }

    /// Удалить требования целиком.
    pub fn remove_item_requires(&mut self, index: usize) -> Result<(), ProjectError> {
        self.set_item_field(index, "requires", None)
    }

    /// Требование по атрибуту: `requires.attr.<attr>`; `None` — удалить.
    pub fn set_item_requires_attr(
        &mut self,
        index: usize,
        attr: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![
                PathSeg::key("requires"),
                PathSeg::key("attr"),
                PathSeg::key(attr),
            ],
            value,
        )
    }

    /// Требование `requires.profession` / `requires.level`; `None` — удалить.
    pub fn set_item_requires_field(
        &mut self,
        index: usize,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), ProjectError> {
        self.item_edit(
            index,
            vec![PathSeg::key("requires"), PathSeg::key(key)],
            value,
        )
    }

    /// Новый предмет (E6.2): id `item_new_N`, базовые поля, пустые статы.
    pub fn add_item(&mut self) -> Result<usize, ProjectError> {
        let ids: Vec<&str> = self
            .content
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        let id = unique_id(&ids, "item_new_");
        let index = self.add_record(
            ContentFile::Items,
            serde_json::json!({
                "id": id,
                "type": ITEM_WEAPON,
                "name": "Новый предмет",
                "rarity": RARITY_COMMON,
                "upgrade": 0,
                "gold_value": 0,
            }),
        )?;
        Ok(index)
    }

    /// Удалить предмет по индексу.
    pub fn remove_item(&mut self, index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Items, index)
    }

    /// Правка полей локации: `name`, `text`, `flags` (массив строк),
    /// `spawn` (bool), `id` и т.п.
    pub fn set_location_fields(
        &mut self,
        index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| Edit::Set(location_path(index, Some(key)), value))
            .collect();
        self.apply_edits(ContentFile::Locations, edits)
    }

    /// Добавить выход в конец списка локации; возвращает его индекс.
    pub fn add_exit(&mut self, location_index: usize) -> Result<usize, ProjectError> {
        let location = self
            .content
            .locations
            .get(location_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет локации {location_index}")))?;
        let ids: Vec<&str> = location.exits.iter().map(|exit| exit.id.as_str()).collect();
        let id = unique_id(&ids, "e_");
        let to = self
            .content
            .locations
            .first()
            .map(|first| first.id.clone())
            .unwrap_or_default();
        let count = location.exits.len();
        self.apply_edits(
            ContentFile::Locations,
            vec![Edit::Push(
                location_array_path(location_index, "exits"),
                json!({ "id": id, "label": "новый выход", "dir": null, "to": to }),
            )],
        )?;
        Ok(count)
    }

    /// Правка полей выхода: `id`, `label`, `dir`, `to`.
    pub fn update_exit(
        &mut self,
        location_index: usize,
        exit_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| {
                let mut path = location_array_path(location_index, "exits");
                path.push(PathSeg::index(exit_index));
                path.push(PathSeg::key(key));
                Edit::Set(path, value)
            })
            .collect();
        self.apply_edits(ContentFile::Locations, edits)
    }

    /// Удалить выход по индексу.
    pub fn remove_exit(
        &mut self,
        location_index: usize,
        exit_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = location_array_path(location_index, "exits");
        path.push(PathSeg::index(exit_index));
        self.apply_edits(ContentFile::Locations, vec![Edit::Remove(path)])
    }

    /// Связать две локации выходом (E4.2, drag узла на узел).
    ///
    /// Если выход `from → to` уже есть — удаляет его (toggle).
    /// Возвращает `true`, если выход создан, и `false`, если удалён.
    /// `dir` — направление по клеткам раскладки (None — без направления).
    pub fn toggle_exit(
        &mut self,
        from: usize,
        to: usize,
        dir: Option<String>,
    ) -> Result<bool, ProjectError> {
        if from == to {
            return Err(ProjectError::Patch(
                "нельзя связать локацию с самой собой".into(),
            ));
        }
        let (existing, exit_id, to_id, to_name) = {
            let from_location = self
                .content
                .locations
                .get(from)
                .ok_or_else(|| ProjectError::Patch(format!("нет локации {from}")))?;
            let to_location = self
                .content
                .locations
                .get(to)
                .ok_or_else(|| ProjectError::Patch(format!("нет локации {to}")))?;
            let existing = from_location
                .exits
                .iter()
                .position(|exit| exit.to == to_location.id);
            let ids: Vec<&str> = from_location
                .exits
                .iter()
                .map(|exit| exit.id.as_str())
                .collect();
            (
                existing,
                unique_id(&ids, "e_"),
                to_location.id.clone(),
                to_location.name.clone(),
            )
        };
        if let Some(index) = existing {
            self.remove_exit(from, index)?;
            return Ok(false);
        }
        self.apply_edits(
            ContentFile::Locations,
            vec![Edit::Push(
                location_array_path(from, "exits"),
                json!({ "id": exit_id, "label": format!("к {to_name}"), "dir": dir, "to": to_id }),
            )],
        )?;
        Ok(true)
    }

    /// Добавить сущность (`monster`/`npc`/`object`/`resource`/`container`);
    /// возвращает её индекс.
    pub fn add_entity(&mut self, location_index: usize, kind: &str) -> Result<usize, ProjectError> {
        let (prefix, name) = match kind {
            "monster" => ("m_", "Новый монстр"),
            "npc" => ("npc_", "Новый НПС"),
            "object" => ("obj_", "Новый объект"),
            "resource" => ("res_", "Новый ресурс"),
            "container" => ("chest_", "Новый контейнер"),
            other => {
                return Err(ProjectError::Patch(format!(
                    "неизвестный вид сущности: {other}"
                )))
            }
        };
        let location = self
            .content
            .locations
            .get(location_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет локации {location_index}")))?;
        let ids: Vec<&str> = location
            .entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect();
        let id = unique_id(&ids, prefix);
        let count = location.entities.len();
        self.apply_edits(
            ContentFile::Locations,
            vec![Edit::Push(
                location_array_path(location_index, "entities"),
                json!({ "id": id, "kind": kind, "name": name }),
            )],
        )?;
        Ok(count)
    }

    /// Заменить сущность целиком (для формы: id/kind/name + extra-поля
    /// одним объектом; неизвестные поля сохраняются вызывающим).
    pub fn set_entity_object(
        &mut self,
        location_index: usize,
        entity_index: usize,
        entity: Value,
    ) -> Result<(), ProjectError> {
        let mut path = location_array_path(location_index, "entities");
        path.push(PathSeg::index(entity_index));
        self.apply_edits(ContentFile::Locations, vec![Edit::Set(path, entity)])
    }

    /// Удалить сущность по индексу.
    pub fn remove_entity(
        &mut self,
        location_index: usize,
        entity_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = location_array_path(location_index, "entities");
        path.push(PathSeg::index(entity_index));
        self.apply_edits(ContentFile::Locations, vec![Edit::Remove(path)])
    }

    // ─── E5: пикеры ссылок ───────────────────────────────────────────────

    /// Значение по пути к сырому JSON-файлу (для формы/пикера).
    pub fn value_at(&self, file: ContentFile, path: &[PathSeg]) -> Option<Value> {
        get_path(self.raw.get(file), path).cloned()
    }

    /// Варианты для пикера-ссылки (отсортированы по id).
    pub fn choices(&self, kind: RefKind) -> Vec<Choice> {
        let mut choices: Vec<Choice> = Vec::new();
        match kind {
            RefKind::Item => choices.extend(self.content.items.iter().map(|item| Choice {
                id: item.id.clone(),
                label: format!("{} — {} ({})", item.id, item.name, item.item_type),
            })),
            RefKind::ItemType => {
                let mut types: Vec<&str> = self
                    .content
                    .items
                    .iter()
                    .map(|item| item.item_type.as_str())
                    .collect();
                types.sort_unstable();
                types.dedup();
                choices.extend(types.into_iter().map(|item_type| Choice {
                    id: item_type.to_string(),
                    label: item_type.to_string(),
                }));
            }
            RefKind::Skill => choices.extend(self.content.skills.iter().map(|skill| Choice {
                id: skill.id.clone(),
                label: format!("{} — {} ({})", skill.id, skill.name, skill.group),
            })),
            RefKind::Profession => {
                choices.extend(self.content.professions.iter().map(|prof| Choice {
                    id: prof.id.clone(),
                    label: format!("{} — {}", prof.id, prof.name),
                }))
            }
            RefKind::Location => {
                choices.extend(self.content.locations.iter().map(|location| Choice {
                    id: location.id.clone(),
                    label: format!("{} — {}", location.id, location.name),
                }))
            }
            RefKind::Npc => {
                for location in &self.content.locations {
                    for entity in &location.entities {
                        if entity.kind != ld2_content::EntityKind::Npc {
                            continue;
                        }
                        choices.push(Choice {
                            id: entity.id.clone(),
                            label: format!("{} — {} ({})", entity.id, entity.name, location.id),
                        });
                    }
                }
            }
            RefKind::Monster => {
                for location in &self.content.locations {
                    for entity in &location.entities {
                        if entity.kind != ld2_content::EntityKind::Monster {
                            continue;
                        }
                        choices.push(Choice {
                            id: entity.id.clone(),
                            label: format!("{} — {} ({})", entity.id, entity.name, location.id),
                        });
                    }
                }
            }
            RefKind::Station => {
                for location in &self.content.locations {
                    for entity in &location.entities {
                        let Some(station) = entity.extra.get("station").and_then(Value::as_str)
                        else {
                            continue;
                        };
                        choices.push(Choice {
                            id: station.to_string(),
                            label: format!("{} — {} ({})", station, entity.name, location.id),
                        });
                    }
                }
            }
            RefKind::Quest => choices.extend(self.content.quests.iter().map(|quest| Choice {
                id: quest.id.clone(),
                label: format!("{} — {}", quest.id, quest.title),
            })),
            RefKind::DialogLine => {
                for npc in &self.dialogs.npcs {
                    for line in &npc.lines {
                        choices.push(Choice {
                            id: line.id.clone(),
                            label: format!("{} — {} ({})", line.id, short_text(&line.text), npc.id),
                        });
                    }
                }
            }
        }
        choices.sort_by(|a, b| a.id.cmp(&b.id));
        choices
    }

    // ─── E5: общие операции с записями верхнего уровня ───────────────────

    /// Добавить запись в массив верхнего уровня файла; возвращает индекс.
    pub fn add_record(&mut self, file: ContentFile, value: Value) -> Result<usize, ProjectError> {
        let count = self
            .raw
            .get(file)
            .get(file.root_key())
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        self.apply_edits(
            file,
            vec![Edit::Push(vec![PathSeg::key(file.root_key())], value)],
        )?;
        Ok(count)
    }

    /// Удалить запись верхнего уровня по индексу.
    pub fn remove_record(&mut self, file: ContentFile, index: usize) -> Result<(), ProjectError> {
        self.apply_edits(file, vec![Edit::Remove(record_path(file, index))])
    }

    /// Первый предмет (для дефолтов новых записей) — обычно `herb`.
    fn first_item_id(&self) -> String {
        self.content
            .items
            .first()
            .map(|item| item.id.clone())
            .unwrap_or_default()
    }

    /// Первый монстр (дефолт цели квеста).
    fn first_monster_id(&self) -> Value {
        self.content
            .locations
            .iter()
            .flat_map(|location| location.entities.iter())
            .find(|entity| entity.kind == ld2_content::EntityKind::Monster)
            .map(|entity| Value::from(entity.id.clone()))
            .unwrap_or(Value::Null)
    }

    /// Первая профессия (дефолт рецепта).
    fn first_profession_id(&self) -> String {
        self.content
            .professions
            .first()
            .map(|profession| profession.id.clone())
            .unwrap_or_default()
    }

    // ─── E5: квесты ──────────────────────────────────────────────────────

    /// Добавить квест; возвращает его индекс.
    pub fn add_quest(&mut self) -> Result<usize, ProjectError> {
        let ids: Vec<&str> = self.content.quests.iter().map(|q| q.id.as_str()).collect();
        let id = unique_id(&ids, "q_new_");
        self.add_record(
            ContentFile::Quests,
            json!({
                "id": id,
                "title": "Новый квест",
                "goals": [],
                "reward": { "xp": 0, "items": [] },
            }),
        )
    }

    /// Удалить квест по индексу.
    pub fn remove_quest(&mut self, index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Quests, index)
    }

    /// Добавить цель квеста; возвращает её индекс.
    pub fn add_quest_goal(&mut self, quest_index: usize) -> Result<usize, ProjectError> {
        let count = self
            .content
            .quests
            .get(quest_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет квеста {quest_index}")))?
            .goals
            .len();
        let target = self.first_monster_id();
        self.apply_edits(
            ContentFile::Quests,
            vec![Edit::Push(
                record_field_path(ContentFile::Quests, quest_index, &["goals"]),
                json!({ "type": "kill", "target": target, "need": 1, "text": "" }),
            )],
        )?;
        Ok(count)
    }

    /// Правка полей цели квеста: `type`, `target`, `need`, `text`.
    pub fn update_quest_goal(
        &mut self,
        quest_index: usize,
        goal_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| {
                let mut path = record_field_path(ContentFile::Quests, quest_index, &["goals"]);
                path.push(PathSeg::index(goal_index));
                path.push(PathSeg::key(key));
                // null означает «удалить необязательное поле».
                if value.is_null() {
                    Edit::RemoveKey(path)
                } else {
                    Edit::Set(path, value)
                }
            })
            .collect();
        self.apply_edits(ContentFile::Quests, edits)
    }

    /// Удалить цель квеста.
    pub fn remove_quest_goal(
        &mut self,
        quest_index: usize,
        goal_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = record_field_path(ContentFile::Quests, quest_index, &["goals"]);
        path.push(PathSeg::index(goal_index));
        self.apply_edits(ContentFile::Quests, vec![Edit::Remove(path)])
    }

    /// Добавить предмет награды; возвращает его индекс.
    pub fn add_quest_reward_item(&mut self, quest_index: usize) -> Result<usize, ProjectError> {
        let count = self
            .content
            .quests
            .get(quest_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет квеста {quest_index}")))?
            .reward
            .items
            .len();
        let item = self.first_item_id();
        self.apply_edits(
            ContentFile::Quests,
            vec![Edit::Push(
                record_field_path(ContentFile::Quests, quest_index, &["reward", "items"]),
                json!({ "type": item, "qty": 1 }),
            )],
        )?;
        Ok(count)
    }

    /// Правка предмета награды: `type`, `qty`.
    pub fn update_quest_reward_item(
        &mut self,
        quest_index: usize,
        item_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| {
                let mut path =
                    record_field_path(ContentFile::Quests, quest_index, &["reward", "items"]);
                path.push(PathSeg::index(item_index));
                path.push(PathSeg::key(key));
                if value.is_null() {
                    Edit::RemoveKey(path)
                } else {
                    Edit::Set(path, value)
                }
            })
            .collect();
        self.apply_edits(ContentFile::Quests, edits)
    }

    /// Удалить предмет награды.
    pub fn remove_quest_reward_item(
        &mut self,
        quest_index: usize,
        item_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = record_field_path(ContentFile::Quests, quest_index, &["reward", "items"]);
        path.push(PathSeg::index(item_index));
        self.apply_edits(ContentFile::Quests, vec![Edit::Remove(path)])
    }

    // ─── E5: рецепты ─────────────────────────────────────────────────────

    /// Добавить рецепт; возвращает его индекс.
    pub fn add_recipe(&mut self) -> Result<usize, ProjectError> {
        let ids: Vec<&str> = self.content.recipes.iter().map(|r| r.id.as_str()).collect();
        let id = unique_id(&ids, "rec_new_");
        let profession = self.first_profession_id();
        let item = self.first_item_id();
        self.add_record(
            ContentFile::Recipes,
            json!({
                "id": id,
                "name": "Новый рецепт",
                "profession": profession,
                "rank": 1,
                "time_s": 1,
                "xp": { "profession": 0 },
                "output": { "type": item, "qty": 1 },
                "inputs": [],
            }),
        )
    }

    /// Удалить рецепт по индексу.
    pub fn remove_recipe(&mut self, index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Recipes, index)
    }

    /// Добавить ингредиент; возвращает его индекс.
    pub fn add_recipe_input(&mut self, recipe_index: usize) -> Result<usize, ProjectError> {
        let count = self
            .content
            .recipes
            .get(recipe_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет рецепта {recipe_index}")))?
            .inputs
            .len();
        let item = self.first_item_id();
        self.apply_edits(
            ContentFile::Recipes,
            vec![Edit::Push(
                record_field_path(ContentFile::Recipes, recipe_index, &["inputs"]),
                json!({ "type": item, "qty": 1 }),
            )],
        )?;
        Ok(count)
    }

    /// Правка ингредиента: `type`, `qty`.
    pub fn update_recipe_input(
        &mut self,
        recipe_index: usize,
        input_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let edits = fields
            .into_iter()
            .map(|(key, value)| {
                let mut path = record_field_path(ContentFile::Recipes, recipe_index, &["inputs"]);
                path.push(PathSeg::index(input_index));
                path.push(PathSeg::key(key));
                if value.is_null() {
                    Edit::RemoveKey(path)
                } else {
                    Edit::Set(path, value)
                }
            })
            .collect();
        self.apply_edits(ContentFile::Recipes, edits)
    }

    /// Удалить ингредиент.
    pub fn remove_recipe_input(
        &mut self,
        recipe_index: usize,
        input_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = record_field_path(ContentFile::Recipes, recipe_index, &["inputs"]);
        path.push(PathSeg::index(input_index));
        self.apply_edits(ContentFile::Recipes, vec![Edit::Remove(path)])
    }

    // ─── E5: навыки и профессии ──────────────────────────────────────────

    /// Добавить навык; возвращает его индекс.
    pub fn add_skill(&mut self) -> Result<usize, ProjectError> {
        let ids: Vec<&str> = self
            .content
            .skills
            .iter()
            .map(|skill| skill.id.as_str())
            .collect();
        let id = unique_id(&ids, "skill_new_");
        self.add_record(
            ContentFile::Skills,
            json!({ "id": id, "name": "Новый навык", "group": "misc", "description": "" }),
        )
    }

    /// Удалить навык по индексу.
    pub fn remove_skill(&mut self, index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Skills, index)
    }

    /// Добавить профессию; возвращает её индекс.
    pub fn add_profession(&mut self) -> Result<usize, ProjectError> {
        let ids: Vec<&str> = self
            .content
            .professions
            .iter()
            .map(|profession| profession.id.as_str())
            .collect();
        let id = unique_id(&ids, "prof_new_");
        self.add_record(
            ContentFile::Professions,
            json!({ "id": id, "name": "Новая профессия" }),
        )
    }

    /// Удалить профессию по индексу.
    pub fn remove_profession(&mut self, index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Professions, index)
    }

    // ─── E5.2: диалоги ───────────────────────────────────────────────────

    /// Добавить запись диалога NPC (сначала NPC из локаций без диалога);
    /// возвращает индекс.
    pub fn add_dialog_npc(&mut self) -> Result<usize, ProjectError> {
        let existing: Vec<&str> = self
            .dialogs
            .npcs
            .iter()
            .map(|npc| npc.id.as_str())
            .collect();
        let candidate = self
            .content
            .locations
            .iter()
            .flat_map(|location| location.entities.iter())
            .filter(|entity| entity.kind == ld2_content::EntityKind::Npc)
            .map(|entity| entity.id.as_str())
            .find(|id| !existing.contains(id))
            .map(str::to_string);
        let id = candidate.unwrap_or_else(|| unique_id(&existing, "npc_new_"));
        let line_id = DEMO_DIALOG_LINE.to_string();
        let value = json!({
            "id": id,
            "root": line_id,
            "lines": [{
                "id": line_id,
                "text": "Здравствуй, {name}.",
                "replies": [],
            }],
            "training": [],
        });
        self.add_record(ContentFile::Dialogs, value)
    }

    /// Удалить запись диалога NPC.
    pub fn remove_dialog_npc(&mut self, npc_index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Dialogs, npc_index)
    }

    /// Добавить строку диалога; возвращает её индекс.
    pub fn add_dialog_line(&mut self, npc_index: usize) -> Result<usize, ProjectError> {
        let npc = self
            .dialogs
            .npcs
            .get(npc_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет диалога {npc_index}")))?;
        let index = npc.lines.len();
        let ids: Vec<&str> = npc.lines.iter().map(|line| line.id.as_str()).collect();
        let id = unique_id(&ids, "l_new_");
        let value = json!({"id": id, "text": "Новая реплика", "replies": []});
        let path = record_field_path(ContentFile::Dialogs, npc_index, &["lines"]);
        self.apply_edits(ContentFile::Dialogs, vec![Edit::Push(path, value)])?;
        Ok(index)
    }

    /// Правка полей строки диалога (`id`, `text`, `quest`, `every_s`,
    /// `when` JSON); `Value::Null` удаляет необязательное поле.
    pub fn update_dialog_line(
        &mut self,
        npc_index: usize,
        line_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let base = dialog_line_path(npc_index, line_index);
        self.apply_edits(ContentFile::Dialogs, field_edits(base, fields))
    }

    /// Удалить строку диалога.
    pub fn remove_dialog_line(
        &mut self,
        npc_index: usize,
        line_index: usize,
    ) -> Result<(), ProjectError> {
        self.apply_edits(
            ContentFile::Dialogs,
            vec![Edit::Remove(dialog_line_path(npc_index, line_index))],
        )
    }

    /// Добавить реплику игрока в строку; возвращает её индекс.
    pub fn add_dialog_reply(
        &mut self,
        npc_index: usize,
        line_index: usize,
    ) -> Result<usize, ProjectError> {
        let npc = self
            .dialogs
            .npcs
            .get(npc_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет диалога {npc_index}")))?;
        let line = npc
            .lines
            .get(line_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет строки {line_index}")))?;
        let index = line.replies.len();
        let ids: Vec<&str> = npc
            .lines
            .iter()
            .flat_map(|line| line.replies.iter())
            .map(|reply| reply.id.as_str())
            .collect();
        let id = unique_id(&ids, "r_new_");
        let value = json!({"id": id, "text": "Ответ"});
        let path = dialog_line_field_path(npc_index, line_index, &["replies"]);
        self.apply_edits(ContentFile::Dialogs, vec![Edit::Push(path, value)])?;
        Ok(index)
    }

    /// Правка полей реплики (`id`, `text`, `next`, `action`, `when` JSON).
    pub fn update_dialog_reply(
        &mut self,
        npc_index: usize,
        line_index: usize,
        reply_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let base = dialog_reply_path(npc_index, line_index, reply_index);
        self.apply_edits(ContentFile::Dialogs, field_edits(base, fields))
    }

    /// Удалить реплику.
    pub fn remove_dialog_reply(
        &mut self,
        npc_index: usize,
        line_index: usize,
        reply_index: usize,
    ) -> Result<(), ProjectError> {
        self.apply_edits(
            ContentFile::Dialogs,
            vec![Edit::Remove(dialog_reply_path(
                npc_index,
                line_index,
                reply_index,
            ))],
        )
    }

    /// Добавить запись обучения навыку; возвращает индекс.
    pub fn add_training_entry(&mut self, npc_index: usize) -> Result<usize, ProjectError> {
        let npc = self
            .dialogs
            .npcs
            .get(npc_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет диалога {npc_index}")))?;
        let index = npc.training.len();
        let skill = self
            .content
            .skills
            .first()
            .map(|skill| skill.id.clone())
            .unwrap_or_default();
        let value = json!({"skill": skill, "points": 0, "gold": 0});
        let path = record_field_path(ContentFile::Dialogs, npc_index, &["training"]);
        self.apply_edits(ContentFile::Dialogs, vec![Edit::Push(path, value)])?;
        Ok(index)
    }

    /// Правка полей обучения (`skill`, `points`, `gold`).
    pub fn update_training_entry(
        &mut self,
        npc_index: usize,
        entry_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let base = record_field_path(
            ContentFile::Dialogs,
            npc_index,
            &["training", &entry_index.to_string()],
        );
        // training[entry] — индекс, а не ключ: собираем путь вручную.
        let mut path = record_field_path(ContentFile::Dialogs, npc_index, &["training"]);
        path.push(PathSeg::Index(entry_index));
        let _ = base;
        self.apply_edits(ContentFile::Dialogs, field_edits(path, fields))
    }

    /// Удалить запись обучения.
    pub fn remove_training_entry(
        &mut self,
        npc_index: usize,
        entry_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = record_field_path(ContentFile::Dialogs, npc_index, &["training"]);
        path.push(PathSeg::Index(entry_index));
        self.apply_edits(ContentFile::Dialogs, vec![Edit::Remove(path)])
    }

    // ─── E5.2: магазины ──────────────────────────────────────────────────

    /// Добавить магазин NPC (сначала NPC из локаций без магазина).
    pub fn add_shop_npc(&mut self) -> Result<usize, ProjectError> {
        let existing: Vec<&str> = self.shops.npcs.iter().map(|npc| npc.id.as_str()).collect();
        let candidate = self
            .content
            .locations
            .iter()
            .flat_map(|location| location.entities.iter())
            .filter(|entity| entity.kind == ld2_content::EntityKind::Npc)
            .map(|entity| entity.id.as_str())
            .find(|id| !existing.contains(id))
            .map(str::to_string);
        let id = candidate.unwrap_or_else(|| unique_id(&existing, "npc_new_"));
        let value = json!({
            "id": id,
            "markup": 1.0,
            "currencies": [],
        });
        self.add_record(ContentFile::Shops, value)
    }

    /// Удалить магазин NPC.
    pub fn remove_shop_npc(&mut self, shop_index: usize) -> Result<(), ProjectError> {
        self.remove_record(ContentFile::Shops, shop_index)
    }

    /// Добавить валютную вкладку; возвращает её индекс.
    pub fn add_shop_currency(&mut self, shop_index: usize) -> Result<usize, ProjectError> {
        let shop = self
            .shops
            .npcs
            .get(shop_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет магазина {shop_index}")))?;
        let index = shop.currencies.len();
        let currency = self
            .content
            .items
            .iter()
            .find(|item| item.item_type == ITEM_MONEY)
            .or_else(|| self.content.items.first())
            .map(|item| item.id.clone())
            .unwrap_or_default();
        let value = json!({"currency": currency, "rate": 1.0, "items": []});
        let path = record_field_path(ContentFile::Shops, shop_index, &["currencies"]);
        self.apply_edits(ContentFile::Shops, vec![Edit::Push(path, value)])?;
        Ok(index)
    }

    /// Правка валютной вкладки (`currency`, `rate`).
    pub fn update_shop_currency(
        &mut self,
        shop_index: usize,
        currency_index: usize,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), ProjectError> {
        let base = shop_currency_path(shop_index, currency_index);
        self.apply_edits(ContentFile::Shops, field_edits(base, fields))
    }

    /// Удалить валютную вкладку.
    pub fn remove_shop_currency(
        &mut self,
        shop_index: usize,
        currency_index: usize,
    ) -> Result<(), ProjectError> {
        self.apply_edits(
            ContentFile::Shops,
            vec![Edit::Remove(shop_currency_path(shop_index, currency_index))],
        )
    }

    /// Добавить товар во вкладку; возвращает индекс.
    pub fn add_shop_item(
        &mut self,
        shop_index: usize,
        currency_index: usize,
    ) -> Result<usize, ProjectError> {
        let shop = self
            .shops
            .npcs
            .get(shop_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет магазина {shop_index}")))?;
        let tab = shop
            .currencies
            .get(currency_index)
            .ok_or_else(|| ProjectError::Patch(format!("нет вкладки {currency_index}")))?;
        let index = tab.items.len();
        let item = self.first_item_id();
        let path = shop_items_path(shop_index, currency_index);
        self.apply_edits(
            ContentFile::Shops,
            vec![Edit::Push(path, Value::from(item))],
        )?;
        Ok(index)
    }

    /// Заменить товар вкладки (id предмета).
    pub fn update_shop_item(
        &mut self,
        shop_index: usize,
        currency_index: usize,
        item_index: usize,
        item: String,
    ) -> Result<(), ProjectError> {
        let mut path = shop_items_path(shop_index, currency_index);
        path.push(PathSeg::Index(item_index));
        self.apply_edits(ContentFile::Shops, vec![Edit::Set(path, Value::from(item))])
    }

    /// Удалить товар из вкладки.
    pub fn remove_shop_item(
        &mut self,
        shop_index: usize,
        currency_index: usize,
        item_index: usize,
    ) -> Result<(), ProjectError> {
        let mut path = shop_items_path(shop_index, currency_index);
        path.push(PathSeg::Index(item_index));
        self.apply_edits(ContentFile::Shops, vec![Edit::Remove(path)])
    }

    /// Отменить последнюю правку.
    pub fn undo(&mut self) -> bool {
        let Some(changes) = self.undo.pop() else {
            return false;
        };
        for change in changes.iter().rev() {
            undo_change(&mut self.raw, change);
        }
        let _ = self.rebuild();
        self.redo.push(changes);
        true
    }

    /// Повторить отменённую правку.
    pub fn redo(&mut self) -> bool {
        let Some(changes) = self.redo.pop() else {
            return false;
        };
        for change in &changes {
            redo_change(&mut self.raw, change);
        }
        let _ = self.rebuild();
        self.undo.push(changes);
        true
    }

    /// Записать изменённые файлы на диск (атомарно). Фатальные ошибки
    /// валидации блокируют сохранение. Возвращает число файлов.
    pub fn save(&mut self) -> Result<usize, ProjectError> {
        if !self.report.is_valid() {
            return Err(ProjectError::Invalid(self.report.errors.clone()));
        }
        let dirty = self.dirty_files();
        if dirty.is_empty() {
            return Ok(0);
        }
        let written = self.raw.save(&self.content_dir, &dirty)?;
        self.baseline = self.raw.clone();
        Ok(written)
    }

    /// Перечитать файлы с диска (внешние изменения): история правок
    /// сбрасывается — «загрузить заново».
    pub fn reload(&mut self) -> Result<(), ProjectError> {
        let raw = RawContent::load(&self.content_dir)?;
        let content = raw.typed()?;
        let dialogs = raw.typed_dialogs()?;
        let shops = raw.typed_shops()?;
        let (report, version) = build_report_and_version(&raw, &content, &dialogs, &shops)?;
        self.raw = raw.clone();
        self.baseline = raw;
        self.content = content;
        self.dialogs = dialogs;
        self.shops = shops;
        self.report = report;
        self.version = version;
        self.undo.clear();
        self.redo.clear();
        Ok(())
    }

    fn push_undo(&mut self, changes: Vec<Change>) {
        self.redo.clear();
        self.undo.push(changes);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Пересобрать типизированное дерево + отчёт + версию из сырых JSON.
    fn rebuild(&mut self) -> Result<(), ContentError> {
        let content = self.raw.typed()?;
        let dialogs = self.raw.typed_dialogs()?;
        let shops = self.raw.typed_shops()?;
        let (report, version) = build_report_and_version(&self.raw, &content, &dialogs, &shops)?;
        self.content = content;
        self.dialogs = dialogs;
        self.shops = shops;
        self.report = report;
        self.version = version;
        Ok(())
    }
}

/// Путь к полю локации: `["locations", index, key]` (или к самой локации
/// при `key = None`).
fn location_path(index: usize, key: Option<&str>) -> Vec<PathSeg> {
    let mut path = vec![
        PathSeg::key(ContentFile::Locations.root_key()),
        PathSeg::index(index),
    ];
    if let Some(key) = key {
        path.push(PathSeg::key(key));
    }
    path
}

/// Путь к массиву локации: `["locations", index, array]`.
fn location_array_path(index: usize, array: &str) -> Vec<PathSeg> {
    location_path(index, Some(array))
}

/// Путь к записи массива верхнего уровня: `[root, index]`.
fn record_path(file: ContentFile, index: usize) -> Vec<PathSeg> {
    vec![PathSeg::key(file.root_key()), PathSeg::index(index)]
}

/// Путь к полю записи: `[root, index, key, ...]`.
fn record_field_path(file: ContentFile, index: usize, keys: &[&str]) -> Vec<PathSeg> {
    let mut path = record_path(file, index);
    path.extend(keys.iter().map(|key| PathSeg::key(*key)));
    path
}

/// Уникальный id с префиксом: `prefixN`, где N — максимум существующих + 1.
fn unique_id(existing: &[&str], prefix: &str) -> String {
    let max = existing
        .iter()
        .filter_map(|id| id.strip_prefix(prefix))
        .filter_map(|suffix| suffix.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", max + 1)
}

/// Правки полей записи: `Null` → удалить необязательное поле.
fn field_edits(base: Vec<PathSeg>, fields: Vec<(&str, Value)>) -> Vec<Edit> {
    fields
        .into_iter()
        .map(|(key, value)| {
            let mut path = base.clone();
            path.push(PathSeg::key(key));
            if value.is_null() {
                Edit::RemoveKey(path)
            } else {
                Edit::Set(path, value)
            }
        })
        .collect()
}

/// Подпись строки диалога в пикере (короткий текст).
fn short_text(text: &str) -> String {
    let mut label: String = text.chars().take(DIALOG_LABEL_MAX).collect();
    if text.chars().count() > DIALOG_LABEL_MAX {
        label.push('…');
    }
    label
}

/// Путь к строке диалога: `[npcs, npc, lines, line]`.
fn dialog_line_path(npc_index: usize, line_index: usize) -> Vec<PathSeg> {
    let mut path = record_field_path(ContentFile::Dialogs, npc_index, &["lines"]);
    path.push(PathSeg::Index(line_index));
    path
}

/// Путь к полю строки диалога: `[npcs, npc, lines, line, key]`.
fn dialog_line_field_path(npc_index: usize, line_index: usize, keys: &[&str]) -> Vec<PathSeg> {
    let mut path = dialog_line_path(npc_index, line_index);
    path.extend(keys.iter().map(|key| PathSeg::key(*key)));
    path
}

/// Путь к реплике: `[npcs, npc, lines, line, replies, reply]`.
fn dialog_reply_path(npc_index: usize, line_index: usize, reply_index: usize) -> Vec<PathSeg> {
    let mut path = dialog_line_field_path(npc_index, line_index, &["replies"]);
    path.push(PathSeg::Index(reply_index));
    path
}

/// Путь к валютной вкладке магазина: `[npcs, shop, currencies, tab]`.
fn shop_currency_path(shop_index: usize, currency_index: usize) -> Vec<PathSeg> {
    let mut path = record_field_path(ContentFile::Shops, shop_index, &["currencies"]);
    path.push(PathSeg::Index(currency_index));
    path
}

/// Путь к массиву товаров вкладки: `[npcs, shop, currencies, tab, items]`.
fn shop_items_path(shop_index: usize, currency_index: usize) -> Vec<PathSeg> {
    let mut path = shop_currency_path(shop_index, currency_index);
    path.push(PathSeg::key("items"));
    path
}

/// Отчёт валидации и версия контента по типизированным вьюхам.
fn build_report_and_version(
    raw: &RawContent,
    content: &GameContent,
    dialogs: &ContentDialogs,
    shops: &ContentShops,
) -> Result<(ValidationReport, String), ContentError> {
    let mut report = validate_report(content);
    report.warnings.extend(validate_references(content));
    report.warnings.extend(validate_items(content));
    let npc_report = validate_npc_content(dialogs, shops, content);
    report.errors.extend(npc_report.errors);
    report.warnings.extend(npc_report.warnings);
    let version = version_raw(raw)?;
    Ok((report, version))
}

/// Корень репозитория и каталог `content/` для пути (репо или сам каталог).
fn resolve_dirs(path: &Path) -> Option<(PathBuf, PathBuf)> {
    if path.join("content").join("items.json").is_file() {
        Some((path.to_path_buf(), path.join("content")))
    } else if path.join("items.json").is_file() {
        let root = path.parent().unwrap_or(path).to_path_buf();
        Some((root, path.to_path_buf()))
    } else {
        None
    }
}

// ─── низкоуровневые операции с JSON ───────────────────────────────────────

fn get_path<'a>(value: &'a Value, path: &[PathSeg]) -> Option<&'a Value> {
    let mut current = value;
    for seg in path {
        current = match seg {
            PathSeg::Key(key) => current.get(key)?,
            PathSeg::Index(index) => current.get(*index)?,
        };
    }
    Some(current)
}

fn set_path(value: &mut Value, path: &[PathSeg], new: Value) -> Result<(), ProjectError> {
    if path.is_empty() {
        return Err(ProjectError::Patch("пустой путь".into()));
    }
    let (last, parents) = path.split_last().expect("путь не пуст");
    let mut current = value;
    for seg in parents {
        current = match seg {
            PathSeg::Key(key) => {
                // Промежуточный объект создаётся, если ключа нет: формы
                // редактора пишут необязательные вложенные поля (`tool.type`).
                let object = current
                    .as_object_mut()
                    .ok_or_else(|| ProjectError::Patch(format!("не объект для ключа {key}")))?;
                object
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()))
            }
            PathSeg::Index(index) => current
                .get_mut(*index)
                .ok_or_else(|| ProjectError::Patch(format!("нет индекса {index}")))?,
        };
    }
    match last {
        PathSeg::Key(key) => {
            let object = current
                .as_object_mut()
                .ok_or_else(|| ProjectError::Patch(format!("не объект для ключа {key}")))?;
            object.insert(key.clone(), new);
        }
        PathSeg::Index(index) => {
            let array = current
                .as_array_mut()
                .ok_or_else(|| ProjectError::Patch(format!("не массив для индекса {index}")))?;
            let slot = array
                .get_mut(*index)
                .ok_or_else(|| ProjectError::Patch(format!("нет индекса {index}")))?;
            *slot = new;
        }
    }
    Ok(())
}

/// Массив по пути (путь указывает на массив).
fn array_at_mut<'a>(
    value: &'a mut Value,
    path: &[PathSeg],
) -> Result<&'a mut Vec<Value>, ProjectError> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| ProjectError::Patch("пустой путь".into()))?;
    let mut current = value;
    for seg in parents {
        current = match seg {
            PathSeg::Key(key) => current
                .get_mut(key)
                .ok_or_else(|| ProjectError::Patch(format!("нет ключа {key}")))?,
            PathSeg::Index(index) => current
                .get_mut(*index)
                .ok_or_else(|| ProjectError::Patch(format!("нет индекса {index}")))?,
        };
    }
    match last {
        PathSeg::Key(key) => current
            .get_mut(key)
            .and_then(|value| value.as_array_mut())
            .ok_or_else(|| ProjectError::Patch(format!("не массив: {key}"))),
        PathSeg::Index(_) => Err(ProjectError::Patch(
            "путь должен указывать на массив".into(),
        )),
    }
}

fn push_at(value: &mut Value, path: &[PathSeg], new: Value) -> Result<(), ProjectError> {
    array_at_mut(value, path)?.push(new);
    Ok(())
}

fn remove_at(value: &mut Value, path: &[PathSeg]) -> Result<Value, ProjectError> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| ProjectError::Patch("пустой путь".into()))?;
    let PathSeg::Index(index) = last else {
        return Err(ProjectError::Patch(
            "путь должен указывать на элемент массива".into(),
        ));
    };
    let mut current = value;
    for seg in parents {
        current = match seg {
            PathSeg::Key(key) => current
                .get_mut(key)
                .ok_or_else(|| ProjectError::Patch(format!("нет ключа {key}")))?,
            PathSeg::Index(index) => current
                .get_mut(*index)
                .ok_or_else(|| ProjectError::Patch(format!("нет индекса {index}")))?,
        };
    }
    let array = current
        .as_array_mut()
        .ok_or_else(|| ProjectError::Patch("не массив".into()))?;
    if *index >= array.len() {
        return Err(ProjectError::Patch(format!("нет индекса {index}")));
    }
    Ok(array.remove(*index))
}

fn pop_at(value: &mut Value, path: &[PathSeg]) -> Option<Value> {
    array_at_mut(value, path).ok()?.pop()
}

fn insert_at(value: &mut Value, path: &[PathSeg], item: Value) -> Result<(), ProjectError> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| ProjectError::Patch("пустой путь".into()))?;
    let PathSeg::Index(index) = last else {
        return Err(ProjectError::Patch(
            "путь должен указывать на элемент массива".into(),
        ));
    };
    let mut current = value;
    for seg in parents {
        current = match seg {
            PathSeg::Key(key) => current
                .get_mut(key)
                .ok_or_else(|| ProjectError::Patch(format!("нет ключа {key}")))?,
            PathSeg::Index(index) => current
                .get_mut(*index)
                .ok_or_else(|| ProjectError::Patch(format!("нет индекса {index}")))?,
        };
    }
    let array = current
        .as_array_mut()
        .ok_or_else(|| ProjectError::Patch("не массив".into()))?;
    if *index > array.len() {
        return Err(ProjectError::Patch(format!("нет индекса {index}")));
    }
    array.insert(*index, item);
    Ok(())
}

fn undo_change(raw: &mut RawContent, change: &Change) {
    let file = change.file();
    match change {
        Change::Set { path, old, .. } => restore_path(raw.get_mut(file), path, old),
        Change::Push { path, .. } => {
            pop_at(raw.get_mut(file), path);
        }
        Change::Remove { path, old, .. } => {
            let _ = insert_at(raw.get_mut(file), path, old.clone());
        }
        Change::RemoveKey { path, old, .. } => {
            if let Some(old) = old {
                let _ = set_path(raw.get_mut(file), path, old.clone());
            } else {
                remove_path(raw.get_mut(file), path);
            }
        }
    }
}

fn redo_change(raw: &mut RawContent, change: &Change) {
    let file = change.file();
    match change {
        Change::Set { path, new, .. } => {
            let _ = set_path(raw.get_mut(file), path, new.clone());
        }
        Change::Push { path, new, .. } => {
            let _ = push_at(raw.get_mut(file), path, new.clone());
        }
        Change::Remove { path, .. } => {
            let _ = remove_at(raw.get_mut(file), path);
        }
        Change::RemoveKey { path, .. } => {
            remove_path(raw.get_mut(file), path);
        }
    }
}

fn restore_path(value: &mut Value, path: &[PathSeg], old: &Option<Value>) {
    match old {
        Some(old) => {
            let _ = set_path(value, path, old.clone());
        }
        None => remove_path(value, path),
    }
}

fn remove_path(value: &mut Value, path: &[PathSeg]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = value;
    for seg in parents {
        current = match seg {
            PathSeg::Key(key) => match current.get_mut(key) {
                Some(next) => next,
                None => return,
            },
            PathSeg::Index(index) => match current.get_mut(*index) {
                Some(next) => next,
                None => return,
            },
        };
    }
    match last {
        PathSeg::Key(key) => {
            if let Some(object) = current.as_object_mut() {
                object.remove(key);
            }
        }
        PathSeg::Index(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_content() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
    }

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ld2-editor-project-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let content = root.join("content");
        std::fs::create_dir_all(&content).expect("временный каталог");
        for file in ContentFile::ALL {
            std::fs::copy(repo_content().join(file.name()), content.join(file.name()))
                .expect("копия контента");
        }
        root
    }

    fn gold_coin_index(project: &EditorProject) -> usize {
        project
            .content()
            .items
            .iter()
            .position(|item| item.id == "gold_coin")
            .expect("gold_coin в контенте")
    }

    fn location_index(project: &EditorProject, id: &str) -> usize {
        project
            .content()
            .locations
            .iter()
            .position(|location| location.id == id)
            .expect("локация в контенте")
    }

    #[test]
    fn open_edit_undo_redo_save() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        assert!(!project.is_dirty());
        assert!(project.report().is_valid(), "{:?}", project.report());
        let index = gold_coin_index(&project);

        project
            .set_item_fields(
                index,
                vec![
                    ("name", Value::from("Золотая монета (проба)")),
                    ("gold_value", Value::from(2)),
                ],
            )
            .expect("правка применяется");
        assert!(project.is_dirty());
        assert_eq!(project.dirty_files(), vec![ContentFile::Items]);
        assert_eq!(
            project.content().items[index].name,
            "Золотая монета (проба)"
        );
        assert_eq!(project.content().items[index].gold_value, 2);

        assert!(project.undo());
        assert!(!project.is_dirty(), "undo вернул исходное состояние");
        assert!(project.redo());
        assert!(project.is_dirty());

        let written = project.save().expect("сохранение");
        assert_eq!(written, 1);
        assert!(!project.is_dirty(), "после save baseline обновлён");
        let saved = std::fs::read_to_string(root.join("content/items.json")).expect("файл");
        assert!(saved.contains("Золотая монета (проба)"));
        assert_eq!(project.save().expect("повторное сохранение"), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_edit_rolls_back_and_bad_content_blocks_save() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let index = gold_coin_index(&project);
        let original = project.content().items[index].name.clone();

        let err = project.set_item_fields(index, vec![("name", Value::from(42))]);
        assert!(err.is_err(), "число в name должно отклоняться");
        assert_eq!(project.content().items[index].name, original);
        assert!(!project.is_dirty(), "неудачная правка не оставила следов");

        project
            .set_item_fields(index, vec![("id", Value::from("sword_knight"))])
            .expect("правка синтаксически корректна");
        assert!(
            !project.report().is_valid(),
            "дубликат id — фатальная ошибка"
        );
        assert!(matches!(project.save(), Err(ProjectError::Invalid(_))));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toggle_exit_creates_and_removes_with_undo() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let from = location_index(&project, "l_village");
        let to = location_index(&project, "l_forge");
        let before = project.content().locations[from].exits.len();

        // создание: id уникален, направление и адресат записаны
        assert!(project
            .toggle_exit(from, to, Some("north".into()))
            .expect("выход создаётся"));
        assert_eq!(project.content().locations[from].exits.len(), before + 1);
        let exit = project.content().locations[from]
            .exits
            .last()
            .expect("новый выход")
            .clone();
        assert_eq!(exit.to, "l_forge");
        assert_eq!(exit.dir.as_deref(), Some("north"));
        assert!(exit.id.starts_with("e_"));

        // повторный drag → toggle: выход удаляется
        assert!(!project
            .toggle_exit(from, to, None)
            .expect("выход удаляется"));
        assert_eq!(project.content().locations[from].exits.len(), before);

        // undo удаления возвращает выход, redo снова убирает
        assert!(project.undo());
        assert_eq!(project.content().locations[from].exits.len(), before + 1);
        assert!(project.redo());
        assert_eq!(project.content().locations[from].exits.len(), before);

        // самосвязь запрещена и не оставляет правок
        assert!(project.toggle_exit(from, from, None).is_err());
        assert!(!project.is_dirty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dirty_files_track_each_file() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        project
            .set_location_fields(0, vec![("name", Value::from("Деревня Изба (проба)"))])
            .expect("правка локации");
        assert_eq!(project.dirty_files(), vec![ContentFile::Locations]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn location_exit_add_edit_remove_undo() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let loc = location_index(&project, "l_village");
        let exits_before = project.content().locations[loc].exits.len();

        let new_index = project.add_exit(loc).expect("выход добавляется");
        assert_eq!(new_index, exits_before);
        assert_eq!(
            project.content().locations[loc].exits.len(),
            exits_before + 1
        );
        let new_id = project.content().locations[loc].exits[new_index].id.clone();
        assert!(new_id.starts_with("e_"), "id выхода: {new_id}");

        project
            .update_exit(
                loc,
                new_index,
                vec![
                    ("label", Value::from("проба выхода")),
                    ("to", Value::from("l_village")),
                ],
            )
            .expect("выход правится");
        assert_eq!(
            project.content().locations[loc].exits[new_index].label,
            "проба выхода"
        );

        // undo правки → добавить ещё раз, чтобы проверить undo добавления
        project
            .remove_exit(loc, new_index)
            .expect("выход удаляется");
        assert_eq!(project.content().locations[loc].exits.len(), exits_before);
        assert!(project.undo(), "undo удаления возвращает выход");
        assert_eq!(
            project.content().locations[loc].exits.len(),
            exits_before + 1
        );
        assert!(project.undo(), "undo правки выхода (выход остаётся)");
        assert_eq!(
            project.content().locations[loc].exits.len(),
            exits_before + 1
        );
        assert!(project.undo(), "undo добавления убирает выход");
        assert_eq!(project.content().locations[loc].exits.len(), exits_before);
        assert!(project.redo(), "redo добавляет выход обратно");
        assert_eq!(
            project.content().locations[loc].exits.len(),
            exits_before + 1
        );

        // Сохранение пишет только locations.json.
        assert_eq!(project.dirty_files(), vec![ContentFile::Locations]);
        assert_eq!(project.save().expect("сохранение"), 1);
        let saved = std::fs::read_to_string(root.join("content/locations.json")).expect("файл");
        assert!(saved.contains("проба выхода") || saved.contains(&new_id));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entity_add_edit_remove_and_validation() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let loc = location_index(&project, "l_village");
        let entities_before = project.content().locations[loc].entities.len();

        let index = project
            .add_entity(loc, "monster")
            .expect("монстр добавляется");
        assert_eq!(index, entities_before);
        assert_eq!(
            format!(
                "{:?}",
                project.content().locations[loc].entities[index].kind
            ),
            "Monster"
        );
        let mut object = serde_json::to_value(&project.content().locations[loc].entities[index])
            .expect("сериализация сущности");
        object["name"] = Value::from("Пробный волк");
        object["difficulty"] = Value::from(3);
        project
            .set_entity_object(loc, index, object)
            .expect("сущность правится");
        assert_eq!(
            project.content().locations[loc].entities[index].name,
            "Пробный волк"
        );

        // Дубликат id сущности — фатальная ошибка (валидация).
        let existing_id = project.content().locations[loc].entities[0].id.clone();
        let mut object = serde_json::to_value(&project.content().locations[loc].entities[index])
            .expect("сериализация сущности");
        object["id"] = Value::from(existing_id.clone());
        project
            .set_entity_object(loc, index, object)
            .expect("правка синтаксически корректна");
        assert!(!project.report().is_valid(), "дубликат сущности — ошибка");
        assert!(project.save().is_err());
        assert!(project.undo(), "undo возвращает уникальный id");
        assert!(project.report().is_valid());

        project
            .remove_entity(loc, index)
            .expect("сущность удаляется");
        assert_eq!(
            project.content().locations[loc].entities.len(),
            entities_before
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quest_ops_with_undo_and_references() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let quests_before = project.content().quests.len();
        let qi = project.add_quest().expect("квест добавляется");
        assert_eq!(project.content().quests.len(), quests_before + 1);
        assert!(project.content().quests[qi].id.starts_with("q_new_"));

        let gi = project.add_quest_goal(qi).expect("цель добавляется");
        assert_eq!(gi, 0);
        project
            .update_quest_goal(
                qi,
                gi,
                vec![("need", json!(2)), ("text", json!("Убить волка"))],
            )
            .expect("цель правится");
        assert_eq!(project.content().quests[qi].goals[gi].need, 2);

        let wi = project
            .add_quest_reward_item(qi)
            .expect("награда добавляется");
        project
            .update_quest_reward_item(
                qi,
                wi,
                vec![("type", json!("potion_heal")), ("qty", json!(2))],
            )
            .expect("награда правится");
        assert_eq!(project.content().quests[qi].reward.items[wi].qty, 2);

        project
            .remove_quest_reward_item(qi, wi)
            .expect("награда удаляется");
        project.remove_quest_goal(qi, gi).expect("цель удаляется");
        assert!(project.undo(), "undo удаления цели");
        assert_eq!(project.content().quests[qi].goals.len(), 1);
        assert!(project.undo(), "undo удаления награды");
        assert_eq!(project.content().quests[qi].reward.items.len(), 1);

        project.remove_quest(qi).expect("квест удаляется");
        assert_eq!(project.content().quests.len(), quests_before);
        assert!(project.undo(), "undo удаления квеста");
        assert_eq!(project.content().quests.len(), quests_before + 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recipe_input_ops_and_remove() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let recipes_before = project.content().recipes.len();
        let ri = project.add_recipe().expect("рецепт добавляется");
        assert!(project.content().recipes[ri].id.starts_with("rec_new_"));
        let ii = project
            .add_recipe_input(ri)
            .expect("ингредиент добавляется");
        project
            .update_recipe_input(ri, ii, vec![("type", json!("herb")), ("qty", json!(3))])
            .expect("ингредиент правится");
        assert_eq!(project.content().recipes[ri].inputs[ii].item_type, "herb");
        assert_eq!(project.content().recipes[ri].inputs[ii].qty, 3);

        project
            .remove_recipe_input(ri, ii)
            .expect("ингредиент удаляется");
        assert!(project.content().recipes[ri].inputs.is_empty());
        project.remove_recipe(ri).expect("рецепт удаляется");
        assert_eq!(project.content().recipes.len(), recipes_before);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_and_profession_ops() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let skills_before = project.content().skills.len();
        let si = project.add_skill().expect("навык добавляется");
        assert!(project.content().skills[si].id.starts_with("skill_new_"));
        project
            .apply(vec![(
                ContentFile::Skills,
                vec![
                    PathSeg::key(ContentFile::Skills.root_key()),
                    PathSeg::index(si),
                    PathSeg::key("group"),
                ],
                json!("combat"),
            )])
            .expect("поле навыка правится");
        assert_eq!(project.content().skills[si].group, "combat");
        project.remove_skill(si).expect("навык удаляется");
        assert_eq!(project.content().skills.len(), skills_before);

        let profs_before = project.content().professions.len();
        let pi = project.add_profession().expect("профессия добавляется");
        assert!(project.content().professions[pi]
            .id
            .starts_with("prof_new_"));
        project.remove_profession(pi).expect("профессия удаляется");
        assert_eq!(project.content().professions.len(), profs_before);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_key_restores_optional_field_on_undo() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let qi = project
            .content()
            .quests
            .iter()
            .position(|quest| quest.id == "q_wolf_hunt")
            .expect("квест репозитория");
        assert!(project.content().quests[qi].giver.is_some());
        let path = vec![
            PathSeg::key(ContentFile::Quests.root_key()),
            PathSeg::index(qi),
            PathSeg::key("giver"),
        ];
        project
            .apply_edits(ContentFile::Quests, vec![Edit::RemoveKey(path)])
            .expect("поле удаляется");
        assert!(project.content().quests[qi].giver.is_none());
        assert!(project.undo(), "undo возвращает необязательное поле");
        assert_eq!(
            project.content().quests[qi].giver.as_deref(),
            Some("npc_elder")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn picker_choices_cover_all_reference_kinds() {
        let root = temp_root();
        let project = EditorProject::open(&root).expect("проект открывается");
        let ids = |kind: RefKind| -> Vec<String> {
            project
                .choices(kind)
                .into_iter()
                .map(|choice| choice.id)
                .collect()
        };
        assert!(ids(RefKind::Item).iter().any(|id| id == "herb"));
        assert!(ids(RefKind::ItemType).iter().any(|id| id == "flask"));
        assert!(ids(RefKind::Skill).iter().any(|id| id == "sword"));
        assert!(ids(RefKind::Profession).iter().any(|id| id == "alchemist"));
        assert!(ids(RefKind::Location).iter().any(|id| id == "l_village"));
        assert!(ids(RefKind::Npc).iter().any(|id| id == "npc_elder"));
        assert!(ids(RefKind::Monster).iter().any(|id| id == "m_3"));
        assert!(ids(RefKind::Station).iter().any(|id| id == "alchemy_table"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_reference_becomes_project_warning() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let qi = project
            .content()
            .quests
            .iter()
            .position(|quest| quest.id == "q_wolf_hunt")
            .expect("квест репозитория");
        project
            .apply(vec![(
                ContentFile::Quests,
                vec![
                    PathSeg::key(ContentFile::Quests.root_key()),
                    PathSeg::index(qi),
                    PathSeg::key("giver"),
                ],
                json!("npc_no_such"),
            )])
            .expect("правка применяется");
        assert!(project.report().errors.is_empty());
        assert!(
            project
                .report()
                .warnings
                .iter()
                .any(|warning| warning.contains("npc_no_such")),
            "{:?}",
            project.report()
        );
        assert!(project.save().is_ok(), "warn не блокирует сохранение");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dangling_exit_is_warning_not_error() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let loc = location_index(&project, "l_village");
        let exit_index = project.add_exit(loc).expect("выход добавляется");
        project
            .update_exit(
                loc,
                exit_index,
                vec![("to", Value::from("no_such_location"))],
            )
            .expect("правка применяется");
        assert!(project.report().errors.is_empty());
        assert!(
            !project.report().warnings.is_empty(),
            "висячий выход — warn"
        );
        assert!(project.save().is_ok(), "warn не блокирует сохранение");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dialog_ops_with_undo_and_save() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let npc_index = project.add_dialog_npc().expect("диалог добавлен");
        let line_index = project.add_dialog_line(npc_index).expect("строка");
        project
            .add_dialog_reply(npc_index, line_index)
            .expect("реплика");
        project
            .update_dialog_line(
                npc_index,
                line_index,
                vec![("text", Value::from("Привет, путник."))],
            )
            .expect("правка строки");
        assert!(project.is_dirty());
        assert_eq!(
            project.dialogs().npcs[npc_index].lines[line_index].text,
            "Привет, путник."
        );
        assert!(project.undo(), "undo");
        assert_eq!(
            project.dialogs().npcs[npc_index].lines[line_index].text,
            "Новая реплика"
        );
        assert!(project.redo(), "redo");
        assert_eq!(
            project.dialogs().npcs[npc_index].lines[line_index].text,
            "Привет, путник."
        );
        project.save().expect("сохранение");
        let reopened = EditorProject::open(&root).expect("переоткрытие");
        assert!(reopened
            .dialogs()
            .npcs
            .iter()
            .any(|npc| npc.lines.iter().any(|line| line.text == "Привет, путник.")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shop_ops_with_currency_and_item() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let shop_index = project.add_shop_npc().expect("магазин");
        let currency_index = project.add_shop_currency(shop_index).expect("валюта");
        let item_index = project
            .add_shop_item(shop_index, currency_index)
            .expect("товар");
        project
            .update_shop_item(shop_index, currency_index, item_index, "potion_heal".into())
            .expect("правка товара");
        assert_eq!(
            project.shops().npcs[shop_index].currencies[currency_index].items[item_index],
            "potion_heal"
        );
        assert!(project.undo(), "undo");
        project
            .remove_shop_item(shop_index, currency_index, item_index)
            .expect("удаление товара");
        assert!(project.shops().npcs[shop_index].currencies[currency_index]
            .items
            .is_empty());
        project.save().expect("сохранение");
        let reopened = EditorProject::open(&root).expect("переоткрытие");
        assert!(reopened.shops().npcs.iter().any(|npc| npc
            .currencies
            .iter()
            .any(|currency| currency.items.is_empty())));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ─── E6.2: полный редактор предмета ──────────────────────────────────

    fn item_index(project: &EditorProject, id: &str) -> usize {
        project
            .content()
            .items
            .iter()
            .position(|item| item.id == id)
            .unwrap_or_else(|| panic!("нет предмета {id}"))
    }

    #[test]
    fn item_stat_and_bonus_ops_with_undo() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let axe = item_index(&project, "drakkar_axe");
        let bonuses_before = project.content().items[axe].bonuses.len();

        // бонус добавляется первым свободным атрибутом/параметром
        let bonus_index = project.add_item_bonus(axe).expect("бонус");
        assert_eq!(bonus_index, bonuses_before);
        let bonuses = project.item_bonuses(axe);
        assert_eq!(bonuses.len(), bonuses_before + 1);
        project
            .set_item_bonus_field(axe, bonus_index, "value", Some(Value::from(7)))
            .expect("значение бонуса");
        assert_eq!(project.item_bonuses(axe)[bonus_index]["value"], 7);
        project
            .set_item_bonus_field(axe, bonus_index, "pct", Some(Value::from(true)))
            .expect("pct");

        // параметр статов: число и {value,pct}
        project
            .set_item_param(axe, "crit_chance", Some(Value::from(3)))
            .expect("параметр");
        assert_eq!(
            project.content().items[axe].stats["param"]["crit_chance"],
            3
        );
        let param = project.add_item_param(axe).expect("новый параметр");
        assert!(project.item_param_keys(axe).contains(&param));

        // undo возвращает состояние до последней операции
        assert!(project.undo(), "undo");
        assert!(!project.item_param_keys(axe).contains(&param));

        project
            .remove_item_bonus(axe, bonus_index)
            .expect("удаление бонуса");
        assert_eq!(project.content().items[axe].bonuses.len(), bonuses_before);

        project.save().expect("сохранение");
        let reopened = EditorProject::open(&root).expect("переоткрытие");
        assert_eq!(reopened.content().items[axe].bonuses.len(), bonuses_before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn item_buff_effect_and_requires_are_structural() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let rune = project
            .content()
            .items
            .iter()
            .position(|item| item.item_type == "rune")
            .expect("руна");

        project
            .set_item_buff_field(rune, "buff", "param", Some(Value::from("damage")))
            .expect("param баффа");
        project
            .set_item_buff_field(rune, "buff", "value", Some(Value::from(5)))
            .expect("value баффа");
        project
            .set_item_buff_field(rune, "buff2", "duration_s", Some(Value::from(12)))
            .expect("второй бафф");
        let stats = &project.content().items[rune].stats;
        assert_eq!(stats["buff"]["param"], "damage");
        assert_eq!(stats["buff2"]["duration_s"], 12);

        project
            .set_item_effect_field(rune, "kind", Some(Value::from("burn")))
            .expect("вид эффекта");
        project
            .set_item_effect_field(rune, "duration_s", Some(Value::from(6)))
            .expect("длительность");
        assert_eq!(
            project.content().items[rune].stats["effect"]["kind"],
            "burn"
        );
        project.remove_item_effect(rune).expect("эффект удалён");
        assert!(project.content().items[rune].stats.get("effect").is_none());

        project.add_item_requires(rune).expect("требования");
        project
            .set_item_requires_attr(rune, "int", Some(Value::from(5)))
            .expect("требование int");
        project
            .set_item_requires_field(rune, "level", Some(Value::from(10)))
            .expect("уровень");
        let requires = project.content().items[rune]
            .requires
            .as_ref()
            .expect("requires");
        assert_eq!(requires.attr["int"], 5);
        assert_eq!(requires.level, Some(10));

        assert!(project.undo(), "undo уровня");
        assert_eq!(
            project.content().items[rune]
                .requires
                .as_ref()
                .and_then(|requires| requires.level),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn add_and_remove_item_with_undo() {
        let root = temp_root();
        let mut project = EditorProject::open(&root).expect("проект открывается");
        let before = project.content().items.len();

        let index = project.add_item().expect("предмет");
        assert_eq!(project.content().items.len(), before + 1);
        let id = project.content().items[index].id.clone();
        assert!(id.starts_with("item_new_"));
        project
            .set_item_field(index, "rarity", Some(Value::from("rare")))
            .expect("редкость");
        assert_eq!(project.content().items[index].rarity, "rare");
        assert!(
            project.report().warnings.is_empty(),
            "новый предмет не даёт предупреждений: {:?}",
            project.report().warnings
        );

        project.remove_item(index).expect("удаление предмета");
        assert_eq!(project.content().items.len(), before);
        assert!(project.undo(), "undo удаления");
        assert_eq!(project.content().items.len(), before + 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
