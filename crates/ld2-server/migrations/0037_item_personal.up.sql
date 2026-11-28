-- M39: печать персональности на инстансе — ставится при первом
-- надевании/применении предмета с item_defs.personal.
ALTER TABLE items ADD COLUMN personal BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE items i SET personal = TRUE
  FROM item_defs d
 WHERE i.def_id = d.id AND d.personal AND i.equipped_slot IS NOT NULL;
