use std::fs;
use std::path::{Path, PathBuf};

fn comment_lines(src: &str) -> Vec<usize> {
    let b = src.as_bytes();
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let (mut i, mut line, mut found) = (0, 1, Vec::new());
    while i < b.len() {
        let c = b[i];
        if c == b'/' && at(i + 1) == b'/' {
            found.push(line);
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && at(i + 1) == b'*' {
            found.push(line);
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && at(i + 1) == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && at(i + 1) == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(b[i] == b'\n');
                    i += 1;
                }
            }
            continue;
        }
        if matches!(c, b'r' | b'b' | b'c') && (i == 0 || !is_ident(b[i - 1])) {
            let mut j = i + 1;
            if c != b'r' && at(j) == b'r' {
                j += 1;
            }
            if b[j - 1] == b'r' {
                let mut k = j;
                while at(k) == b'#' {
                    k += 1;
                }
                if at(k) == b'"' {
                    let close = format!("\"{}", "#".repeat(k - j));
                    let end = src[k + 1..].find(&close).map_or(b.len(), |e| k + 1 + e + close.len());
                    line += b[i..end].iter().filter(|&&x| x == b'\n').count();
                    i = end;
                    continue;
                }
            }
        }
        if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                let step = if b[i] == b'\\' { 2 } else { 1 };
                line += b[i..(i + step).min(b.len())].iter().filter(|&&x| x == b'\n').count();
                i += step;
            }
            i += 1;
            continue;
        }
        if c == b'\'' {
            if at(i + 1) == b'\\' {
                i += 1;
                while i < b.len() && b[i] != b'\'' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            if let Some(ch) = src[i + 1..].chars().next() {
                if at(i + 1 + ch.len_utf8()) == b'\'' {
                    i += 2 + ch.len_utf8();
                    continue;
                }
            }
        }
        line += usize::from(c == b'\n');
        i += 1;
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() && path.file_name().is_some_and(|n| n != "target") {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "the workspace still has comments; un-ignore once they are gone"]
fn the_workspace_has_no_comments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["crates", "apps"] {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(!files.is_empty());
    let offenders: Vec<String> = files
        .iter()
        .flat_map(|f| {
            let src = fs::read_to_string(f).unwrap();
            let shown = f.strip_prefix(&root).unwrap_or(f).display().to_string();
            comment_lines(&src).into_iter().map(move |l| format!("{shown}:{l}"))
        })
        .collect();
    assert!(offenders.is_empty(), "comments are not allowed:\n{}", offenders.join("\n"));
}

#[test]
fn comment_lines_finds_comments_outside_literals() {
    let src = concat!(
        "let url = \"http://x\";\n",
        "let raw = r#\"// not \"one\" /* either */\"#;\n",
        "let c = '\\''; let d = '/'; fn f<'a>() {}\n",
        "let x = 1; // trailing\n",
        "/* block\n",
        "   /* nested */ still */ let y = 2;\n",
        "/// doc\n",
    );
    assert_eq!(comment_lines(src), vec![4, 5, 7]);
}
