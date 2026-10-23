/// Normalize an asset path into the canonical lookup key: forward slashes,
/// no leading `/` or `./`, no empty or `.` segments, lowercase.
pub fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for seg in path.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(seg);
    }
    out.make_ascii_lowercase();
    out
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("/Materials//Foo.JSON"), "materials/foo.json");
        assert_eq!(normalize("./a\\b/./c"), "a/b/c");
        assert_eq!(normalize(""), "");
    }
}
