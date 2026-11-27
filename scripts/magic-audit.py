#!/usr/bin/env python3
"""Аудит «магических» чисел и слов в Rust-коде (план — plans/PLAN-magic-constants.md).

Ищет в production-коде:
  * «магические слова» — строковые литералы, совпадающие с доменным
    словарём: id предметов/навыков/профессий/локаций/сущностей из
    content/*.json, слоты/профили/редкости/эффекты, а также wire-строки
    `evt/…`, `cmd/…`, `E_…`;
  * «магические числа» — числовые литералы (кроме 0/1) вне `const`/
    `static`; отдельно помечаются UI-значения (px/rem/размеры/отступы).

Не считает: тесты (`/tests/`, `ld2-test-kit`, `#[cfg(test)]`), переводы
(l10n), канонические карты (`errors.rs` — коды ошибок уже в одном месте),
JSON-ключи wire/статики (payload, id, damage, …), строки-логи.

Запуск:
    python3 scripts/magic-audit.py            # сводка + топ-30
    python3 scripts/magic-audit.py --all      # полная таблица по файлам
    python3 scripts/magic-audit.py --markdown # markdown-таблица (для плана)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import Counter, defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CONTENT = os.path.join(ROOT, "content")
CRATES = os.path.join(ROOT, "crates")

STR_RE = re.compile(r'"((?:\\.|[^"\\])*)"')
NUM_RE = re.compile(
    r"(?<![\w.])(-?\d[\d_]*(?:\.[\d_]+)?(?:[eE][+-]?\d+)?)"
    r"(?:_?(?:[iu](?:8|16|32|64|128|size)|f(?:32|64)))?"
)
UI_RE = re.compile(
    r"\b(px|rems?|relative|size|size_full|min_h|min_w|max_h|max_w|"
    r"w|h|gap|gap_x|gap_y|p|px_|py_|pt_|pb_|pl_|pr_|m|mt_|mb_|ml_|mr_|"
    r"mx_|my_|text_size|rounded|border|inset|top|left|right|bottom)\s*\("
)
CONST_RE = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\b")

# Ключи wire/статов: это схема, а не «магические слова».
GENERIC = {
    "id", "name", "text", "type", "kind", "value", "max", "cur", "items",
    "item", "char", "hp", "mp", "damage", "accuracy", "armor", "crit_chance",
    "crit_damage", "dodge", "parry_min", "parry_max", "parry_chance",
    "block_min", "block_max", "block_chance", "magic_defense", "magic_dodge",
    "magic_resist", "magic_dispel", "magic_crit_damage", "magic_crit_chance",
    "str", "dex", "vit", "int", "spir", "skill", "profession", "resource",
    "tool", "spell", "school", "class", "param", "attr", "effect", "buff",
    "buff2", "charges", "chance", "cost", "duration_s", "max_stacks",
    "period_s", "glyph", "pct", "qty", "rarity", "slot", "hands", "upgrade",
    "gold_value", "weight", "cooldown_s", "dmg", "heal", "magic", "quality",
}

# Доменные перечисления (не выводятся из content/*.json).
ENUM = {
    "warrior", "archer", "mage", "male", "female",
    "stun", "silence", "blind", "bleed", "root", "burn",
    "main_hand", "off_hand", "head", "body", "legs", "feet", "gloves",
    "shoulders", "bracers", "cloak", "ring", "ring1", "ring2", "amulet",
    "artifact", "artifact1", "artifact2",
    "common", "uncommon", "rare", "epic", "legendary",
    "weapon", "armor", "shield", "gem", "rune", "staff", "hide",
    "hide_scraps", "money", "potion", "scroll", "bag", "ammo", "key",
    "meat", "offal", "skin", "gut",
    "system", "local", "trade", "guild", "party", "world", "general",
    "reagent", "quiver", "keyring",
}

# Файлы с каноническим источником строк: выносить нечего.
CANONICAL = {
    "crates/ld2-protocol/src/errors.rs",
}
# Явные UI-файлы: числа — дизайн-токены, не доменные константы.
UI_FILES = {
    "crates/ld2-client/src/root/popups.rs",
    "crates/ld2-client/src/root/game/map.rs",
    "crates/ld2-editor/src/app.rs",
}


def content_vocab() -> set[str]:
    vocab: set[str] = set()
    for fn in ("items", "skills", "professions", "locations", "quests",
               "recipes", "shops", "dialogs"):
        path = os.path.join(CONTENT, f"{fn}.json")
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)

        def walk(node) -> None:
            if isinstance(node, dict):
                for key, value in node.items():
                    if key in ("id", "def_id") and isinstance(value, str):
                        vocab.add(value)
                    walk(value)
            elif isinstance(node, list):
                for value in node:
                    walk(value)

        walk(data)
    return vocab


def strip_comment(line: str) -> str:
    """Убрать `// …` вне строк, сохранив содержимое строк."""
    out: list[str] = []
    i = 0
    in_str = False
    esc = False
    while i < len(line):
        ch = line[i]
        if in_str:
            out.append(ch)
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
            i += 1
            continue
        if ch == '"':
            in_str = True
            out.append(ch)
            i += 1
            continue
        if ch == "/" and i + 1 < len(line) and line[i + 1] == "/":
            break
        out.append(ch)
        i += 1
    return "".join(out)


def rust_files() -> list[str]:
    files: list[str] = []
    for dirpath, dirnames, filenames in os.walk(CRATES):
        dirnames[:] = [d for d in dirnames if d not in ("target", ".git")]
        for fn in filenames:
            if fn.endswith(".rs"):
                files.append(os.path.relpath(os.path.join(dirpath, fn), ROOT))
    return sorted(files)


def audit_file(path: str, vocab: set[str]) -> dict:
    is_test = "/tests/" in path or "/ld2-test-kit/" in path
    is_canonical = path in CANONICAL
    lines = open(os.path.join(ROOT, path), encoding="utf-8").read().splitlines()
    in_block = False
    in_test_mod = False
    in_const = False
    words: list[tuple[int, str]] = []
    nums: list[tuple[int, str]] = []
    ui_nums: list[tuple[int, str]] = []
    for lineno, line in enumerate(lines, 1):
        if in_block:
            if "*/" in line:
                line = line.split("*/", 1)[1]
                in_block = False
            else:
                continue
        if "/*" in line and "*/" not in line:
            line = line.split("/*", 1)[0]
            in_block = True
        clean = strip_comment(line)
        if "#[cfg(test)]" in clean:
            in_test_mod = True
        # Строка определения const/static (в т.ч. однострочного) — не
        # кандидат; многострочное тело остаётся под in_const до `;`.
        const_line = in_const or CONST_RE.match(clean) is not None
        if CONST_RE.match(clean):
            in_const = True
        if in_const and clean.rstrip().endswith(";"):
            in_const = False
        if is_test or in_test_mod or const_line:
            continue
        for word in STR_RE.findall(clean):
            if word in vocab or word.startswith(("evt/", "cmd/", "E_")):
                words.append((lineno, word))
        table = UI_RE.search(clean) is not None or path in UI_FILES
        for match in NUM_RE.finditer(STR_RE.sub('""', clean)):
            raw = match.group(1)
            try:
                value = float(raw.replace("_", ""))
            except ValueError:
                continue
            if abs(value) in (0.0, 1.0):
                continue
            (ui_nums if table else nums).append((lineno, raw))
    can_fix_words = 0 if is_canonical else len(words)
    can_fix_nums = len(nums) + len(ui_nums)
    return {
        "path": path,
        "test": is_test,
        "canonical": is_canonical,
        "words": words,
        "nums": nums,
        "ui_nums": ui_nums,
        "fix_words": can_fix_words,
        "fix_nums": can_fix_nums,
        "fix_total": can_fix_words + can_fix_nums,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Аудит магических литералов")
    parser.add_argument("--all", action="store_true", help="все файлы")
    parser.add_argument("--top", type=int, default=30, help="сколько строк")
    parser.add_argument("--markdown", action="store_true", help="markdown")
    args = parser.parse_args()

    vocab = (content_vocab() | ENUM) - GENERIC
    rows = []
    file_count = 0
    test_count = 0
    for path in rust_files():
        if "/tests/" in path or "/ld2-test-kit/" in path:
            test_count += 1
            continue
        file_count += 1
        row = audit_file(path, vocab)
        if row["words"] or row["nums"] or row["ui_nums"]:
            rows.append(row)

    total_words = sum(len(r["words"]) for r in rows)
    total_nums = sum(len(r["nums"]) for r in rows)
    total_ui = sum(len(r["ui_nums"]) for r in rows)
    fix_words = sum(r["fix_words"] for r in rows)
    fix_nums = sum(r["fix_nums"] for r in rows)
    files = sum(1 for r in rows if r["fix_total"] > 0)
    values = Counter(
        raw for r in rows for _, raw in (r["nums"] + r["ui_nums"])
    )

    print(
        f"Rust: {file_count + test_count} файлов, production {file_count}, "
        f"тесты {test_count}"
    )
    print(
        f"Найдено в production: слов {total_words}, чисел {total_nums} "
        f"(+ UI {total_ui}); файлов с кандидатами {files}"
    )
    print(
        f"К выносу: слов {fix_words}, чисел {fix_nums} "
        f"(всего {fix_words + fix_nums}); уникальных чисел {len(values)}"
    )
    print(
        "Не считается: тесты/test-kit, l10n, канонические карты "
        f"({', '.join(CANONICAL)}), JSON-ключи wire/статов"
    )

    rows.sort(key=lambda r: (-r["fix_total"], r["path"]))
    show = rows if args.all else rows[: args.top]
    if args.markdown:
        print()
        print("| Файл | Слова | Числа | UI | Исправимо |")
        print("|---|---:|---:|---:|---:|")
        for r in show:
            print(
                f"| `{r['path']}` | {len(r['words'])} | {len(r['nums'])} | "
                f"{len(r['ui_nums'])} | {r['fix_total']} |"
            )
    else:
        print()
        for r in show:
            print(
                f"{r['path']}: слова {len(r['words'])} | числа "
                f"{len(r['nums'])} + UI {len(r['ui_nums'])} | "
                f"исправимо {r['fix_total']}"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
