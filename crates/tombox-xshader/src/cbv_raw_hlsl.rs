pub const TB_CBRAW_SPACE: u32 = 4_100_000;
const MAX_TITLE_SPACE: u32 = 100_000;

pub fn space_base() -> u32 {
    static B: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *B.get_or_init(|| base_from(std::env::var("TOMBOX_CBRAW_SPACE").ok().as_deref()))
}

fn base_from(v: Option<&str>) -> u32 {
    v.and_then(|s| s.trim().parse::<u32>().ok()).filter(|b| *b >= MAX_TITLE_SPACE && b.checked_add(MAX_TITLE_SPACE).is_some_and(|e| e <= 4_000_000)).unwrap_or(TB_CBRAW_SPACE)
}

pub fn map_space(space: u32) -> Result<u32, String> {
    if space >= MAX_TITLE_SPACE {
        return Err(format!("register space {space} is above the {MAX_TITLE_SPACE} spaces reserved for constant buffers"));
    }
    Ok(space_base() + space)
}

#[cfg(test)]
mod space_tests {
    use super::*;

    #[test]
    fn space_base_knob_accepts_only_free_ranges() {
        assert_eq!(base_from(None), TB_CBRAW_SPACE);
        assert_eq!(base_from(Some("100000")), 100_000);
        assert_eq!(base_from(Some("5")), TB_CBRAW_SPACE);
        assert_eq!(base_from(Some("3950000")), TB_CBRAW_SPACE);
        assert_eq!(base_from(Some("x")), TB_CBRAW_SPACE);
    }
}

fn ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn parse_decl(line: &str) -> Option<(String, u32, u32)> {
    let t = line.trim();
    let rest = t.strip_prefix("cbuffer CB")?;
    let (n, rest) = rest.split_once(" : register(b")?;
    let (reg, rest) = rest.split_once(')')?;
    let (r, space) = match reg.split_once(", space") {
        Some((r, s)) => (r.parse().ok()?, s.parse().ok()?),
        None => (reg.parse().ok()?, 0),
    };
    let inner = rest.trim().strip_prefix('{')?.trim().strip_prefix("uint4 cb")?;
    let (n2, tail) = inner.split_once('[')?;
    let (k, tail) = tail.split_once(']')?;
    if n != n2 || n.parse::<u32>().is_err() || k.parse::<u32>().is_err() || tail.trim() != "; };" {
        return None;
    }
    Some((format!("cb{n}"), r, space))
}

fn close_bracket(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn rewrite_uses(s: &str, names: &[String]) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if ident(b[i]) && (i == 0 || !ident(b[i - 1])) {
            let mut j = i;
            while j < b.len() && ident(b[j]) {
                j += 1;
            }
            let word = &s[i..j];
            if names.iter().any(|n| n == word) {
                if b.get(j) != Some(&b'[') {
                    return Err(format!("constant buffer {word} is used other than by indexing"));
                }
                let end = close_bracket(b, j).ok_or_else(|| format!("unbalanced index after {word}"))?;
                let inner = rewrite_uses(&s[j + 1..end], names)?;
                out.push_str(&format!("tb_cbr{}.Load4(({inner}) * 16u)", &word[2..]));
                i = end + 1;
                continue;
            }
            out.push_str(word);
            i = j;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

pub fn rewrite_hlsl(h: &str) -> Result<String, String> {
    if h.contains("ConstantBuffer<TbBlCb") {
        return Err("bindless constant buffer (ConstantBuffer<TbBlCb..>) in an Xbox One shader: raw-SRV constant binding does not cover it yet".into());
    }
    let mut names = Vec::new();
    let mut body = String::with_capacity(h.len() + 256);
    for line in h.split_inclusive('\n') {
        if let Some((name, r, space)) = parse_decl(line) {
            body.push_str(&format!("ByteAddressBuffer tb_cbr{} : register(t{r}, space{});\n", &name[2..], map_space(space)?));
            names.push(name);
        } else if line.trim_start().starts_with("cbuffer CB") {
            return Err(format!("unrecognised constant buffer declaration: {}", line.trim()));
        } else {
            body.push_str(line);
        }
    }
    if names.is_empty() {
        return Ok(body);
    }
    rewrite_uses(&body, &names)
}
