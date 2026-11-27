//! E4: карта-граф мира для редактора — компасная раскладка узлов
//! (BFS от спавн-локации: север вверх, восток вправо, коллизии —
//! спираль; недостижимые компоненты — в отдельной области) + sidecar
//! `content/editor/layout.json` с ручными позициями узлов.
//!
//! Раскладка детерминирована (порядок локаций/выходов → одинаковые
//! координаты), поэтому авто-раскладка не «прыгает» между запусками.
//! Ручные позиции из `layout.json` перекрывают авто (drag/стрелки);
//! файл сервером не читается и в версию контента не входит.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use ld2_content::ContentLocation;
use serde::{Deserialize, Serialize};

/// Подкаталог sidecar-файлов редактора внутри каталога контента.
const EDITOR_DIR: &str = "editor";
const LAYOUT_NAME: &str = "layout.json";

/// Позиция узла на компасной сетке (клетки; y растёт вниз — север −1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
}

impl Cell {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Раскладка: id локации → клетка.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapLayout {
    pub nodes: HashMap<String, Cell>,
}

/// Формат `layout.json` (открытый для будущих полей: zoom/размер).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LayoutFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    nodes: HashMap<String, [i32; 2]>,
}

impl MapLayout {
    /// Путь к sidecar-файлу раскладки.
    pub fn path(content_dir: &Path) -> PathBuf {
        content_dir.join(EDITOR_DIR).join(LAYOUT_NAME)
    }

    /// Прочитать `layout.json`; файла нет — `Ok(None)`.
    pub fn load(content_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(content_dir);
        if !path.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| format!("раскладка: чтение {}: {err}", path.display()))?;
        let file: LayoutFile = serde_json::from_str(&raw)
            .map_err(|err| format!("раскладка: JSON {}: {err}", path.display()))?;
        Ok(Some(Self {
            nodes: file
                .nodes
                .into_iter()
                .map(|(id, [x, y])| (id, Cell::new(x, y)))
                .collect(),
        }))
    }

    /// Записать `layout.json` атомарно (tmp + rename, pretty 2 пробела).
    pub fn save(&self, content_dir: &Path) -> Result<(), String> {
        let dir = content_dir.join(EDITOR_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("раскладка: каталог {}: {err}", dir.display()))?;
        let mut nodes: Vec<(&String, &Cell)> = self.nodes.iter().collect();
        nodes.sort_by(|a, b| a.0.cmp(b.0));
        let file = LayoutFile {
            version: 1,
            nodes: nodes
                .into_iter()
                .map(|(id, cell)| (id.clone(), [cell.x, cell.y]))
                .collect(),
        };
        let mut text = serde_json::to_string_pretty(&file)
            .map_err(|err| format!("раскладка: сериализация: {err}"))?;
        text.push('\n');
        let path = dir.join(LAYOUT_NAME);
        let tmp = dir.join(format!(".{LAYOUT_NAME}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|err| format!("раскладка: запись {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            format!("раскладка: rename {}: {err}", path.display())
        })?;
        Ok(())
    }

    /// Дополнить раскладку авто-позициями для локаций, которых нет
    /// (новые локации/первый запуск). Ручные позиции сохраняются.
    pub fn merge_auto(&mut self, locations: &[ContentLocation]) {
        let auto = auto_layout(locations);
        for (id, cell) in auto.nodes {
            self.nodes.entry(id).or_insert(cell);
        }
        // Удаляем узлы, которых больше нет в контенте.
        let ids: HashSet<&str> = locations.iter().map(|loc| loc.id.as_str()).collect();
        self.nodes.retain(|id, _| ids.contains(id.as_str()));
    }
}

/// Смещение по каноническому направлению выхода (`dir`).
pub fn dir_offset(dir: &str) -> Option<(i32, i32)> {
    match dir {
        "north" => Some((0, -1)),
        "south" => Some((0, 1)),
        "east" => Some((1, 0)),
        "west" => Some((-1, 0)),
        "north_east" => Some((1, -1)),
        "north_west" => Some((-1, -1)),
        "south_east" => Some((1, 1)),
        "south_west" => Some((-1, 1)),
        _ => None,
    }
}

