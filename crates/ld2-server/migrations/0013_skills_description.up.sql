-- M14/F17 (PLAN-M14): описание навыка для UI (тултип/карточка) —
-- приходит в SkillSnapshot.description; DEFAULT '' старые строки берёт.
ALTER TABLE skills
    ADD COLUMN description TEXT NOT NULL DEFAULT '';
