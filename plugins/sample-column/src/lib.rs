//! Sample column plugin: for each path, `<EXT uppercased> <length of path>`.
//! Built for wasm32-wasip2 as a component against `tsubame:plugin` (the
//! `column-plugin` world). See `docs/wasm-plugins.md`.

wit_bindgen::generate!({
    path: "../../crates/tsubame-plughost/wit",
    world: "column-plugin",
});

use exports::tsubame::plugin::{column, meta};
use tsubame::plugin::types::Cell;

struct Sample;

fn value(path: &str, out: &mut String) {
    use core::fmt::Write;
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    };
    for c in ext.chars() {
        out.extend(c.to_uppercase());
    }
    let _ = write!(out, " {}", path.len());
}

impl meta::Guest for Sample {
    fn describe() -> meta::Info {
        meta::Info {
            id: "dev.tsubame.sample-column".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            entrypoints: vec![meta::Entrypoint { kind: meta::EntrypointKind::Column, id: "ext-len".into() }],
        }
    }
}

impl column::Guest for Sample {
    fn values(_column: String, paths: Vec<String>) -> Vec<Cell> {
        paths
            .iter()
            .map(|p| {
                let mut s = String::with_capacity(16);
                value(p, &mut s);
                Cell::Text(s)
            })
            .collect()
    }
}

export!(Sample);
