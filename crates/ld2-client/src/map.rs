//! M26/F26b–F26c: компасная раскладка карты-графа мира (заказчик:
//! север вверх, юг вниз, восток вправо, запад влево; диагонали — между;
//! коллизии — спиральный сдвиг; недостижимые компоненты — вторичные
//! корни в отдельной области) + окрестность для компактной карты.
//! Детерминизм: порядок входа (сервер — ORDER BY id) + порядок выходов.

use std::collections::{HashMap, HashSet, VecDeque};

/// Позиция узла на компасной сетке (клетки; y растёт вниз — север −1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodePos {
    pub x: i32,
    pub y: i32,
}

/// Узел карты: позиция + имя/описание (для тултипа «название +
/// описание» — заказчик).
#[derive(Debug, Clone, PartialEq)]
pub struct MapNode {
    pub pos: NodePos,
    pub name: String,
    pub text: String,
}

/// Смещение по каноническому направлению выхода (§6 `dir`).
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

/// Глиф направления для подписи плеча (заказчик: «направления
/// выходов» видны на карте).
pub fn dir_glyph(dir: Option<&str>) -> &'static str {
    match dir {
        Some("north") => "↑",
        Some("south") => "↓",
        Some("east") => "→",
        Some("west") => "←",
        Some("north_east") => "↗",
        Some("north_west") => "↖",
        Some("south_east") => "↘",
        Some("south_west") => "↙",
        _ => "•",
    }
}

/// Свободная клетка: `origin`, иначе кольца спирали вокруг неё
/// (коллизии раскладки — два узла претендуют на одну клетку).
fn spiral_free(taken: &mut HashSet<(i32, i32)>, origin: (i32, i32)) -> (i32, i32) {
    let (ox, oy) = origin;
    if taken.insert(origin) {
        return origin;
    }
    let mut r = 1i32;
    loop {
        // Кольцо радиуса r (чебышёв): ближайшие к origin — первыми
        // (манхэттен, затем y/x — детерминизм); визуально узлы
        // ложатся вплотную, без «прыжков» в угол.
        let mut ring: Vec<(i32, i32)> = Vec::new();
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() == r || dy.abs() == r {
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
        r += 1;
    }
}

type Edges = HashMap<String, Vec<(String, Option<String>)>>;
type Meta = HashMap<String, (String, String)>;

fn map_node(id: &str, pos: NodePos, meta: &Meta) -> MapNode {
    let (name, text) = meta.get(id).cloned().unwrap_or_default();
    MapNode { pos, name, text }
}

/// BFS от `start`: позиция узла = позиция родителя + смещение по
/// `dir` (выход без направления — клетка у родителя, спираль);
/// первый визит выигрывает (BFS-порядок), коллизии — спираль.
fn bfs_from(
    start: &str,
    origin: (i32, i32),
    adj: &Edges,
    meta: &Meta,
    taken: &mut HashSet<(i32, i32)>,
    placed: &mut HashMap<String, MapNode>,
) {
    if placed.contains_key(start) {
        return;
    }
    let (sx, sy) = spiral_free(taken, origin);
    let start_pos = NodePos { x: sx, y: sy };
    placed.insert(start.to_string(), map_node(start, start_pos, meta));
    let mut queue: VecDeque<(String, (i32, i32))> =
        VecDeque::from([(start.to_string(), (start_pos.x, start_pos.y))]);
    while let Some((id, pos)) = queue.pop_front() {
        let Some(edges) = adj.get(&id) else {
            continue;
        };
        for (to, dir) in edges {
            if placed.contains_key(to) {
                continue; // первый визит выигрывает (детерминизм)
            }
            let (tx, ty) = match dir.as_deref().and_then(dir_offset) {
                Some((dx, dy)) => spiral_free(taken, (pos.0 + dx, pos.1 + dy)),
                // Выход без направления — узел у родителя.
                None => spiral_free(taken, (pos.0, pos.1)),
            };
            let p = NodePos { x: tx, y: ty };
            placed.insert(to.clone(), map_node(to, p, meta));
            queue.push_back((to.clone(), (tx, ty)));
        }
    }
}

/// Раскладка всего графа: основной компонент — от `root` (спавн —
/// мировой корень, стабильная карта); недостижимые компоненты — от
/// вторичных корней в отдельной области (правее, с шагом).
///
/// `locations` — массив `{id, name, text, exits: [{to, dir?}]}`.
pub fn layout(locations: &[serde_json::Value], root: &str) -> HashMap<String, MapNode> {
    let mut adj: Edges = HashMap::new();
    let mut meta: Meta = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    for loc in locations {
        let Some(id) = loc["id"].as_str() else {
            continue;
        };
        ids.push(id.to_string());
        meta.insert(
            id.to_string(),
            (
                loc["name"].as_str().unwrap_or("").to_string(),
                loc["text"].as_str().unwrap_or("").to_string(),
            ),
        );
        let edges = loc["exits"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|e| {
                        let to = e["to"].as_str()?.to_string();
                        let dir = e["dir"].as_str().map(str::to_string);
                        Some((to, dir))
                    })
                    .collect()
            })
            .unwrap_or_default();
        adj.insert(id.to_string(), edges);
    }

    let mut placed: HashMap<String, MapNode> = HashMap::new();
    let mut taken: HashSet<(i32, i32)> = HashSet::new();

    // Основной компонент — от спавн-корня (fallback — первая локация).
    let root_id = if ids.iter().any(|i| i == root) {
        root
    } else {
        ids.first().map(String::as_str).unwrap_or("")
    };
    if !root_id.is_empty() {
        bfs_from(root_id, (0, 0), &adj, &meta, &mut taken, &mut placed);
    }

    // Недостижимые компоненты — вторичные корни в отдельной области
    // (шаг 1000 клеток — области не пересекаются).
    let mut secondary = 0i32;
    for id in &ids {
        if placed.contains_key(id) {
            continue;
        }
        let origin = (10_000 + secondary * 1_000, 0);
        secondary += 1;
        bfs_from(id, origin, &adj, &meta, &mut taken, &mut placed);
    }
    placed
}

