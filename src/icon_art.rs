//! Kresba ikony aplikace (zlatý zaoblený čtverec se „stohem“ kontejnerů).
//! Sdílí ji build.rs (ikona EXE) i aplikace (ikona okna a traye).
//! Vykresluje se v libovolné velikosti se supersamplingem 4×4.

const GOLD: [f32; 3] = [0xDB as f32, 0xB4 as f32, 0x7C as f32];
const DARK: [f32; 3] = [0x1E as f32, 0x1A as f32, 0x14 as f32];

/// Je bod (x, y) v zaobleném obdélníku? Souřadnice 0..1.
fn in_rrect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Barva vzorku: None = průhledné.
fn sample(x: f32, y: f32, dot: Option<[u8; 3]>) -> Option<[f32; 3]> {
    if let Some(c) = dot {
        // stavová tečka vpravo dole s tmavým lemem
        let d = ((x - 0.78).powi(2) + (y - 0.78).powi(2)).sqrt();
        if d <= 0.17 {
            return Some([c[0] as f32, c[1] as f32, c[2] as f32]);
        }
        if d <= 0.23 {
            return Some(DARK);
        }
    }
    if !in_rrect(x, y, 0.04, 0.04, 0.96, 0.96, 0.22) {
        return None;
    }
    let boxes = [
        (0.355, 0.20, 0.645, 0.45),
        (0.19, 0.53, 0.48, 0.79),
        (0.52, 0.53, 0.81, 0.79),
    ];
    for (x0, y0, x1, y1) in boxes {
        if in_rrect(x, y, x0, y0, x1, y1, 0.035) {
            // vodorovná „žebra“ kontejneru
            let rel = (y - y0) / (y1 - y0);
            let rib = (rel * 3.0).fract() < 0.18 && rel > 0.15;
            return Some(if rib { [0x3A as f32, 0x31 as f32, 0x24 as f32] } else { DARK });
        }
    }
    Some(GOLD)
}

/// RGBA pixely (řádky shora dolů) ikony o velikosti `size`×`size`.
/// `dot` přidá stavovou tečku dané barvy (pro ikonu v tray).
pub fn render(size: u32, dot: Option<[u8; 3]>) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some(c) = sample(x, y, dot) {
                        r += c[0];
                        g += c[1];
                        b += c[2];
                        a += 1.0;
                    }
                }
            }
            let i = ((py * size + px) * 4) as usize;
            if a > 0.0 {
                out[i] = (r / a).round() as u8;
                out[i + 1] = (g / a).round() as u8;
                out[i + 2] = (b / a).round() as u8;
                out[i + 3] = (a / (SS * SS) as f32 * 255.0).round() as u8;
            }
        }
    }
    out
}

#[allow(dead_code)]
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
            bmp.extend(std::iter::repeat(0u8).take((mask_row * s) as usize));
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
