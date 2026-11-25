//! A small JSON reader and writer for the tags export (no serde_json in
//! the dependency tree). Objects keep their key order.

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn obj<const N: usize>(kv: [(&str, J); N]) -> J {
        J::Obj(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(kv) => kv.iter().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let J::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_arr(&self) -> Option<&[J]> {
        if let J::Arr(a) = self { Some(a) } else { None }
    }
}

/// Pretty-printed with two-space indents.
pub fn write(v: &J) -> String {
    let mut s = String::new();
    put(&mut s, v, 0);
    s.push('\n');
    s
}

fn put(s: &mut String, v: &J, ind: usize) {
    let pad = |s: &mut String, n: usize| s.extend(std::iter::repeat_n(' ', n * 2));
    match v {
        J::Null => s.push_str("null"),
        J::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
        J::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                s.push_str(&format!("{}", *n as i64))
            } else {
                s.push_str(&format!("{n}"))
            }
        }
        J::Str(t) => put_str(s, t),
        J::Arr(a) if a.is_empty() => s.push_str("[]"),
        J::Arr(a) => {
            // short scalar arrays on one line ("tags": ["Urgent", "Work"])
            if a.iter().all(|x| !matches!(x, J::Arr(_) | J::Obj(_))) {
                s.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    put(s, x, ind);
                }
                s.push(']');
                return;
            }
            s.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                pad(s, ind + 1);
                put(s, x, ind + 1);
                s.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            pad(s, ind);
            s.push(']');
        }
        J::Obj(kv) if kv.is_empty() => s.push_str("{}"),
        J::Obj(kv) => {
            s.push_str("{\n");
            for (i, (k, x)) in kv.iter().enumerate() {
                pad(s, ind + 1);
                put_str(s, k);
                s.push_str(": ");
                put(s, x, ind + 1);
                s.push_str(if i + 1 < kv.len() { ",\n" } else { "\n" });
            }
            pad(s, ind);
            s.push('}');
        }
    }
}

fn put_str(s: &mut String, t: &str) {
    s.push('"');
    for c in t.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

/// Parses one JSON value (whitespace around it allowed). Depth is capped.
pub fn parse(text: &str) -> Result<J, String> {
    let mut p = P { s: text.as_bytes(), at: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    if p.at != p.s.len() {
        return Err(format!("unexpected data at byte {}", p.at));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a [u8],
    at: usize,
    depth: usize,
}

impl P<'_> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.at))
    }
    fn ws(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }
    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.s[self.at..].starts_with(lit) {
            self.at += lit.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Result<J, String> {
        self.ws();
        let Some(&c) = self.s.get(self.at) else { return self.err("unexpected end") };
        match c {
            b'{' | b'[' => {
                self.depth += 1;
                if self.depth > 64 {
                    return self.err("nested too deep");
                }
                let v = if c == b'{' { self.object() } else { self.array() };
                self.depth -= 1;
                v
            }
            b'"' => self.string().map(J::Str),
            b't' if self.eat(b"true") => Ok(J::Bool(true)),
            b'f' if self.eat(b"false") => Ok(J::Bool(false)),
            b'n' if self.eat(b"null") => Ok(J::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("unexpected character"),
        }
    }
    fn object(&mut self) -> Result<J, String> {
        self.at += 1;
        let mut kv = Vec::new();
        self.ws();
        if self.eat(b"}") {
            return Ok(J::Obj(kv));
        }
        loop {
            self.ws();
            if self.s.get(self.at) != Some(&b'"') {
                return self.err("expected a key");
            }
            let k = self.string()?;
            self.ws();
            if !self.eat(b":") {
                return self.err("expected ':'");
            }
            let v = self.value()?;
            kv.push((k, v));
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                return Ok(J::Obj(kv));
            }
            return self.err("expected ',' or '}'");
        }
    }
    fn array(&mut self) -> Result<J, String> {
        self.at += 1;
        let mut a = Vec::new();
        self.ws();
        if self.eat(b"]") {
            return Ok(J::Arr(a));
        }
        loop {
            a.push(self.value()?);
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                return Ok(J::Arr(a));
            }
            return self.err("expected ',' or ']'");
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.at..self.at + 4).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u32::from_str_radix(h, 16).ok());
        match h {
            Some(v) => {
                self.at += 4;
                Ok(v)
            }
            None => self.err("bad \\u escape"),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        let mut run = self.at;
        loop {
            let Some(&c) = self.s.get(self.at) else { return self.err("unterminated string") };
            match c {
                b'"' => {
                    out.push_str(std::str::from_utf8(&self.s[run..self.at]).map_err(|_| "bad utf-8".to_string())?);
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    out.push_str(std::str::from_utf8(&self.s[run..self.at]).map_err(|_| "bad utf-8".to_string())?);
                    self.at += 1;
                    let Some(&e) = self.s.get(self.at) else { return self.err("unterminated string") };
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) && self.eat(b"\\u") {
                                let lo = self.hex4()?;
                                u = 0x10000 + ((u - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3ff);
                            }
                            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        }
                        _ => return self.err("bad escape"),
                    }
                    run = self.at;
                }
                c if c < 0x20 => return self.err("control character in string"),
                _ => self.at += 1,
            }
        }
    }
    fn number(&mut self) -> Result<J, String> {
        let a = self.at;
        while self.at < self.s.len() && matches!(self.s[self.at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.at += 1;
        }
        std::str::from_utf8(&self.s[a..self.at]).ok().and_then(|t| t.parse::<f64>().ok()).map(J::Num).ok_or_else(|| format!("bad number at byte {a}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_errors() {
        let v = J::obj([("a", J::Arr(vec![J::Str("x\"y\\z\n".into()), J::Num(2.0), J::Null, J::Bool(true)])), ("b", J::Obj(vec![])), ("c", J::Str("é😀".into()))]);
        let t = write(&v);
        assert_eq!(parse(&t).unwrap(), v);
        assert_eq!(parse(r#" {"s": "é😀", "n": -1.5e2} "#).unwrap(), J::obj([("s", J::Str("é😀".into())), ("n", J::Num(-150.0))]));
        assert!(parse("[1,]").is_err());
        assert!(parse("{\"a\" 1}").is_err());
        assert!(parse("\"abc").is_err());
        assert!(parse("1 2").is_err());
        assert!(parse(&"[".repeat(100)).is_err());
    }
}
