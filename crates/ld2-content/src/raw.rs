//! Сырые JSON-файлы контента: сохранение **без нормализации**.
//!
//! Редактор правит конкретные поля/элементы в [`serde_json::Value`] с
//! сохранением порядка ключей (`preserve_order`) и пишет файл заново
//! pretty-форматом (2 пробела + перевод строки) — diff в git остаётся
//! минимальным: меняются только отредактированные строки.
//!
//! Типизированное представление ([`GameContent`]) строится из тех же
//! значений для валидации/UI/версии; версия считается по каноническому
//! дереву и не зависит от форматирования файлов.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::load::{ItemsRoot, LocationsRoot, ProfessionsRoot, QuestsRoot, RecipesRoot, SkillsRoot};
use crate::model::{ContentDialogs, ContentShops, GameContent};
use crate::ContentError;

/// Файл каталога контента, редактируемый редактором (8 файлов каталога).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentFile {
    Locations,
    Skills,
    Items,
    Professions,
    Recipes,
    Quests,
    Dialogs,
    Shops,
}

impl ContentFile {
    /// Файлы, участвующие в версии/сиде (`GameContent`): 6 обязательных.
    pub const SEED: [ContentFile; 6] = [
        ContentFile::Locations,
        ContentFile::Skills,
        ContentFile::Items,
        ContentFile::Professions,
        ContentFile::Recipes,
        ContentFile::Quests,
    ];

    /// Все файлы каталога контента (6 сида + диалоги/магазины).
    pub const ALL: [ContentFile; 8] = [
        ContentFile::Locations,
        ContentFile::Skills,
        ContentFile::Items,
        ContentFile::Professions,
        ContentFile::Recipes,
        ContentFile::Quests,
        ContentFile::Dialogs,
        ContentFile::Shops,
    ];

    /// Ключ-корень JSON-объекта файла (`{"items": [...]}` → `items`).
    pub fn root_key(self) -> &'static str {
        match self {
            ContentFile::Locations => "locations",
            ContentFile::Skills => "skills",
            ContentFile::Items => "items",
            ContentFile::Professions => "professions",
            ContentFile::Recipes => "recipes",
            ContentFile::Quests => "quests",
            ContentFile::Dialogs | ContentFile::Shops => "npcs",
        }
    }

    /// Имя файла в каталоге `content/`.
    pub fn name(self) -> &'static str {
        match self {
            ContentFile::Locations => "locations.json",
            ContentFile::Skills => "skills.json",
            ContentFile::Items => "items.json",
            ContentFile::Professions => "professions.json",
            ContentFile::Recipes => "recipes.json",
            ContentFile::Quests => "quests.json",
            ContentFile::Dialogs => "dialogs.json",
            ContentFile::Shops => "shops.json",
        }
    }
}

/// Сырые значения всех редактируемых файлов контента.
#[derive(Debug, Clone, PartialEq)]
pub struct RawContent {
    pub locations: Value,
    pub skills: Value,
    pub items: Value,
    pub professions: Value,
    pub recipes: Value,
    pub quests: Value,
    pub dialogs: Value,
    pub shops: Value,
}

impl RawContent {
    /// Прочитать каталог `content/` как сырые JSON-значения.
    pub fn load(dir: &Path) -> Result<Self, ContentError> {
        Ok(Self {
            locations: read_value(dir, ContentFile::Locations)?,
            skills: read_value(dir, ContentFile::Skills)?,
            items: read_value(dir, ContentFile::Items)?,
            professions: read_value(dir, ContentFile::Professions)?,
            recipes: read_value(dir, ContentFile::Recipes)?,
            quests: read_value(dir, ContentFile::Quests)?,
            dialogs: read_value(dir, ContentFile::Dialogs)?,
            shops: read_value(dir, ContentFile::Shops)?,
        })
    }

    /// Значение файла.
    pub fn get(&self, file: ContentFile) -> &Value {
        match file {
            ContentFile::Locations => &self.locations,
            ContentFile::Skills => &self.skills,
            ContentFile::Items => &self.items,
            ContentFile::Professions => &self.professions,
            ContentFile::Recipes => &self.recipes,
            ContentFile::Quests => &self.quests,
            ContentFile::Dialogs => &self.dialogs,
            ContentFile::Shops => &self.shops,
        }
    }

    /// Мутабельное значение файла (правки редактора).
    pub fn get_mut(&mut self, file: ContentFile) -> &mut Value {
        match file {
            ContentFile::Locations => &mut self.locations,
            ContentFile::Skills => &mut self.skills,
            ContentFile::Items => &mut self.items,
            ContentFile::Professions => &mut self.professions,
            ContentFile::Recipes => &mut self.recipes,
            ContentFile::Quests => &mut self.quests,
            ContentFile::Dialogs => &mut self.dialogs,
            ContentFile::Shops => &mut self.shops,
        }
    }

