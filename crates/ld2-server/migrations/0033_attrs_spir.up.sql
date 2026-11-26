-- M30: атрибут «духовность» — макс. мана и реген маны (character-params.md §1.1).
UPDATE characters SET attrs = attrs || '{"spir":10}'::jsonb
 WHERE NOT (attrs ? 'spir');
ALTER TABLE characters
    ALTER COLUMN attrs SET DEFAULT '{"str":10,"dex":10,"vit":10,"int":10,"spir":10}';
