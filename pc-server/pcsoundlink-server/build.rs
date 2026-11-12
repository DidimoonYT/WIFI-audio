//! Сборка ресурсов: растрируем SVG-иконки в RGBA и собираем .ico для exe.
//!
//! Иконки лежат в `<repo>/assets/icons`. В `OUT_DIR` пишем сырые RGBA-файлы,
//! которые затем подключаются через `include_bytes!` в `src/icons.rs`.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icons_dir = manifest.join("..").join("..").join("assets").join("icons");

    let computer_svg = icons_dir.join("computer-wifi-sound.svg");
    let phone_svg = icons_dir.join("phone-wifi-sound.svg");

    println!("cargo:rerun-if-changed={}", computer_svg.display());
    println!("cargo:rerun-if-changed={}", phone_svg.display());
    println!("cargo:rerun-if-changed=build.rs");

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out = PathBuf::from(out_dir);

    // Иконка окна/уведомлений и маленькая иконка трея.
    render(&computer_svg, 256, &out.join("computer-256.rgba"));
    render(&computer_svg, 32, &out.join("computer-32.rgba"));
    render(&phone_svg, 256, &out.join("phone-256.rgba"));
    render(&phone_svg, 32, &out.join("phone-32.rgba"));

    // Многоразмерный .ico для файла программы (Проводник и панель задач).
    let ico = out.join("app.ico");
    if write_ico(&computer_svg, &ico).is_ok() {
        embed_exe_icon(&ico);
    }
}

fn load_svg(path: &Path) -> Option<resvg::usvg::Tree> {
    let data = std::fs::read(path).ok()?;
    let options = resvg::usvg::Options::default();
    resvg::usvg::Tree::from_data(&data, &options).ok()
}

/// Рендерит SVG в квадрат `size`×`size` и сохраняет несжатые RGBA-байты.
fn render(svg: &Path, size: u32, dest: &Path) {
    let Some(tree) = load_svg(svg) else {
        panic!("не удалось прочитать SVG: {}", svg.display());
    };
    let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(size, size) else {
        panic!("не удалось создать изображение {size}×{size}");
    };

    let scale = size as f32 / 512.0;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia хранит цвет с предумноженной альфой — приводим к обычному RGBA.
    let mut rgba = pixmap.data().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            for channel in &mut px[..3] {
                *channel = (((*channel as u32) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    std::fs::write(dest, rgba).unwrap_or_else(|err| {
        panic!("не удалось записать {}: {err}", dest.display());
    });
}

/// Собирает .ico из BMP-записей (32 бита) в нескольких размерах.
fn write_ico(svg: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let tree = load_svg(svg).ok_or("не удалось прочитать SVG")?;
    let sizes: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

    let mut images: Vec<Vec<u8>> = Vec::new();
    for size in sizes {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or("pixmap")?;
        let scale = size as f32 / 512.0;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );

        let w = size as usize;
        let h = size as usize;
        let rgba = pixmap.data();

        let mask_row = ((w + 31) / 32) * 4;
        let mask_len = mask_row * h;
        let mut image = Vec::with_capacity(40 + w * h * 4 + mask_len);

        // BITMAPINFOHEADER
        image.extend_from_slice(&40u32.to_le_bytes());
        image.extend_from_slice(&(w as i32).to_le_bytes());
        image.extend_from_slice(&((h as i32) * 2).to_le_bytes());
        image.extend_from_slice(&1u16.to_le_bytes());
        image.extend_from_slice(&32u16.to_le_bytes());
        image.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        image.extend_from_slice(&((w * h * 4 + mask_len) as u32).to_le_bytes());
        image.extend_from_slice(&[0u8; 16]); // разрешение и палитра

        // Пиксели BGRA снизу вверх.
        for y in (0..h).rev() {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let a = rgba[i + 3];
                let (r, g, b) = unpremultiply(rgba[i], rgba[i + 1], rgba[i + 2], a);
                image.push(b);
                image.push(g);
                image.push(r);
                image.push(a);
            }
        }

        // AND-маска: 0 — пиксель непрозрачен, 1 — прозрачен.
        for y in (0..h).rev() {
            let mut row = vec![0u8; mask_row];
            for x in 0..w {
                if rgba[(y * w + x) * 4 + 3] == 0 {
                    row[x / 8] |= 0x80 >> (x % 8);
                }
            }
            image.extend_from_slice(&row);
        }

        images.push(image);
    }

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    for (size, image) in sizes.iter().zip(&images) {
        let edge = if *size >= 256 { 0 } else { *size as u8 };
        ico.push(edge); // width
        ico.push(edge); // height
        ico.push(0); // палитра
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in &images {
        ico.extend_from_slice(image);
    }

    std::fs::write(dest, ico)?;
    Ok(())
}

fn unpremultiply(r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8) {
    let a = a as u32;
    if a == 0 {
        return (0, 0, 0);
    }
    if a == 255 {
        return (r, g, b);
    }
    let fix = |c: u8| (((c as u32) * 255 + a / 2) / a).min(255) as u8;
    (fix(r), fix(g), fix(b))
}

/// Встраивает иконку в PE-файл, чтобы она была видна в Проводнике.
#[cfg(windows)]
fn embed_exe_icon(ico: &Path) {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(&ico.to_string_lossy());
    resource.set("ProductName", "PC Sound Link Server");
    resource.set(
        "FileDescription",
        "PC Sound Link — передача звука и экрана на Android",
    );
    resource.set("LegalCopyright", "PcSoundLink");
    if let Err(err) = resource.compile() {
        println!("cargo:warning=не удалось встроить иконку в exe: {err}");
    }
}

#[cfg(not(windows))]
fn embed_exe_icon(_ico: &Path) {}
