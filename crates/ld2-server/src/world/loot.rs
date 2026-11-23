//! Дроп, контейнеры и лавки мира (M20/F20a).

use super::*;

impl World {
    /// M24: выбросить всю надстройку сундука (база — tombstone'ами).
    pub fn container_clear(&mut self, entity_id: &str) {
        self.container_extra.remove(entity_id);
    }
    /// M24: выбросить всю надстройку магазина (контент — tombstone'ами).
    pub fn shop_clear(&mut self, npc_id: &str) {
        self.shop_extra.remove(npc_id);
    }
    /// M24: очистить весь лут живого монстра (восстановится при
    /// респавне из шаблона).
    pub fn mob_drops_clear(&mut self, mob_id: &str) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) => {
                mob.drops.clear();
                true
            }
            None => false,
        }
    }
    /// M24: админ-лут монстра (одноразовый, в труп при смерти).
    pub fn mob_bonus_add(&mut self, mob_id: &str, def_id: &str) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) => {
                mob.bonus_loot.push(def_id.to_string());
                true
            }
            None => false,
        }
    }
    /// M24: убрать `count` позиций админ-лута монстра с `idx`.
    pub fn mob_bonus_remove_at(&mut self, mob_id: &str, idx: usize, count: usize) -> bool {
        match self.mobs.get_mut(mob_id) {
            Some(mob) if idx < mob.bonus_loot.len() => {
                let end = (idx + count).min(mob.bonus_loot.len());
                mob.bonus_loot.drain(idx..end);
                true
            }
            _ => false,
        }
    }
    /// M24: текущий админ-лут монстра (для admin.target.info).
    pub fn mob_bonus(&self, mob_id: &str) -> Vec<String> {
        self.mobs
            .get(mob_id)
            .map(|mob| mob.bonus_loot.clone())
            .unwrap_or_default()
    }
    /// Слоты надстройки сундука (позиции после базовых defs).
    pub fn container_extra(&self, entity_id: &str) -> Vec<Option<(String, i64)>> {
        self.container_extra
            .get(entity_id)
            .cloned()
            .unwrap_or_default()
    }
    /// Добавить в надстройку сундука **один стак** с указанным
    /// количеством (указание заказчика): складывается с уже лежащим
    /// таким же дефом, иначе — одна новая позиция.
    pub fn container_extra_add(&mut self, entity_id: &str, def_id: &str, qty: i64) {
        let slots = self
            .container_extra
            .entry(entity_id.to_string())
            .or_default();
        if let Some(slot) = slots.iter_mut().flatten().find(|slot| slot.0 == def_id) {
            slot.1 = (slot.1 + qty).min(i64::from(i32::MAX));
            return;
        }
        slots.push(Some((def_id.to_string(), qty)));
    }
    /// Слот надстройки сундука: (def_id, количество).
    pub fn container_extra_slot(&self, entity_id: &str, idx: usize) -> Option<(String, i64)> {
        self.container_extra.get(entity_id)?.get(idx)?.clone()
    }
    /// Занять весь слот надстройки (взятие игроком/удаление целиком).
    pub fn container_extra_clear(&mut self, entity_id: &str, idx: usize) -> bool {
        match self.container_extra.get_mut(entity_id) {
            Some(slots) if idx < slots.len() && slots[idx].is_some() => {
                slots[idx] = None;
                true
            }
            _ => false,
        }
    }
    /// Убрать `count` единиц из слота надстройки (частичное удаление
    /// админом): остаток < count → слот освобождается.
    pub fn container_extra_reduce(&mut self, entity_id: &str, idx: usize, count: i64) -> bool {
        match self.container_extra.get_mut(entity_id) {
            Some(slots) if idx < slots.len() => match slots[idx].as_mut() {
                Some((_, qty)) if count >= *qty => {
                    slots[idx] = None;
                    true
                }
                Some((_, qty)) => {
                    *qty -= count;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }
    /// Базовый слот сундука удалён админом (tombstone)?
    pub fn container_base_removed(&self, entity_id: &str, idx: usize) -> bool {
        self.container_removed
            .contains(&(entity_id.to_string(), idx))
    }
    /// Пометить базовый слот сундука удалённым (глобально).
    pub fn container_base_remove(&mut self, entity_id: &str, idx: usize) {
        self.container_removed.insert((entity_id.to_string(), idx));
    }
    /// Позиции надстройки магазина (`None` - удалено).
    pub fn shop_extra(&self, npc_id: &str) -> Vec<Option<String>> {
        self.shop_extra.get(npc_id).cloned().unwrap_or_default()
    }
    /// Добавить позицию в надстройку магазина — **одна позиция на
    /// предмет** (магазин не лимитирован, копии не нужны).
    pub fn shop_extra_add(&mut self, npc_id: &str, def_id: &str) {
        let slots = self.shop_extra.entry(npc_id.to_string()).or_default();
        if slots.iter().flatten().any(|d| d == def_id) {
            return;
        }
        slots.push(Some(def_id.to_string()));
    }
    /// Удалить `count` позиций надстройки магазина с `idx`.
    pub fn shop_extra_take_at(&mut self, npc_id: &str, idx: usize, count: usize) -> bool {
        match self.shop_extra.get_mut(npc_id) {
            Some(slots) if idx < slots.len() => {
                let end = (idx + count).min(slots.len());
                for slot in slots.iter_mut().take(end).skip(idx) {
                    *slot = None;
                }
                true
            }
            _ => false,
        }
    }
    /// Контентный товар магазина удалён админом?
    pub fn shop_content_removed(&self, npc_id: &str, tab: usize, idx: usize) -> bool {
        self.shop_removed.contains(&(npc_id.to_string(), tab, idx))
    }
    /// Пометить контентный товар магазина удалённым.
    pub fn shop_content_remove(&mut self, npc_id: &str, tab: usize, idx: usize) {
        self.shop_removed.insert((npc_id.to_string(), tab, idx));
    }
}