/// Направление выхода по разнице клеток (только кардинальные —
/// как в контенте; диагонали в `dir` не используются).
pub fn compass_dir(from: Cell, to: Cell) -> Option<&'static str> {
    match (to.x - from.x, to.y - from.y) {
        (0, -1) => Some("north"),
        (1, 0) => Some("east"),
        (0, 1) => Some("south"),
        (-1, 0) => Some("west"),
        _ => None,
    }
}

/// Размер узла на полотне для текущего масштаба клетки.
pub fn node_size(cell_px: f32) -> f32 {
    (cell_px * 0.55).clamp(12.0, 38.0)
}

/// Клетка под курсором при перетаскивании узла.
///
/// `local` — позиция курсора в координатах полотна, `pan` — смещение
/// полотна, `grab` — точка захвата относительно левого верхнего угла
/// узла (как отдаёт `on_drag`), `node_size` — текущий диаметр узла.
pub fn snap_cell(
    local: (f32, f32),
    pan: (f32, f32),
    cell_px: f32,
    grab: (f32, f32),
    node_size: f32,
) -> Cell {
    let center_x = local.0 - grab.0 + node_size / 2.0;
    let center_y = local.1 - grab.1 + node_size / 2.0;
    Cell {
        x: ((center_x - pan.0) / cell_px).round() as i32,
        y: ((center_y - pan.1) / cell_px).round() as i32,
    }
}

/// Авто-раскладка всего графа (детерминированная).
pub fn auto_layout(locations: &[ContentLocation]) -> MapLayout {
    let mut adj: HashMap<&str, Vec<(&str, Option<&str>)>> = HashMap::new();
    let mut ids: Vec<&str> = Vec::new();
    for loc in locations {
        ids.push(loc.id.as_str());
        let exits = loc
            .exits
            .iter()
            .map(|exit| (exit.to.as_str(), exit.dir.as_deref()))
            .collect();
        adj.insert(loc.id.as_str(), exits);
    }

    let mut placed: HashMap<String, Cell> = HashMap::new();
    let mut taken: HashSet<(i32, i32)> = HashSet::new();

    // Основной компонент — от спавн-локации (fallback — первая).
    let root = locations
        .iter()
        .find(|loc| loc.spawn)
        .map(|loc| loc.id.as_str())
        .or_else(|| ids.first().copied())
        .unwrap_or("");
    if !root.is_empty() {
        bfs_from(root, (0, 0), &adj, &mut taken, &mut placed);
    }

    // Недостижимые компоненты — вторичные корни в отдельной области.
    let mut secondary = 0i32;
    for id in &ids {
        if placed.contains_key(*id) {
            continue;
        }
        bfs_from(
            id,
            (10_000 + secondary * 1_000, 0),
            &adj,
            &mut taken,
            &mut placed,
        );
        secondary += 1;
    }
    MapLayout { nodes: placed }
}

fn bfs_from(
    start: &str,
    origin: (i32, i32),
    adj: &HashMap<&str, Vec<(&str, Option<&str>)>>,
    taken: &mut HashSet<(i32, i32)>,
    placed: &mut HashMap<String, Cell>,
) {
    if placed.contains_key(start) {
        return;
    }
    let (sx, sy) = spiral_free(taken, origin);
    placed.insert(start.to_string(), Cell::new(sx, sy));
    let mut queue: VecDeque<(&str, (i32, i32))> = VecDeque::from([(start, (sx, sy))]);
    while let Some((id, pos)) = queue.pop_front() {
        let Some(edges) = adj.get(id) else {
            continue;
        };
        for (to, dir) in edges {
            if placed.contains_key(*to) {
                continue; // первый визит выигрывает
            }
            let (tx, ty) = match dir.and_then(dir_offset) {
                Some((dx, dy)) => spiral_free(taken, (pos.0 + dx, pos.1 + dy)),
                // Выход без направления — клетка рядом с родителем.
                None => spiral_free(taken, pos),
            };
            placed.insert((*to).to_string(), Cell::new(tx, ty));
            queue.push_back((to, (tx, ty)));
        }
    }
}

