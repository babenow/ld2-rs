-- Откат: профиль и схема возвращаются; посох снова в слот staff.
UPDATE characters SET profile = 'rogue' WHERE profile = 'archer';
UPDATE items SET equipped_slot = 'staff'
 WHERE equipped_slot = 'main_hand'
   AND def_id IN (SELECT id FROM item_defs WHERE item_type = 'staff');
ALTER TABLE item_defs DROP COLUMN slot;
ALTER TABLE item_defs DROP COLUMN hands;
