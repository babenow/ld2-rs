-- M39: текстовое описание предмета для карточки/тултипа.
ALTER TABLE item_defs ADD COLUMN description TEXT NOT NULL DEFAULT '';
