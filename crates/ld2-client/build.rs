//! M24: Font Awesome Free — генерация таблицы иконок в
//! `OUT_DIR/fa_icons.rs`: путь `<стиль>/<имя>` → SVG-байты.
//!
//! Файлы из `assets/fa` не правятся — в них лежит embedded-комментарий
//! лицензии CC BY 4.0 (атрибуция), см. `assets/fa/LICENSE.txt`.
//!
//! Windows: встраивание `assets/icons/icon.ico` в exe (иконка в
//! проводнике, таскбаре и ярлыках; окно на Windows берёт её же).
//! Гейт — `CARGO_CFG_TARGET_OS` (паттерн из README winresource):
//! build.rs компилируется под хостом, а не под целевой ОС.

use std::{env, fs, path::Path};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    windows_icon(&manifest);
    let root = Path::new(&manifest).join("assets/fa");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut entries: Vec<(String, String)> = Vec::new();
    for style in ["solid", "regular", "brands"] {
        let dir = root.join(style);
        let mut files: Vec<_> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("каталог {}: {e}", dir.display()))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
            .collect();
        files.sort();
        for file in files {
            let name = file
                .file_stem()
                .expect("имя файла")
                .to_string_lossy()
                .to_string();
            // Абсолютный путь для include_bytes!; на Windows — прямые
            // слэши (обратные были бы escape-последовательностью).
            let abs = file
                .canonicalize()
                .unwrap_or_else(|e| panic!("canonicalize {}: {e}", file.display()))
                .to_string_lossy()
                .replace('\\', "/");
            entries.push((format!("{style}/{name}"), abs));
        }
    }
    assert!(
        entries.len() > 1_000,
        "набор Font Awesome не найден в assets/fa: {} иконок",
        entries.len()
    );

    // Обычные `//`-комментарии: файл подставляется через include!
    // после модульных доков fa.rs — inner-doc там был бы ошибкой.
    let mut out = String::from(
        "// Генерируется build.rs из assets/fa — таблица Font Awesome Free.\n\
         // Путь: `<стиль>/<имя>`; лицензия иконок CC BY 4.0\n\
         // (атрибуция — в самих SVG и в assets/fa/LICENSE.txt).\n\
         pub static ICONS: &[(&str, &[u8])] = &[\n",
    );
    for (path, file) in &entries {
        out.push_str(&format!("    ({path:?}, include_bytes!({file:?})),\n"));
    }
    out.push_str("];\n\n");
    out.push_str("// SVG-байты иконки; None — такого пути в наборе нет.\n");
    out.push_str("pub fn get(path: &str) -> Option<&'static [u8]> {\n");
    out.push_str("    ICONS.iter().find(|(p, _)| *p == path).map(|(_, bytes)| *bytes)\n");
    out.push_str("}\n");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    fs::write(Path::new(&out_dir).join("fa_icons.rs"), out).expect("запись fa_icons.rs");
}

/// Windows: icon.ico → ресурс exe (RT_GROUP_ICON). На не-Windows
/// целевые ОС (в т.ч. при сборке на mac/linux) — ранний выход:
/// иконка не нужна, и winresource не вызывается вовсе.
fn windows_icon(manifest: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = Path::new(manifest).join("assets/icons/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    winresource::WindowsResource::new()
        .set_icon(icon.to_str().expect("путь icon.ico в utf8"))
        .compile()
        .expect("встраивание icon.ico в exe (winresource)");
}
