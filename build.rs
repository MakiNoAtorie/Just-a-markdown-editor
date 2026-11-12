// Draws the app icon and embeds it as Windows resource 1. No extra crates: the MSVC linker accepts a .res file.
use std::{env, fs, path::PathBuf};

const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 128];
const BACKGROUND: [u8; 3] = [0x2b, 0x25, 0x20];
const ACCENT: [u8; 3] = [0xd9, 0xaa, 0x72];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icon.res");
    fs::write(&path, resource_file()).unwrap();
    println!("cargo:rustc-link-arg-bin=just-a-markdown-editor={}", path.display());
}

// Returns (background coverage, accent coverage) at a point in the unit square.
fn shape(x: f32, y: f32) -> (bool, bool) {
    let (inset, radius) = (0.04, 0.2);
    let cx = x.clamp(inset + radius, 1. - inset - radius);
    let cy = y.clamp(inset + radius, 1. - inset - radius);
    let inside = (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
        && (inset..=1. - inset).contains(&x)
        && (inset..=1. - inset).contains(&y);
    let half = 0.05;
    let slant = (0.5 - y) * 0.16;
    let vertical = (0.2..=0.8).contains(&y)
        && [0.41, 0.63].iter().any(|c| (x - c - slant).abs() <= half);
    let horizontal = (0.22..=0.80).contains(&x)
        && [0.39, 0.61].iter().any(|c| (y - c).abs() <= half);
    (inside, inside && (vertical || horizontal))
}

// 32-bit BGRA DIB, bottom-up, followed by an empty AND mask.
fn image(size: u32) -> Vec<u8> {
    let mut data = Vec::new();
    for value in [40u32, size, size * 2] {
        data.extend(value.to_le_bytes());
    }
    data.extend(1u16.to_le_bytes());
    data.extend(32u16.to_le_bytes());
    data.extend([0u8; 24]);
    let samples = 4;
    for row in (0..size).rev() {
        for column in 0..size {
            let (mut fill, mut mark) = (0u32, 0u32);
            for sy in 0..samples {
                for sx in 0..samples {
                    let x = (column as f32 + (sx as f32 + 0.5) / samples as f32) / size as f32;
                    let y = (row as f32 + (sy as f32 + 0.5) / samples as f32) / size as f32;
                    let (inside, accent) = shape(x, y);
                    fill += inside as u32;
                    mark += accent as u32;
                }
            }
            let alpha = fill * 255 / (samples * samples);
            let mix = if fill == 0 { 0. } else { mark as f32 / fill as f32 };
            let rgb: Vec<u8> = (0..3)
                .map(|i| (BACKGROUND[i] as f32 * (1. - mix) + ACCENT[i] as f32 * mix) as u8)
                .collect();
            data.extend([rgb[2], rgb[1], rgb[0], alpha as u8]);
        }
    }
    data.extend(vec![0u8; (size.div_ceil(32) * 4 * size) as usize]);
    data
}

fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    for value in [0xffff, kind, 0xffff, id] {
        out.extend(value.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend(data);
    out.resize(out.len().next_multiple_of(4), 0);
}

fn resource_file() -> Vec<u8> {
    let mut out = Vec::new();
    resource(&mut out, 0, 0, 0, &[]);
    let mut group = vec![0, 0, 1, 0, SIZES.len() as u8, 0];
    for (index, &size) in SIZES.iter().enumerate() {
        let data = image(size);
        let id = index as u16 + 1;
        resource(&mut out, 3, id, 0x1010, &data);
        group.extend([size as u8, size as u8, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((data.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    resource(&mut out, 14, 1, 0x1030, &group);
    out
}