    /// Типизированное представление (для валидации/UI/версии).
    pub fn typed(&self) -> Result<GameContent, ContentError> {
        let locations: LocationsRoot = parse_root(&self.locations, ContentFile::Locations)?;
        let skills: SkillsRoot = parse_root(&self.skills, ContentFile::Skills)?;
        let items: ItemsRoot = parse_root(&self.items, ContentFile::Items)?;
        let professions: ProfessionsRoot = parse_root(&self.professions, ContentFile::Professions)?;
        let recipes: RecipesRoot = parse_root(&self.recipes, ContentFile::Recipes)?;
        let quests: QuestsRoot = parse_root(&self.quests, ContentFile::Quests)?;
        Ok(GameContent {
            locations: locations.locations,
            skills: skills.skills,
            items: items.items,
            professions: professions.professions,
            recipes: recipes.recipes,
            quests: quests.quests,
        })
    }

    /// Типизированные диалоги (вьюха для валидации/подписей редактора).
    pub fn typed_dialogs(&self) -> Result<ContentDialogs, ContentError> {
        serde_json::from_value(self.dialogs.clone())
            .map_err(|e| ContentError::Json(PathBuf::from(ContentFile::Dialogs.name()), e))
    }

    /// Типизированные магазины (вьюха для валидации/подписей редактора).
    pub fn typed_shops(&self) -> Result<ContentShops, ContentError> {
        serde_json::from_value(self.shops.clone())
            .map_err(|e| ContentError::Json(PathBuf::from(ContentFile::Shops.name()), e))
    }

    /// Атомарно записать файлы (`tmp + rename`); пишутся только переданные.
    /// Возвращает число записанных файлов.
    pub fn save(&self, dir: &Path, files: &[ContentFile]) -> Result<usize, ContentError> {
        for file in files {
            let text = render(self.get(*file), dir, *file)?;
            write_atomic(dir, *file, text.as_bytes())?;
        }
        Ok(files.len())
    }
}

/// Канонический вид файла: pretty 2 пробела + перевод строки в конце
/// (совпадает с генераторами контента — diff только по правкам).
pub fn render(value: &Value, dir: &Path, file: ContentFile) -> Result<String, ContentError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| ContentError::Json(dir.join(file.name()), e))?;
    text.push('\n');
    Ok(text)
}

fn read_value(dir: &Path, file: ContentFile) -> Result<Value, ContentError> {
    let path = dir.join(file.name());
    let raw = std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path.clone(), e))?;
    serde_json::from_str(&raw).map_err(|e| ContentError::Json(path, e))
}

fn parse_root<T: DeserializeOwned>(value: &Value, file: ContentFile) -> Result<T, ContentError> {
    serde_json::from_value(value.clone())
        .map_err(|e| ContentError::Json(PathBuf::from(file.name()), e))
}

fn write_atomic(dir: &Path, file: ContentFile, bytes: &[u8]) -> Result<(), ContentError> {
    let path = dir.join(file.name());
    let tmp = dir.join(format!(".{}.{}.tmp", file.name(), std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| ContentError::Io(tmp.clone(), e))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ContentError::Io(path, e)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_content() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
    }

    /// E2: сохранение без правок побайтово совпадает с файлами контента
    /// (контент в репозитории приведён к каноническому формату командой
    /// `ld2-editor --normalize-content`): diff правки — только правки.
    #[test]
    fn raw_save_is_byte_identical() {
        let src = repo_content();
        let raw = RawContent::load(&src).expect("контент загружается");
        let dst = std::env::temp_dir().join(format!(
            "ld2-content-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dst).expect("временный каталог");
        raw.save(&dst, &ContentFile::ALL).expect("сохранение");
        for file in ContentFile::ALL {
            let original_text = std::fs::read_to_string(src.join(file.name())).expect("оригинал");
            let saved_text = std::fs::read_to_string(dst.join(file.name())).expect("сохранённый");
            let original: Value = serde_json::from_str(&original_text).expect("оригинал JSON");
            let saved: Value = serde_json::from_str(&saved_text).expect("сохранённый JSON");
            assert_eq!(original, saved, "файл {} потерял данные", file.name());
            assert_eq!(
                original_text,
                saved_text,
                "файл {} изменился без правок",
                file.name()
            );
        }
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// E2: типизированное представление строится из сырых значений.
    #[test]
    fn raw_typed_matches_load() {
        let dir = repo_content();
        let raw = RawContent::load(&dir).expect("сырой контент");
        let typed = raw.typed().expect("типизированный контент");
        let direct = crate::load(&dir).expect("прямая загрузка");
        assert_eq!(
            serde_json::to_string(&typed).expect("сериализация typed"),
            serde_json::to_string(&direct).expect("сериализация direct")
        );
    }
}