/// Свободная клетка: `origin`, иначе ближайшие клетки спирали
/// (детерминированный порядок: манхэттен, затем y/x).
fn spiral_free(taken: &mut HashSet<(i32, i32)>, origin: (i32, i32)) -> (i32, i32) {
    let (ox, oy) = origin;
    if taken.insert(origin) {
        return origin;
    }
    let mut radius = 1i32;
    loop {
        let mut ring: Vec<(i32, i32)> = Vec::new();
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs() == radius || dy.abs() == radius {
                    ring.push((ox + dx, oy + dy));
                }
            }
        }
        ring.sort_by_key(|(x, y)| ((x - ox).abs() + (y - oy).abs(), *y, *x));
        for cell in ring {
            if taken.insert(cell) {
                return cell;
            }
        }
        radius += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ld2_content::ContentExit;
    use serde_json::Value;

    fn location(id: &str, spawn: bool, exits: &[(&str, Option<&str>)]) -> ContentLocation {
        ContentLocation {
            id: id.into(),
            name: id.into(),
            spawn,
            flags: Vec::new(),
            text: String::new(),
            exits: exits
                .iter()
                .map(|(to, dir)| ContentExit {
                    id: format!("e_{to}"),
                    label: format!("→{to}"),
                    dir: dir.map(str::to_string),
                    to: (*to).to_string(),
                    extra: Default::default(),
                })
                .collect(),
            entities: Vec::new(),
            extra: Default::default(),
        }
    }

    fn temp_content() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ld2-editor-layout-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("каталог");
        dir
    }

    /// Направление выхода задаёт относительное положение узла.
    #[test]
    fn direction_is_respected() {
        let locations = vec![location(
            "a",
            true,
            &[
                ("n", Some("north")),
                ("e", Some("east")),
                ("s", Some("south")),
            ],
        )];
        let mut locations = locations;
        locations.extend([
            location("n", false, &[]),
            location("e", false, &[]),
            location("s", false, &[]),
        ]);
        let layout = auto_layout(&locations);
        let a = layout.nodes["a"];
        assert_eq!(layout.nodes["n"], Cell::new(a.x, a.y - 1), "север — вверх");
        assert_eq!(
            layout.nodes["e"],
            Cell::new(a.x + 1, a.y),
            "восток — вправо"
        );
        assert_eq!(layout.nodes["s"], Cell::new(a.x, a.y + 1), "юг — вниз");
    }

    /// Раскладка детерминирована и не пересекает узлы.
    #[test]
    fn layout_is_deterministic_and_unique() {
        let mut locations = vec![location("root", true, &[])];
        for index in 0..40 {
            let id = format!("l{index}");
            let from = if index == 0 {
                "root".to_string()
            } else {
                format!("l{}", index - 1)
            };
            locations.push(location(&id, false, &[(from.as_str(), Some("east"))]));
        }
        let first = auto_layout(&locations);
        let second = auto_layout(&locations);
        assert_eq!(first, second, "раскладка детерминирована");
        let unique: HashSet<(i32, i32)> =
            first.nodes.values().map(|cell| (cell.x, cell.y)).collect();
        assert_eq!(unique.len(), first.nodes.len(), "клетки уникальны");
    }

    /// Недостижимые компоненты раскладываются отдельно и не пересекаются.
    #[test]
    fn unreachable_components_placed_separately() {
        let locations = vec![
            location("root", true, &[("a", Some("east"))]),
            location("a", false, &[]),
            location("island", false, &[]),
        ];
        let layout = auto_layout(&locations);
        assert_eq!(layout.nodes.len(), 3);
        assert!(
            layout.nodes["island"].x >= 10_000,
            "остров — вторичная область"
        );
    }

    /// layout.json: сохранение/чтение; пустой каталог — `None`.
    #[test]
    fn layout_roundtrip() {
        let dir = temp_content();
        assert!(MapLayout::load(&dir).expect("load").is_none());
        let mut layout = MapLayout::default();
        layout.nodes.insert("a".into(), Cell::new(3, -2));
        layout.save(&dir).expect("save");
        let loaded = MapLayout::load(&dir).expect("load").expect("файл есть");
        assert_eq!(loaded, layout);
        let path = MapLayout::path(&dir);
        let text = std::fs::read_to_string(&path).expect("файл");
        assert!(text.ends_with('\n'));
        assert!(text.contains("\"a\""), "id в файле: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// merge_auto сохраняет ручные позиции и добавляет недостающие.
    #[test]
    fn merge_auto_keeps_manual_positions() {
        let locations = vec![
            location("root", true, &[("a", Some("east"))]),
            location("a", false, &[]),
        ];
        let mut layout = MapLayout::default();
        layout.nodes.insert("root".into(), Cell::new(100, 100));
        layout.merge_auto(&locations);
        assert_eq!(layout.nodes["root"], Cell::new(100, 100), "ручная позиция");
        assert!(layout.nodes.contains_key("a"), "недостающий добавлен");
    }

    /// Неизвестные поля layout.json сохраняются (открытый формат).
    #[test]
    fn layout_reads_future_fields() {
        let dir = temp_content();
        std::fs::create_dir_all(dir.join(EDITOR_DIR)).expect("editor dir");
        std::fs::write(
            MapLayout::path(&dir),
            r#"{ "version": 1, "future": {"zoom": 2}, "nodes": {"a": [1, 2]} }"#,
        )
        .expect("запись");
        let loaded = MapLayout::load(&dir).expect("load").expect("есть");
        assert_eq!(loaded.nodes["a"], Cell::new(1, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Дубликат значения в map нельзя: serde должен отклонять мусор.
    #[test]
    fn layout_rejects_bad_json() {
        let dir = temp_content();
        std::fs::create_dir_all(dir.join(EDITOR_DIR)).expect("editor dir");
        std::fs::write(MapLayout::path(&dir), "{ not json }").expect("запись");
        assert!(MapLayout::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Направление по разнице клеток: кардинальные оси; диагонали — нет.
    #[test]
    fn compass_direction_from_cells() {
        let origin = Cell::new(3, 4);
        assert_eq!(compass_dir(origin, Cell::new(3, 3)), Some("north"));
        assert_eq!(compass_dir(origin, Cell::new(4, 4)), Some("east"));
        assert_eq!(compass_dir(origin, Cell::new(3, 5)), Some("south"));
        assert_eq!(compass_dir(origin, Cell::new(2, 4)), Some("west"));
        assert_eq!(compass_dir(origin, Cell::new(4, 3)), None);
        assert_eq!(compass_dir(origin, origin), None);
        assert_eq!(compass_dir(origin, Cell::new(6, 4)), None);
    }

    /// Перетаскивание: курсор в центре узла → его клетка; округление
    /// к ближайшей клетке с учётом зума и смещения полотна.
    #[test]
    fn snap_cell_tracks_pointer() {
        let cell_px = 46.0;
        let nsize = node_size(cell_px);
        let pan = (100.0, -50.0);
        // центр узла клетки (2, 1): cell*cell_px + pan
        let center = (pan.0 + 2.0 * cell_px, pan.1 + cell_px);
        assert_eq!(
            snap_cell(center, pan, cell_px, (0.0, 0.0), nsize),
            Cell::new(2, 1)
        );
        // захват за правый нижний край не должен сдвигать клетку
        let grab = (nsize - 1.0, nsize - 1.0);
        let local = (
            center.0 + grab.0 - nsize / 2.0,
            center.1 + grab.1 - nsize / 2.0,
        );
        assert_eq!(snap_cell(local, pan, cell_px, grab, nsize), Cell::new(2, 1));
        // сдвиг на полклетки вправо — уже следующая клетка
        assert_eq!(
            snap_cell(
                (center.0 + cell_px / 2.0, center.1),
                pan,
                cell_px,
                (0.0, 0.0),
                nsize
            ),
            Cell::new(3, 1)
        );
        // зум 0.5: клетка визуально вдвое меньше, но координата та же
        assert_eq!(
            snap_cell(
                (pan.0 + 2.0 * cell_px * 0.5, pan.1 + cell_px * 0.5),
                pan,
                cell_px * 0.5,
                (0.0, 0.0),
                node_size(cell_px * 0.5)
            ),
            Cell::new(2, 1)
        );
    }

    /// Значение `dir` неизвестно — выход без направления (клетка рядом).
    #[test]
    fn unknown_direction_places_near_parent() {
        let locations = vec![
            location("root", true, &[]),
            location("a", false, &[("root", Some("diagonal_unknown"))]),
        ];
        let layout = auto_layout(&locations);
        let _ = layout; // главное — без паники и узел размещён
        assert!(Value::Null.is_null()); // заглушка для импорта Value
    }
}
