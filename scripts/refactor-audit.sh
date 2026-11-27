#!/usr/bin/env bash
# RF0e: аудит «бюджета рефакторинга» — размеры прод-кода, функции, тесты.
#
# Замеры (правило §15 плана): прод = файл минус хвост `#[cfg(test)] mod tests`;
# функции — по балансу скобок; тесты — `#[test]` в `crates/*/src`.
#
#   scripts/refactor-audit.sh            — отчёт (ничего не проверяет)
#   scripts/refactor-audit.sh --verify   — проверка бюджета (CI, gate)
#   scripts/refactor-audit.sh --update   — опустить лимиты до текущих
#                                          (поднять без --force нельзя)
#
# Бюджет — `scripts/refactor-budget.txt`, ратчет: лимит только уменьшается,
# минимум тестов только растёт.
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 - "$@" <<'PY'
import re
import sys
from pathlib import Path

ROOT = Path.cwd()
BUDGET = ROOT / "scripts" / "refactor-budget.txt"
SRC = sorted(ROOT.glob("crates/*/src/**/*.rs"))

PROD_HARD_LIMIT = 900      # план §12: ни один файл не должен перейти порог
FN_HARD_LIMIT = 200        # план §12: ни одной функции > 200 строк


def sanitize(text: str) -> str:
    """Заменяет строки/символы/комментарии пробелами, сохраняя переводы строк."""
    out = list(text)
    i, n = 0, len(text)
    state = None  # 'str' | 'char' | 'line' | 'block'
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if state is None:
            if c == '"':
                state = "str"
                out[i] = " "
            elif c == "'" and (text[i + 2 : i + 3] == "'" or (nxt == "\\" and text[i + 3 : i + 4] == "'")):
                state = "char"
                out[i] = " "
            elif c == "/" and nxt == "/":
                state = "line"
                out[i] = out[i + 1] = " "
                i += 2
                continue
            elif c == "/" and nxt == "*":
                state = "block"
                out[i] = out[i + 1] = " "
                i += 2
                continue
        elif state == "str":
            if c == "\\":
                out[i] = " "
                if i + 1 < n:
                    out[i + 1] = " "
                    i += 2
                    continue
            elif c == '"':
                state = None
            out[i] = " "
        elif state == "char":
            if c == "\\":
                out[i] = " "
                if i + 1 < n:
                    out[i + 1] = " "
                    i += 2
                    continue
            elif c == "'":
                state = None
            out[i] = " "
        elif state == "line":
            if c == "\n":
                state = None
            else:
                out[i] = " "
        elif state == "block":
            if c == "*" and nxt == "/":
                state = None
                out[i] = out[i + 1] = " "
                i += 2
                continue
            out[i] = " "
        i += 1
    return "".join(out)


def prod_text(text: str) -> str:
    """Файл без хвоста `#[cfg(test)] mod tests`."""
    lines = text.splitlines(keepends=True)
    for i, line in enumerate(lines):
        if line.startswith("#[cfg(test)]"):
            rest = [l for l in lines[i + 1 :] if l.strip()]
            if rest and rest[0].lstrip().startswith("mod tests"):
                return "".join(lines[:i])
    return text