/// Окрестность `current` глубиной `depth` (BFS по выходам) — узлы
/// (BFS-порядок) и плечи с обоими концами в наборе — для
/// компактной карты в колонке (F26c).
pub struct Neighborhood {
    pub nodes: Vec<String>,
    /// (from, to, label, dir) — label = имя выхода (тултип плеча),
    /// dir = каноническое направление (глиф на плече; None — выход
    /// без направления).
    pub edges: Vec<(String, String, String, Option<String>)>,
}

pub fn neighborhood(locations: &[serde_json::Value], current: &str, depth: usize) -> Neighborhood {
    let mut adj: HashMap<String, Vec<(String, String, Option<String>)>> = HashMap::new();
    for loc in locations {
        let Some(id) = loc["id"].as_str() else {
            continue;
        };
        let exits = loc["exits"].as_array().cloned().unwrap_or_default();
        let list = exits
            .iter()
            .filter_map(|e| {
                Some((
                    e["to"].as_str()?.to_string(),
                    e["label"].as_str().unwrap_or("").to_string(),
                    e["dir"].as_str().map(str::to_string),
                ))
            })
            .collect();
        adj.insert(id.to_string(), list);
    }

    // BFS-кольца от текущей локации.
    let mut nodes = vec![current.to_string()];
    let mut seen: HashSet<String> = HashSet::from([current.to_string()]);
    let mut frontier = vec![current.to_string()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            for (to, _, _) in adj.get(id).into_iter().flatten() {
                if seen.insert(to.clone()) {
                    next.push(to.clone());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        nodes.extend(next.iter().cloned());
        frontier = next;
    }

    // Плечи: только с обоими концами в наборе (вне — не рисуем).
    let mut edges = Vec::new();
    for id in &nodes {
        for (to, label, dir) in adj.get(id).into_iter().flatten() {
            if seen.contains(to) {
                edges.push((id.clone(), to.clone(), label.clone(), dir.clone()));
            }
        }
    }
    Neighborhood { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(id: &str, exits: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "id": id, "name": id, "text": "", "exits": exits })
    }

    fn exit(to: &str, dir: Option<&str>) -> serde_json::Value {
        match dir {
            Some(d) => serde_json::json!({ "to": to, "dir": d }),
            None => serde_json::json!({ "to": to }),
        }
    }

    /// M26: компасные смещения — 8 канонических направлений.
    #[test]
    fn dir_offsets_follow_compass() {
        assert_eq!(dir_offset("north"), Some((0, -1)));
        assert_eq!(dir_offset("south"), Some((0, 1)));
        assert_eq!(dir_offset("east"), Some((1, 0)));
        assert_eq!(dir_offset("west"), Some((-1, 0)));
        assert_eq!(dir_offset("north_east"), Some((1, -1)));
        assert_eq!(dir_offset("south_west"), Some((-1, 1)));
        assert_eq!(dir_offset("diagonal_up"), None);
        assert_eq!(dir_offset(""), None);
        assert_eq!(dir_glyph(Some("north")), "↑");
        assert_eq!(dir_glyph(None), "•");
    }

    /// M26: цепочка по компасу — позиции накапливаются от корня.
    #[test]
    fn chain_accumulates_offsets() {
        let locations = vec![
            loc(
                "a",
                serde_json::json!([exit("b", Some("north")), exit("c", Some("east"))]),
            ),
            loc("b", serde_json::json!([exit("d", Some("west"))])),
            loc("c", serde_json::json!([])),
            loc("d", serde_json::json!([])),
        ];
        let map = layout(&locations, "a");
        assert_eq!(map["a"].pos, NodePos { x: 0, y: 0 });
        assert_eq!(map["b"].pos, NodePos { x: 0, y: -1 }); // север вверх
        assert_eq!(map["c"].pos, NodePos { x: 1, y: 0 }); // восток вправо
        assert_eq!(map["d"].pos, NodePos { x: -1, y: -1 }); // b + west
        assert_eq!(map["b"].name, "b", "имя из контента");
    }

    /// M26: коллизия (два узла в одной клетке) — спиральный сдвиг.
    #[test]
    fn collision_spirals_to_free_cell() {
        let locations = vec![
            loc(
                "a",
                serde_json::json!([exit("b", Some("north")), exit("c", Some("north"))]),
            ),
            loc("b", serde_json::json!([])),
            loc("c", serde_json::json!([])),
        ];
        let map = layout(&locations, "a");
        assert_eq!(map["b"].pos, NodePos { x: 0, y: -1 });
        // c претендует на (0,-1) — занят b → спираль.
        assert_ne!(map["c"].pos, map["b"].pos, "клетки не совпадают");
        assert!(
            (map["c"].pos.x - map["b"].pos.x).abs() <= 1
                && (map["c"].pos.y - map["b"].pos.y).abs() <= 1,
            "c — соседняя клетка (кольцо спирали)"
        );
    }

    /// M26: выход без направления — узел у родителя (спираль).
    #[test]
    fn dirless_exit_places_near_parent() {
        let locations = vec![
            loc("a", serde_json::json!([exit("b", None)])),
            loc("b", serde_json::json!([])),
        ];
        let map = layout(&locations, "a");
        let b = map["b"].pos;
        let a = map["a"].pos;
        assert!(
            (b.x - a.x).abs() <= 1 && (b.y - a.y).abs() <= 1,
            "b — сосед клетки a"
        );
    }

    /// M26: недостижимая компонента — от вторичного корня в отдельной
    /// области (не пересекается с основной картой).
    #[test]
    fn unreachable_component_gets_secondary_root() {
        let locations = vec![
            loc("a", serde_json::json!([exit("b", Some("north"))])),
            loc("b", serde_json::json!([])),
            loc("x", serde_json::json!([exit("y", Some("east"))])),
            loc("y", serde_json::json!([])),
        ];
        let map = layout(&locations, "a");
        assert_eq!(map.len(), 4, "все узлы расставлены");
        assert!(map["x"].pos.x >= 10_000, "вторичная область правее");
        assert_eq!(
            map["y"].pos,
            NodePos {
                x: map["x"].pos.x + 1,
                y: 0
            }
        );
        assert!(
            map["a"].pos.x.abs() < 100 && map["b"].pos.y < 0,
            "основная область"
        );
    }

    /// M26: детерминизм — одинаковый вход → одинаковая раскладка.
    #[test]
    fn layout_is_deterministic() {
        let locations = vec![
            loc(
                "a",
                serde_json::json!([exit("b", Some("north")), exit("c", Some("east"))]),
            ),
            loc("b", serde_json::json!([exit("c", Some("south"))])),
            loc("c", serde_json::json!([exit("d", Some("west"))])),
            loc("d", serde_json::json!([])),
            loc("z", serde_json::json!([])),
        ];
        let first = layout(&locations, "a");
        let second = layout(&locations, "a");
        assert_eq!(first, second);
        assert_eq!(first.len(), 5, "все узлы (включая недостижимую z)");
    }

    /// M26/F26c: окрестность — узлы глубины ≤ depth, плечи только с
    /// обоими концами в наборе (вне — не рисуем).
    #[test]
    fn neighborhood_limits_depth_and_edges() {
        let locations = vec![
            loc("a", serde_json::json!([exit("b", Some("north"))])),
            loc("b", serde_json::json!([exit("c", Some("north"))])),
            loc("c", serde_json::json!([exit("d", Some("north"))])),
            loc("d", serde_json::json!([])),
        ];
        let nb = neighborhood(&locations, "a", 2);
        assert_eq!(nb.nodes, vec!["a", "b", "c"], "глубина 2 — без d");
        // Плечо c→d (d вне набора) — не рисуем; a→b, b→c — рисуем.
        assert_eq!(nb.edges.len(), 2, "только внутренние плечи");
        assert!(nb.edges.iter().all(|(f, t, _, _)| f != "c" || t != "d"));
        assert_eq!(nb.edges[0].2, "", "label из контента");
        assert_eq!(nb.edges[0].3.as_deref(), Some("north"), "dir из контента");
    }
}
