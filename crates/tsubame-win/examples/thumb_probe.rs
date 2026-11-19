//! Thumbnail flag probe. RIG ONLY (shell COM). Writes fresh random images (not
//! in the thumbnail cache), then asks `IShellItemImageFactory::GetImage` with
//! different flag sets and reports per file: HRESULT, size, whether the bitmap
//! is the generic type icon.
//!
//! ```text
//! thumb_probe <scratch dir> [count] [make]   (make: only write fresh fixtures img_NN.png)
//! ```

use std::time::Instant;
use tsubame_win::{bitmap, wide};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Shell::*;
use windows::core::PCWSTR;

fn crc32(data: &[u8], mut c: u32) -> u32 {
    c = !c;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// 24-bit RGB PNG with stored (uncompressed) deflate blocks: random content from `seed`.
fn png(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    let (r0, g0, b0) = ((seed % 200) as u8, ((seed >> 8) % 200) as u8, ((seed >> 16) % 200) as u8);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let n = (s & 31) as u8;
            raw.push(r0.wrapping_add((x / 8) as u8).wrapping_add(n));
            raw.push(g0.wrapping_add((y / 8) as u8).wrapping_add(n));
            raw.push(b0.wrapping_add(n));
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(chunks.peek().is_none() as u8);
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |ty: &[u8; 4], d: &[u8]| {
        out.extend((d.len() as u32).to_be_bytes());
        out.extend(ty);
        out.extend(d);
        let c = crc32(d, crc32(ty, 0));
        out.extend(c.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn get(path: &str, size: i32, flags: SIIGBF) -> Result<bitmap::Bitmap, String> {
    unsafe {
        let w = wide(path);
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).map_err(|e| format!("create {:?}", e.code()))?;
        match f.GetImage(SIZE { cx: size, cy: size }, flags) {
            Ok(h) => {
                let b = bitmap::from_shell_hbitmap(h);
                let _ = DeleteObject(h.into());
                b.ok_or("read".to_string())
            }
            Err(e) => Err(format!("{:?}", e.code())),
        }
    }
}

fn hash(b: &bitmap::Bitmap) -> u64 {
    b.px.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &p| (h ^ p as u64).wrapping_mul(0x100_0000_01b3))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = a.get(1).expect("scratch dir");
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(16);
    unsafe {
        let _ = OleInitialize(None);
    }
    std::fs::create_dir_all(dir).unwrap();
    let size = 256;
    if a.get(3).map(|s| s.as_str()) == Some("make") {
        // fixtures only: n fresh random PNGs, nothing asked of the shell
        let s0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
        for i in 0..n {
            std::fs::write(format!("{dir}/img_{i:02}.png"), png(640, 480, s0 + i as u64)).unwrap();
        }
        return;
    }
    let seed0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
    // (label, flags, generate-in-two-steps)
    let variants: [(&str, SIIGBF); 4] = [
        ("A BIGGERSIZEOK (current generate)", SIIGBF_BIGGERSIZEOK),
        ("B THUMBNAILONLY|BIGGERSIZEOK", SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK),
        ("C INCACHEONLY|THUMBNAILONLY|BIGGERSIZEOK then B", SIIGBF(SIIGBF_INCACHEONLY.0 | SIIGBF_THUMBNAILONLY.0 | SIIGBF_BIGGERSIZEOK.0)),
        ("D THUMBNAILONLY (exact size)", SIIGBF_THUMBNAILONLY),
    ];
    for (vi, (label, flags)) in variants.iter().enumerate() {
        println!("== {label}");
        let (mut real, mut icon, mut err) = (0, 0, 0);
        for i in 0..n {
            let p = format!("{dir}\\v{vi}_{i}.png");
            std::fs::write(&p, png(640, 480, seed0 + (vi * 1000 + i) as u64)).unwrap();
            let icon_ref = get(&p, size, SIIGBF_ICONONLY).ok().map(|b| hash(&b));
            let t = Instant::now();
            let mut r = get(&p, size, *flags);
            let mut note = "";
            if vi == 2 && r.is_err() {
                r = get(&p, size, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK);
                note = " (after cache miss)";
            }
            let ms = t.elapsed().as_secs_f64() * 1e3;
            match r {
                Ok(b) => {
                    let is_icon = Some(hash(&b)) == icon_ref;
                    if is_icon {
                        icon += 1
                    } else {
                        real += 1
                    }
                    println!("  {i:2}: {}x{} {} {ms:.1} ms{note}", b.w, b.h, if is_icon { "ICON" } else { "thumb" });
                }
                Err(e) => {
                    err += 1;
                    println!("  {i:2}: ERR {e} {ms:.1} ms{note}");
                }
            }
        }
        println!("  -> thumb {real}, icon {icon}, err {err}");
    }
    // parallel: 8 threads, current flow
    println!("== E 8 STA threads, probe then generate");
    let (tx, rx) = std::sync::mpsc::channel();
    let mut hs = Vec::new();
    for t in 0..8usize {
        let tx = tx.clone();
        let dir = dir.clone();
        hs.push(std::thread::spawn(move || {
            unsafe {
                let _ = OleInitialize(None);
            }
            for i in (t..n).step_by(8) {
                let p = format!("{dir}\\e_{i}.png");
                std::fs::write(&p, png(640, 480, seed0 + 9000 + i as u64)).unwrap();
                let icon_ref = get(&p, size, SIIGBF_ICONONLY).ok().map(|b| hash(&b));
                let r = match get(&p, size, SIIGBF(SIIGBF_BIGGERSIZEOK.0 | SIIGBF_INCACHEONLY.0 | SIIGBF_THUMBNAILONLY.0)) {
                    Ok(b) => Ok(b),
                    Err(_) => get(&p, size, SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY),
                };
                let s = match r {
                    Ok(b) => format!("{}x{} {}", b.w, b.h, if Some(hash(&b)) == icon_ref { "ICON" } else { "thumb" }),
                    Err(e) => format!("ERR {e}"),
                };
                tx.send((i, s)).unwrap();
            }
        }));
    }
    drop(tx);
    let mut v: Vec<_> = rx.iter().collect();
    v.sort();
    for (i, s) in v {
        println!("  {i:2}: {s}");
    }
    for h in hs {
        let _ = h.join();
    }
}
