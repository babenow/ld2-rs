//! Карта мира и диалог карты (RF3m).

use super::*;

impl Root {
    /// M26/F26c: компактная карта-граф мира (вместо истории
    /// посещений, заказчик) — окрестность текущей локации (BFS-
    /// глубина 2), компасная раскладка; узлы — точки с тултипом
    /// «название + описание», плечи — линии с глифом направления
    /// (тултип — имя выхода). Только наведение (заказчик: клик —
    /// не действие). Растягивается на остаток высоты колонки.
    pub(crate) fn map_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        // flex_grow_1: блок занимает ВСЮ высоту колонки (без него
        // col по содержимому → тело = min_h → карта обрезалась по
        // высоте и связи не были видны — заказчик «нет ребер»).
        let mut col = div().flex().flex_col().gap_2().min_h_0().flex_grow_1();
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(13.0))
                .child(t(lang, Key::MapLabel)),
        );
        if !self.map_pos_ready {
            // Чанки карты ещё грузятся (cmd/map.world).
            return col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
        }
        let Some(snap) = self.store.snapshot.as_ref() else {
            return col;
        };
        let current = snap.location.id.clone();
        let visited: std::collections::HashSet<&str> =
            self.store.visited.iter().map(|v| v.id.as_str()).collect();
        let nb = crate::map::neighborhood(&self.store.map_locations, &current, 2);

        // Пиксели: клетка фиксированная; bbox окрестности сдвигается
        // в PAD — весь участок виден (зум/панорама — F26d, полное
        // окно карты).
        const CELL: f32 = 46.0;
        const PAD: f32 = 10.0;
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        for id in &nb.nodes {
            if let Some(n) = self.map_pos.get(id.as_str()) {
                min_x = min_x.min(n.pos.x);
                min_y = min_y.min(n.pos.y);
            }
        }
        if min_x == i32::MAX {
            return col; // позиции не посчитаны — заглушка
        }
        let sx = PAD - min_x as f32 * CELL;
        let sy = PAD - min_y as f32 * CELL;
        let hit_xy = |p: crate::map::NodePos| (sx + p.x as f32 * CELL, sy + p.y as f32 * CELL);

        // Линии-плечи и узлы — в пиксельные view-модели.
        let mut edges: Vec<MapEdgeView> = Vec::new();
        for (from, to, label, dir) in &nb.edges {
            let (Some(a), Some(b)) = (
                self.map_pos.get(from.as_str()),
                self.map_pos.get(to.as_str()),
            ) else {
                continue;
            };
            let (ax, ay) = hit_xy(a.pos);
            let (bx, by) = hit_xy(b.pos);
            edges.push(MapEdgeView {
                x1: ax,
                y1: ay,
                x2: bx,
                y2: by,
                glyph: crate::map::dir_glyph(dir.as_deref()),
                label: label.clone(),
            });
        }
        let mut nodes: Vec<MapNodeView> = Vec::new();
        for id in &nb.nodes {
            let Some(node) = self.map_pos.get(id.as_str()) else {
                continue;
            };
            let (nx, ny) = hit_xy(node.pos);
            nodes.push(MapNodeView {
                id: id.clone(),
                x: nx,
                y: ny,
                name: node.name.clone(),
                text: node.text.clone(),
                current: *id == current,
                visited: visited.contains(id.as_str()),
            });
        }

        let layer = map_graph_layer(
            pal,
            nodes,
            edges,
            div().id("ld2-map-compact").flex_grow_1().min_h(px(240.0)),
            24.0,
            40.0,
        );
        col = col.child(layer);
        col
    }

    /// M26/F26d: окно карты мира (заказчик: кнопка в шапке + «m») —
    /// весь мир, зум колесом (якорь — центр окна), панорама
    /// перетаскиванием; центр — текущая локация при открытии;
    /// кулинг — только узлы/плечи видимой области.
    pub(crate) fn map_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.map_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        let lang = self.lang;
        if !self.map_pos_ready {
            // Чанки карты грузятся — заглушка.
            let body = div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::Loading)),
                );
            return Some(self.popup_window(
                window,
                cx,
                pal,
                PopupKind::Map,
                t(lang, Key::MapLabel),
                None,
                body,
                None,
            ));
        }

        let (w, h) = self.popup(PopupKind::Map).size;
        let current = self.store.snapshot.as_ref().map(|s| s.location.id.clone());
        // Центр на текущей при первом открытии (pan = None).
        if self.map_pan.is_none() {
            if let Some(cur) = current.as_ref().and_then(|c| self.map_pos.get(c)) {
                let cell = 46.0 * self.map_zoom;
                self.map_pan = Some((
                    w / 2.0 - cur.pos.x as f32 * cell - cell / 2.0,
                    h / 2.0 - cur.pos.y as f32 * cell - cell / 2.0,
                ));
            }
        }
        let pan = self.map_pan.unwrap_or((0.0, 0.0));
        let cell = 46.0 * self.map_zoom;
        // Кружок/хит-бокс масштабируются с клеткой (клампы — чтобы
        // на зуме не превращались в кашицу и не слипались).
        let dot = (cell * 0.52).clamp(8.0, 26.0);
        let hit = (cell * 0.9).clamp(10.0, 44.0);
        let visited: std::collections::HashSet<&str> =
            self.store.visited.iter().map(|v| v.id.as_str()).collect();

        // Узлы: все + кулинг по вьюпорту (зазор — клетка); порядок
        // стабильный (по id) — детерминированный z-order.
        let mut ids: Vec<&String> = self.map_pos.keys().collect();
        ids.sort();
        let mut nodes: Vec<MapNodeView> = Vec::new();
        for id in ids {
            let node = &self.map_pos[id];
            let x = node.pos.x as f32 * cell + pan.0;
            let y = node.pos.y as f32 * cell + pan.1;
            if x < -cell || y < -cell || x > w || y > h {
                continue;
            }
            nodes.push(MapNodeView {
                id: id.clone(),
                x,
                y,
                name: node.name.clone(),
                text: node.text.clone(),
                current: current.as_deref() == Some(id.as_str()),
                visited: visited.contains(id.as_str()),
            });
        }

        // Плечи: из локаций (конец обязан быть расставлен — иначе
        // пропуск); кулинг — если оба конца вне вьюпорта.
        let out = |x: f32, y: f32| x < -cell || y < -cell || x > w || y > h;
        let mut edges: Vec<MapEdgeView> = Vec::new();
        for loc in &self.store.map_locations {
            let Some(from) = loc["id"].as_str() else {
                continue;
            };
            let Some(from_node) = self.map_pos.get(from) else {
                continue;
            };
            let fx = from_node.pos.x as f32 * cell + pan.0;
            let fy = from_node.pos.y as f32 * cell + pan.1;
            let Some(exits) = loc["exits"].as_array() else {
                continue;
            };
            for exit in exits {
                let Some(to) = exit["to"].as_str() else {
                    continue;
                };
                let Some(to_node) = self.map_pos.get(to) else {
                    continue;
                };
                let tx = to_node.pos.x as f32 * cell + pan.0;
                let ty = to_node.pos.y as f32 * cell + pan.1;
                if out(fx, fy) && out(tx, ty) {
                    continue;
                }
                edges.push(MapEdgeView {
                    x1: fx,
                    y1: fy,
                    x2: tx,
                    y2: ty,
                    glyph: crate::map::dir_glyph(exit["dir"].as_str()),
                    label: exit["label"].as_str().unwrap_or("").to_string(),
                });
            }
        }

        // Тело: зум колесом (якорь — центр окна) + панорама drag'ом.
        let scroll_this = cx.entity();
        let drag_this = cx.entity();
        let move_this = cx.entity();
        let shell = div()
            .id("ld2-map-body")
            .flex_grow_1()
            .min_h_0()
            .on_scroll(move |event, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if delta.y == gpui::Pixels::ZERO {
                    return;
                }
                scroll_this.update(cx, |root, cx| {
                    let old = root.map_zoom;
                    let new = (old * (1.0 - f32::from(delta.y) * 0.003)).clamp(0.2, 3.0);
                    if (new - old).abs() < f32::EPSILON {
                        return;
                    }
                    // Якорь — центр окна: точка мира в центре не
                    // двигается при зуме.
                    let (cw, ch) = root.popup(PopupKind::Map).size;
                    let pan = root.map_pan.unwrap_or((0.0, 0.0));
                    let k = new / old;
                    root.map_pan = Some((
                        cw / 2.0 - (cw / 2.0 - pan.0) * k,
                        ch / 2.0 - (ch / 2.0 - pan.1) * k,
                    ));
                    root.map_zoom = new;
                    cx.notify();
                });
            })
            .on_drag(MapPan, move |_, _offset, _, acx| {
                drag_this.update(acx, |root, cx| {
                    root.map_drag_last = None;
                    cx.notify();
                });
                acx.new(|_| EmptyView)
            })
            .on_drag_move::<MapPan>(move |event, _, cx| {
                move_this.update(cx, |root, cx| {
                    let p = (
                        f32::from(event.event.position.x),
                        f32::from(event.event.position.y),
                    );
                    if let Some(last) = root.map_drag_last {
                        let pan = root.map_pan.unwrap_or((0.0, 0.0));
                        root.map_pan = Some((pan.0 + p.0 - last.0, pan.1 + p.1 - last.1));
                    }
                    root.map_drag_last = Some(p);
                    cx.notify();
                });
            });
        let body = map_graph_layer(pal, nodes, edges, shell, dot, hit);
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Map,
            t(lang, Key::MapLabel),
            None,
            body,
            None,
        ))
    }
}