def functions(text: str):
    """[(имя, первая строка, последняя строка)] по балансу скобок."""
    code = sanitize(text)
    out = []
    for m in re.finditer(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", code):
        name = m.group(1)
        open_brace = code.find("{", m.end())
        if open_brace < 0:
            continue
        depth = 0
        end = None
        for j in range(open_brace, len(code)):
            c = code[j]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    end = j
                    break
        if end is None:
            continue
        start_line = text.count("\n", 0, m.start()) + 1
        end_line = text.count("\n", 0, end) + 1
        out.append((name, start_line, end_line))
    return out


def measure():
    files = []
    for path in SRC:
        rel = path.relative_to(ROOT).as_posix()
        raw = path.read_text(encoding="utf-8")
        prod = prod_text(raw)
        fns = functions(prod)
        sizes = [(f[0], f[2] - f[1] + 1) for f in fns]
        longest = max(sizes, key=lambda f: f[1], default=None)
        files.append(
            {
                "path": rel,
                "prod": prod.count("\n") + (0 if prod.endswith("\n") or not prod else 1),
                "tests": raw.count("#[test]"),
                "max_fn": longest[1] if longest else 0,
                "max_fn_name": longest[0] if longest else "",
                "fns": sizes,
            }
        )
    return files


def read_budget():
    prod, fns, tests = {}, {}, {}
    if not BUDGET.exists():
        return prod, fns, tests
    for line in BUDGET.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if parts[0] == "tests":
            tests[parts[1]] = int(parts[2])
        else:
            prod[parts[0]] = int(parts[1])
            fns[parts[0]] = int(parts[2])
    return prod, fns, tests


def write_budget(files):
    lines = [
        "# Бюджет рефакторинга (RF0e). Формат:",
        "#   <путь> <max прод-строк> <max строк в функции>",
        "#   tests <крейт> <минимум #[test]>",
        "# Ратчет: лимиты только уменьшать, минимум тестов только увеличивать.",
        "# Пересчёт: scripts/refactor-audit.sh --update",
        "",
        "# --- прод-лимиты (файлы > 600 строк и целевые модули) ---",
    ]
    for f in files:
        if f["prod"] > 600 or f["max_fn"] > 200:
            lines.append(f"{f['path']} {f['prod']} {max(f['max_fn'], 150)}")
    lines += ["", "# --- минимум тестов по крейтам ---"]
    for crate in sorted({f["path"].split("/")[1] for f in files}):
        total = sum(f["tests"] for f in files if f["path"].split("/")[1] == crate)
        lines.append(f"tests {crate} {total}")
    BUDGET.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"бюджет записан: {BUDGET.relative_to(ROOT)}")


def report(files):
    print(f"{'файл':52} {'прод':>6} {'фн':>5} {'тесты':>6}")
    for f in sorted(files, key=lambda x: -x["prod"])[:20]:
        print(f"{f['path']:52} {f['prod']:6} {f['max_fn']:5} {f['tests']:6}")
    total = sum(f["prod"] for f in files)
    tests = sum(f["tests"] for f in files)
    big = [f for f in files if f["prod"] > 900]
    long_fns = [(f["path"], n, ln) for f in files for n, ln in f["fns"] if ln > FN_HARD_LIMIT]
    print(f"\nвсего прод-строк: {total}; файлов >900: {len(big)}; функций >200: {len(long_fns)}; тестов: {tests}")
    top = sorted(files, key=lambda x: -x["prod"])[:7]
    print(f"доля 7 крупнейших: {sum(f['prod'] for f in top) / total:.1%}")


def verify(files):
    prod, fns, tests = read_budget()
    errors = []
    for f in files:
        path = f["path"]
        if path in prod:
            if f["prod"] > prod[path]:
                errors.append(f"{path}: прод {f['prod']} > бюджета {prod[path]}")
            limit = fns[path]
        else:
            if f["prod"] > PROD_HARD_LIMIT:
                errors.append(f"{path}: {f['prod']} строк — новый файл > {PROD_HARD_LIMIT}, нужен бюджет/разбиение")
            limit = FN_HARD_LIMIT
        for name, size in f["fns"]:
            if size > limit:
                errors.append(f"{path}:{name} — {size} строк > лимита {limit}")
    for crate, minimum in tests.items():
        have = sum(f["tests"] for f in files if f["path"].split("/")[1] == crate)
        if have < minimum:
            errors.append(f"{crate}: тестов {have} < минимума {minimum}")
    total_tests = sum(f["tests"] for f in files)
    if total_tests < 295:
        errors.append(f"всего тестов {total_tests} < 295")
    if errors:
        print("БЮДЖЕТ НАРУШЕН:")
        for e in errors:
            print("  ✗", e)
        sys.exit(1)
    print(f"бюджет ок: файлов {len(files)}, тестов {total_tests}")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "report"
    files = measure()
    if mode == "--update":
        force = "--force" in sys.argv
        old_prod, old_fns, _ = read_budget()
        if not force:
            raised = [f"{p}: {old_prod[p]} → {files[[x['path'] for x in files].index(p)]['prod']}"
                      for p in old_prod
                      if p in {x['path'] for x in files}
                      and files[[x['path'] for x in files].index(p)]['prod'] > old_prod[p]]
            if raised:
                print("рост бюджета без --force:")
                for r in raised:
                    print("  ✗", r)
                sys.exit(1)
        write_budget(files)
    elif mode == "--verify":
        verify(files)
    else:
        report(files)


main()
PY
