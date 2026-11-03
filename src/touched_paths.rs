//! Ordered touched paths from Git patch file sections.
//! Handles Git byte quoting and hunk boundaries without external dependencies.

use std::path::PathBuf;

pub(crate) fn parse(text: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut headers = Vec::new();
    let mut fallback = Vec::new();
    let mut hunk = None;
    let mut opaque_hunk = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&mut files, &mut headers, &mut fallback);
            fallback = diff_paths(rest);
            hunk = None;
            opaque_hunk = false;
        } else if opaque_hunk {
            continue;
        } else if line.starts_with("@@") {
            hunk = hunk_counts(line);
            opaque_hunk = hunk.is_none();
        } else if let Some((old, new)) = hunk.as_mut() {
            match line.as_bytes().first() {
                Some(b'-') => *old = old.saturating_sub(1),
                Some(b'+') => *new = new.saturating_sub(1),
                Some(b' ') => {
                    *old = old.saturating_sub(1);
                    *new = new.saturating_sub(1);
                }
                _ => {}
            }
            if *old == 0 && *new == 0 {
                hunk = None;
            }
        } else if let Some((prefix, rest)) = [
            "--- ",
            "+++ ",
            "rename from ",
            "rename to ",
            "copy from ",
            "copy to ",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(*prefix).map(|rest| (*prefix, rest)))
        {
            let path = decode_path(rest.split('\t').next().unwrap_or(rest));
            // Only ---/+++ carry a side prefix; rename and copy lines hold the real path.
            let is_side = prefix == "--- " || prefix == "+++ ";
            headers.push(if is_side { strip_side(&path).to_owned() } else { path });
        }
    }
    flush(&mut files, &mut headers, &mut fallback);
    files
}

fn strip_side(path: &str) -> &str {
    path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path)
}

fn flush(files: &mut Vec<PathBuf>, headers: &mut Vec<String>, fallback: &mut Vec<String>) {
    let from_fallback = headers.is_empty();
    let paths = if from_fallback {
        &mut *fallback
    } else {
        &mut *headers
    };
    for path in paths.drain(..).filter(|path| path != "/dev/null") {
        let path = PathBuf::from(if from_fallback { strip_side(&path) } else { &path });
        if !files.contains(&path) {
            files.push(path);
        }
    }
    headers.clear();
    fallback.clear();
}

fn hunk_counts(line: &str) -> Option<(usize, usize)> {
    let mut fields = line.strip_prefix("@@ ")?.split_whitespace();
    let count = |field: &str, prefix| -> Option<usize> {
        let range = field.strip_prefix(prefix)?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        start.parse::<usize>().ok()?;
        count.parse().ok()
    };
    Some((count(fields.next()?, '-')?, count(fields.next()?, '+')?))
}

fn diff_paths(rest: &str) -> Vec<String> {
    if rest.starts_with('"') {
        if let Some((first, remaining)) = quoted(rest) {
            return vec![first, decode_path(remaining.trim_start())];
        }
    }
    for (index, _) in rest.match_indices(" b/") {
        if rest[..index].strip_prefix("a/") == Some(&rest[index + 3..]) {
            return vec![rest[..index].into(), rest[index + 1..].into()];
        }
    }
    // Git may quote only the destination when its name requires escaping.
    for (index, _) in rest.match_indices(" \"") {
        if let Some((second, "")) = quoted(&rest[index + 1..]) {
            return vec![rest[..index].into(), second];
        }
    }
    Vec::new()
}

fn decode_path(value: &str) -> String {
    quoted(value).map_or_else(|| value.to_owned(), |(path, _)| path)
}

fn quoted(value: &str) -> Option<(String, &str)> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut decoded = Vec::new();
    let mut index = 1;
    while let Some(&byte) = bytes.get(index) {
        index += 1;
        match byte {
            b'"' => {
                return Some((
                    String::from_utf8_lossy(&decoded).into_owned(),
                    &value[index..],
                ))
            }
            b'\\' => {
                let escape = *bytes.get(index)?;
                index += 1;
                decoded.push(unescape(escape, bytes, &mut index));
            }
            other => decoded.push(other),
        }
    }
    None
}

fn unescape(escape: u8, bytes: &[u8], index: &mut usize) -> u8 {
    match escape {
        b'0'..=b'3'
            if bytes
                .get(*index..*index + 2)
                .is_some_and(|pair| pair.iter().all(|b| (b'0'..=b'7').contains(b))) =>
        {
            let byte =
                (escape - b'0') * 64 + (bytes[*index] - b'0') * 8 + (bytes[*index + 1] - b'0');
            *index += 2;
            byte
        }
        b't' => b'\t',
        b'n' => b'\n',
        b'r' => b'\r',
        b'a' => 7,
        b'b' => 8,
        b'f' => 12,
        b'v' => 11,
        other => other,
    }
}

#[cfg(test)]
#[path = "touched_paths_tests.rs"]
mod tests;
