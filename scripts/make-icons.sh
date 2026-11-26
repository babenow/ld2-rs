#!/usr/bin/env bash
# Генерация иконок приложения Likedimion из векторного исходника.
#
# Исходник — crates/ld2-client/assets/icons/icon.svg (единственная точка
# правды). Скрипт рендерит PNG нужных размеров и собирает платформенные
# форматы:
#   - icon-<size>.png  — Linux (hicolor), окна x11 (gpui), README
#   - icon.icns        — macOS .app (Resources/icon.icns)
#   - icon.ico         — Windows (встраивается в exe через build.rs +
#                        winresource; также кладётся в релизный zip)
#
# Требования: rsvg-convert, magick; iconutil — только на macOS.
# Все сгенерированные файлы коммитятся: build.rs и main.rs читают их
# напрямую (include_bytes!), CI рендер не запускает.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ICONS="$root/crates/ld2-client/assets/icons"
SRC="$ICONS/icon.svg"

for tool in rsvg-convert magick; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "не найден $tool (brew install librsvg imagemagick)" >&2
        exit 1
    }
done

# PNG-набор: от 16 (таскбар/иконки в UI) до 1024 (мастер для icns).
for size in 16 24 32 48 64 128 256 512 1024; do
    rsvg-convert -w "$size" -h "$size" "$SRC" -o "$ICONS/icon-$size.png"
done

# macOS: iconset с именами Apple (у всех ступеней есть и @2x-варианты).
if command -v iconutil >/dev/null 2>&1; then
    ICONSET="$ICONS/icon.iconset"
    rm -rf "$ICONSET"
    mkdir -p "$ICONSET"
    cp "$ICONS/icon-16.png"   "$ICONSET/icon_16x16.png"
    cp "$ICONS/icon-32.png"   "$ICONSET/icon_16x16@2x.png"
    cp "$ICONS/icon-32.png"   "$ICONSET/icon_32x32.png"
    cp "$ICONS/icon-64.png"   "$ICONSET/icon_32x32@2x.png"
    cp "$ICONS/icon-128.png"  "$ICONSET/icon_128x128.png"
    cp "$ICONS/icon-256.png"  "$ICONSET/icon_128x128@2x.png"
    cp "$ICONS/icon-256.png"  "$ICONSET/icon_256x256.png"
    cp "$ICONS/icon-512.png"  "$ICONSET/icon_256x256@2x.png"
    cp "$ICONS/icon-512.png"  "$ICONSET/icon_512x512.png"
    cp "$ICONS/icon-1024.png" "$ICONSET/icon_512x512@2x.png"
    iconutil -c icns "$ICONSET" -o "$ICONS/icon.icns"
    rm -rf "$ICONSET"
    echo "icon.icns — готово"
else
    echo "iconutil не найден (не macOS) — icon.icns пропущен" >&2
fi

# Windows: многослойный .ico (16…256; 256 хранится как PNG).
magick "$ICONS/icon-16.png" "$ICONS/icon-24.png" "$ICONS/icon-32.png" \
       "$ICONS/icon-48.png" "$ICONS/icon-64.png" "$ICONS/icon-128.png" \
       "$ICONS/icon-256.png" "$ICONS/icon.ico"

echo "иконки сгенерированы в $ICONS"
