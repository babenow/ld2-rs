-- M17/F30b: новая система слотов (15) и правила классов.
ALTER TABLE item_defs ADD COLUMN slot TEXT;
ALTER TABLE item_defs ADD COLUMN hands INTEGER NOT NULL DEFAULT 1;

-- Посох был отдельным слотом: не-магам он больше не положен — снимаем;
-- магам посох занимает правую руки (двухручник).
UPDATE items SET equipped_slot = NULL
 WHERE equipped_slot = 'staff'
   AND character_id IN (SELECT id FROM characters WHERE profile <> 'mage');
UPDATE items SET equipped_slot = 'main_hand' WHERE equipped_slot = 'staff';
-- Конфликт в правой руке (посох + холодное оружие) — снимаем не-посох.
UPDATE items SET equipped_slot = NULL
 WHERE equipped_slot = 'main_hand'
   AND def_id IN (SELECT id FROM item_defs WHERE item_type <> 'staff')
   AND character_id IN (
        SELECT character_id FROM items
         WHERE equipped_slot = 'main_hand'
           AND def_id IN (SELECT id FROM item_defs WHERE item_type = 'staff'));

-- Легаси: гейт "armor → body" надевал всё на один слот — раскладываем
-- по слотам предметов.
UPDATE items SET equipped_slot = 'head'
 WHERE equipped_slot = 'body'
   AND def_id IN ('helmet_leather', 'hood_scout', 'helm_knight', 'helm_champion');
UPDATE items SET equipped_slot = 'feet'
 WHERE equipped_slot = 'body'
   AND def_id IN ('boots_leather', 'boots_traveler', 'boots_thunder');
UPDATE items SET equipped_slot = 'legs'
 WHERE equipped_slot = 'body' AND def_id = 'greaves_iron';

-- Профиль: Вор → Лучник (wire-значение archer).
UPDATE characters SET profile = 'archer' WHERE profile = 'rogue';
