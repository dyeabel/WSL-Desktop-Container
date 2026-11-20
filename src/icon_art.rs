//! Ikona aplikace (zlaté kostky s tučňákem na tmavém zaobleném čtverci).
//! Pixely jsou předgenerované ze `assets/icon.jpg` skriptem `scripts/make_icon.py`
//! (`assets/icon/<n>.rgba`). Sdílí ji build.rs (ikona EXE) i aplikace (okno, tray, oznámení).

const DARK: [f32; 3] = [0x1B as f32, 0x19 as f32, 0x16 as f32];

/// Předgenerované velikosti (RGBA, řádky shora dolů).
const PREPARED: [(u32, &[u8]); 9] = [
    (16, include_bytes!("../assets/icon/16.rgba")),
    (20, include_bytes!("../assets/icon/20.rgba")),
    (24, include_bytes!("../assets/icon/24.rgba")),
    (32, include_bytes!("../assets/icon/32.rgba")),
    (40, include_bytes!("../assets/icon/40.rgba")),
    (48, include_bytes!("../assets/icon/48.rgba")),
    (64, include_bytes!("../assets/icon/64.rgba")),
    (128, include_bytes!("../assets/icon/128.rgba")),
    (256, include_bytes!("../assets/icon/256.rgba")),
];

/// Ikona bez stavové tečky: přesná předgenerovaná velikost, jinak zmenšení nejbližší větší
/// (průměr plochy, s předem vynásobenou průhledností).
fn base(size: u32) -> Vec<u8> {
    if let Some((_, d)) = PREPARED.iter().find(|(n, _)| *n == size) {
        return d.to_vec();
    }
    let (src_n, src) = PREPARED.iter().find(|(n, _)| *n > size).copied().unwrap_or(PREPARED[PREPARED.len() - 1]);
    let mut out = vec![0u8; (size * size * 4) as usize];
    let scale = src_n as f32 / size as f32;
    for py in 0..size {
        for px in 0..size {
            let (x0, x1) = ((px as f32 * scale) as u32, (((px + 1) as f32 * scale).ceil() as u32).min(src_n));
            let (y0, y1) = ((py as f32 * scale) as u32, (((py + 1) as f32 * scale).ceil() as u32).min(src_n));
            let mut acc = [0f32; 4];
            let mut cnt = 0f32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * src_n + sx) * 4) as usize;
                    let a = src[i + 3] as f32;
                    acc[0] += src[i] as f32 * a;
                    acc[1] += src[i + 1] as f32 * a;
                    acc[2] += src[i + 2] as f32 * a;
                    acc[3] += a;
                    cnt += 1.0;
                }
            }
            let i = ((py * size + px) * 4) as usize;
            if acc[3] > 0.0 {
                out[i] = (acc[0] / acc[3]).round() as u8;
                out[i + 1] = (acc[1] / acc[3]).round() as u8;
                out[i + 2] = (acc[2] / acc[3]).round() as u8;
            }
            out[i + 3] = (acc[3] / cnt.max(1.0)).round() as u8;
        }
    }
    out
}

/// RGBA pixely (řádky shora dolů) ikony o velikosti `size`×`size`.
/// `dot` přidá stavovou tečku dané barvy vpravo dole (pro ikonu v tray).
pub fn render(size: u32, dot: Option<[u8; 3]>) -> Vec<u8> {
    let mut out = base(size);
    let Some(c) = dot else { return out };
    const SS: u32 = 4;
    for py in 0..size {
        for px in 0..size {
            // podíl vzorků v tečce a v tmavém lemu kolem ní
            let (mut in_dot, mut in_ring) = (0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let d = ((x - 0.78).powi(2) + (y - 0.78).powi(2)).sqrt();
                    if d <= 0.17 {
                        in_dot += 1.0;
                    } else if d <= 0.23 {
                        in_ring += 1.0;
                    }
                }
            }
            let n = (SS * SS) as f32;
            let cover = (in_dot + in_ring) / n;
            if cover == 0.0 {
                continue;
            }
            let i = ((py * size + px) * 4) as usize;
            let col = |k: usize| (c[k] as f32 * in_dot + DARK[k] * in_ring) / (in_dot + in_ring);
            for k in 0..3 {
                out[i + k] = (col(k) * cover + out[i + k] as f32 * (1.0 - cover)).round() as u8;
            }
            out[i + 3] = (255.0 * cover + out[i + 3] as f32 * (1.0 - cover)).round().min(255.0) as u8;
        }
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Nekomprimované PNG (deflate „stored“ bloky) – stačí pro ikonu 256 px.
#[allow(dead_code)]
pub fn png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((size * (size * 4 + 1)) as usize);
    for y in 0..size as usize {
        raw.push(0); // filtr None
        raw.extend_from_slice(&rgba[y * size as usize * 4..(y + 1) * size as usize * 4]);
    }
    // zlib: hlavička + stored bloky + adler32
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, c) in chunks.iter().enumerate() {
        z.push(if i + 1 == chunks.len() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |ty: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bit, RGBA
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// Sestaví .ico soubor z několika velikostí (do 128 px BMP, 256 px PNG).
#[allow(dead_code)]
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&s| {
            let rgba = render(s, None);
            if s >= 256 {
                return (s, png(s, &rgba));
            }
            let mut bmp = Vec::new();
            // BITMAPINFOHEADER (výška ×2 = barva + maska)
            bmp.extend_from_slice(&40u32.to_le_bytes());
            bmp.extend_from_slice(&(s as i32).to_le_bytes());
            bmp.extend_from_slice(&((s * 2) as i32).to_le_bytes());
            bmp.extend_from_slice(&1u16.to_le_bytes());
            bmp.extend_from_slice(&32u16.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            let mask_row = s.div_ceil(32) * 4;
            bmp.extend_from_slice(&(s * s * 4 + mask_row * s).to_le_bytes());
            bmp.extend_from_slice(&[0u8; 16]);
            // pixely BGRA zdola nahoru
            for y in (0..s).rev() {
                for x in 0..s {
                    let i = ((y * s + x) * 4) as usize;
                    bmp.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            // AND maska (vše 0 – průhlednost řeší alfa kanál)
            bmp.extend(std::iter::repeat_n(0u8, (mask_row * s) as usize));
            (s, bmp)
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, data) in &images {
        let dim = if *s >= 256 { 0u8 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}
