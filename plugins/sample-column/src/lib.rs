//! Sample column plugin: for each path, `<EXT uppercased> <length of path>`.
//! Built for wasm32-wasip2 as a component.

wit_bindgen::generate!({
    path: "../../crates/tsubame-plughost/wit",
    world: "column",
});

struct Sample;

fn value(path: &str, out: &mut String) {
    use core::fmt::Write;
    let name = path.rsplit([b'\\' as char, '/']).next().unwrap_or(path);
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    };
    for c in ext.chars() {
        out.extend(c.to_uppercase());
    }
    let _ = write!(out, " {}", path.len());
}

impl Guest for Sample {
    fn column_values(paths: Vec<String>) -> Vec<String> {
        paths
            .iter()
            .map(|p| {
                let mut s = String::with_capacity(16);
                value(p, &mut s);
                s
            })
            .collect()
    }

    fn column_values_packed(buf: Vec<u8>) -> Vec<u8> {
        let rd = |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
        let n = rd(0);
        let data = &buf[4 + (n + 1) * 4..];
        let mut vals = String::with_capacity(n * 12);
        let mut offs: Vec<u32> = Vec::with_capacity(n + 1);
        offs.push(0);
        for i in 0..n {
            let a = rd(4 + i * 4);
            let b = rd(4 + (i + 1) * 4);
            let p = core::str::from_utf8(&data[a..b]).unwrap_or("");
            value(p, &mut vals);
            offs.push(vals.len() as u32);
        }
        let mut out = Vec::with_capacity(4 + offs.len() * 4 + vals.len());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        for o in &offs {
            out.extend_from_slice(&o.to_le_bytes());
        }
        out.extend_from_slice(vals.as_bytes());
        out
    }
}

export!(Sample);
