use std::path::{Path, PathBuf};

include!("src/icon.rs");

const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=app.manifest");

    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-arg-bins=/PDBALTPATH:%_PDB%");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("app.ico"), ico_bytes()).unwrap();
    let script = resource_script(&out.join("app.ico"), &root.join("app.manifest"));
    std::fs::write(out.join("app.rc"), script).unwrap();
    embed_resource::compile(out.join("app.rc"), embed_resource::NONE).manifest_required().unwrap();
}

fn resource_script(icon: &Path, manifest: &Path) -> String {
    let quoted = |path: &Path| path.display().to_string().replace('\\', "\\\\");
    let version = env!("CARGO_PKG_VERSION");
    let numbers = version.replace('.', ",");
    format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numbers},0
PRODUCTVERSION {numbers},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{name}"
      VALUE "FileVersion", "{version}"
      VALUE "LegalCopyright", "Copyright (c) 2026 tvvt3aiio"
      VALUE "OriginalFilename", "{name}.exe"
      VALUE "ProductName", "{name}"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#,
        icon = quoted(icon),
        manifest = quoted(manifest),
        name = env!("CARGO_PKG_NAME"),
    )
}

fn ico_bytes() -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICO_SIZES.iter().map(|&s| encode_png(&render(Glyph::App, s), s)).collect();
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((ICO_SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * ICO_SIZES.len() as u32;
    for (&size, image) in ICO_SIZES.iter().zip(&images) {
        let side = if size == 256 { 0 } else { size as u8 };
        out.extend([side, side, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((image.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend(image);
    }
    out
}

fn encode_png(rgba: &[u8], size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let indexed = index_colors(rgba);
    match &indexed {
        Some((palette, _)) => {
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_palette(palette.iter().flat_map(|c| &c[..3]).copied().collect::<Vec<u8>>());
            encoder.set_trns(palette.iter().map(|c| c[3]).collect::<Vec<u8>>());
            encoder.set_filter(png::Filter::NoFilter);
        }
        None => {
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_filter(png::Filter::Adaptive);
        }
    }
    let pixels = indexed.as_ref().map_or(rgba, |(_, indices)| indices);
    encoder.write_header().unwrap().write_image_data(pixels).unwrap();
    out
}

fn index_colors(rgba: &[u8]) -> Option<(Vec<[u8; 4]>, Vec<u8>)> {
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut indices = Vec::with_capacity(rgba.len() / 4);
    for p in rgba.chunks_exact(4) {
        let color = [p[0], p[1], p[2], p[3]];
        let index = palette.iter().position(|c| *c == color).unwrap_or_else(|| {
            palette.push(color);
            palette.len() - 1
        });
        indices.push(u8::try_from(index).ok()?);
    }
    Some((palette, indices))
}
