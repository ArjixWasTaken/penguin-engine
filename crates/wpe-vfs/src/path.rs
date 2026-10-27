/// Normalize an asset path into the canonical lookup key: forward slashes,
/// no leading `/` or `./`, no empty or `.` segments, resolved `..` segments, lowercase.
pub fn normalize(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            segments.pop();
        } else {
            segments.push(seg);
        }
    }
    let mut out = segments.join("/");
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
        assert_eq!(normalize("a/b/../c"), "a/c");
        assert_eq!(normalize("a/b/../../c"), "c");
        assert_eq!(normalize("../../etc/passwd"), "etc/passwd");
        assert_eq!(normalize("a/.."), "");
    }
}
