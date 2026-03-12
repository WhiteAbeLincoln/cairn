/// A single line in a grep result group.
#[derive(Debug, Clone, PartialEq)]
pub struct GrepLine {
    pub line_num: Option<u32>,
    pub content: String,
    pub is_match: bool,
}

/// A group of grep result lines sharing the same file path.
#[derive(Debug, Clone, PartialEq)]
pub struct GrepGroup {
    pub file: String,
    pub lines: Vec<GrepLine>,
}

/// Parse ripgrep output into structured groups.
///
/// Match lines use `:` separators:   filepath:linenum:content
/// Context lines use `-` separators: filepath-linenum-content
/// Section breaks are `--` on their own line.
/// Sometimes filenames are omitted:  linenum:content / linenum-context
pub fn parse_grep_output(output: &str) -> Vec<GrepGroup> {
    let mut groups: Vec<GrepGroup> = Vec::new();
    let mut current: Option<GrepGroup> = None;

    for line in output.lines() {
        if line == "--" {
            if let Some(g) = current.take()
                && !g.lines.is_empty()
            {
                groups.push(g);
            }
            continue;
        }

        if line.is_empty() {
            continue;
        }

        let Some(parsed) = parse_grep_line(line) else {
            continue;
        };

        let file = parsed.file.unwrap_or_default();

        // Start a new group when file changes
        let need_new = match &current {
            None => true,
            Some(g) => !same_file(&g.file, &file),
        };

        if need_new {
            if let Some(g) = current.take()
                && !g.lines.is_empty()
            {
                groups.push(g);
            }
            current = Some(GrepGroup {
                file: file.clone(),
                lines: Vec::new(),
            });
        } else if let Some(g) = &mut current {
            // Prefer longer (absolute) path
            if file.len() > g.file.len() {
                g.file.clone_from(&file);
            }
        }

        if let Some(g) = &mut current {
            g.lines.push(parsed.line);
        }
    }

    if let Some(g) = current
        && !g.lines.is_empty()
    {
        groups.push(g);
    }

    groups
}

struct ParsedLine {
    file: Option<String>,
    line: GrepLine,
}

fn same_file(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // One must be a path-suffix of the other (match at `/` boundary)
    let (longer, shorter) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if longer.ends_with(shorter) {
        let split_pos = longer.len() - shorter.len();
        split_pos > 0 && longer.as_bytes()[split_pos - 1] == b'/'
    } else {
        false
    }
}

fn try_nofile_match(line: &str) -> Option<ParsedLine> {
    let colon = line.find(':')?;
    let num_str = &line[..colon];
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line_num: u32 = num_str.parse().ok()?;
    if line_num == 0 {
        return None;
    }
    Some(ParsedLine {
        file: None,
        line: GrepLine {
            line_num: Some(line_num),
            content: line[colon + 1..].to_string(),
            is_match: true,
        },
    })
}

fn try_nofile_context(line: &str) -> Option<ParsedLine> {
    let dash = line.find('-')?;
    let num_str = &line[..dash];
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line_num: u32 = num_str.parse().ok()?;
    if line_num == 0 {
        return None;
    }
    Some(ParsedLine {
        file: None,
        line: GrepLine {
            line_num: Some(line_num),
            content: line[dash + 1..].to_string(),
            is_match: false,
        },
    })
}

/// Find `sep + digits + sep` in the line, returning (path_end, line_num, content_start).
fn find_separator(line: &str, sep: u8) -> Option<(usize, u32, usize)> {
    let bytes = line.as_bytes();
    let mut i = 1usize;
    while i < bytes.len() {
        let sep_idx = bytes[i..].iter().position(|&b| b == sep).map(|p| i + p)?;
        let mut j = sep_idx + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > sep_idx + 1
            && j < bytes.len()
            && bytes[j] == sep
            && let Ok(line_num) = line[sep_idx + 1..j].parse::<u32>()
            && line_num > 0
        {
            return Some((sep_idx, line_num, j + 1));
        }
        i = sep_idx + 1;
    }
    None
}

fn parse_grep_line(line: &str) -> Option<ParsedLine> {
    // Try no-file patterns first (unambiguous when line starts with digits)
    if let Some(p) = try_nofile_match(line) {
        return Some(p);
    }
    if let Some(p) = try_nofile_context(line) {
        return Some(p);
    }

    // File match line: scan for `:digits:`
    if let Some((path_end, line_num, content_start)) = find_separator(line, b':') {
        return Some(ParsedLine {
            file: Some(line[..path_end].to_string()),
            line: GrepLine {
                line_num: Some(line_num),
                content: line[content_start..].to_string(),
                is_match: true,
            },
        });
    }

    // File context line: scan for `-digits-`
    if let Some((path_end, line_num, content_start)) = find_separator(line, b'-') {
        return Some(ParsedLine {
            file: Some(line[..path_end].to_string()),
            line: GrepLine {
                line_num: Some(line_num),
                content: line[content_start..].to_string(),
                is_match: false,
            },
        });
    }

    // Unparseable — treat as match with no metadata
    Some(ParsedLine {
        file: None,
        line: GrepLine {
            line_num: None,
            content: line.to_string(),
            is_match: true,
        },
    })
}

/// Strip ANSI escape codes from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    // CSI sequence: consume until ASCII alphabetic terminator
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    // OSC sequence: consume until BEL (\x07) or ST (\x1b\)
                    chars.next();
                    loop {
                        match chars.next() {
                            None => break,
                            Some('\x07') => break,
                            Some('\x1b') => {
                                // ST is ESC followed by backslash
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
                _ => {
                    // Bare ESC or unrecognized sequence: consume the ESC only
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Strip the `    N\t` line number prefixes added by the Read tool.
pub fn strip_read_line_numbers(s: &str) -> String {
    s.lines()
        .map(|line| {
            if let Some(idx) = line.find('\t') {
                let prefix = &line[..idx];
                if prefix.trim().chars().all(|c| c.is_ascii_digit()) {
                    return &line[idx + 1..];
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_grep_output() {
        let output = "src/main.rs:10:fn main() {\nsrc/main.rs:11:    println!(\"hello\");\nsrc/lib.rs:5:pub fn foo() {";
        let groups = parse_grep_output(output);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].file, "src/main.rs");
        assert_eq!(groups[0].lines.len(), 2);
        assert_eq!(groups[1].file, "src/lib.rs");
        assert_eq!(groups[1].lines.len(), 1);
    }

    #[test]
    fn test_parse_grep_output_section_break() {
        let output = "src/a.rs:1:foo\n--\nsrc/b.rs:1:bar";
        let groups = parse_grep_output(output);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].file, "src/a.rs");
        assert_eq!(groups[1].file, "src/b.rs");
    }

    #[test]
    fn test_parse_grep_output_context_lines() {
        let output = "src/a.rs-1-context\nsrc/a.rs:2:match";
        let groups = parse_grep_output(output);
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].lines[0].is_match);
        assert!(groups[0].lines[1].is_match);
    }

    #[test]
    fn test_strip_read_line_numbers() {
        let input = "     1\tline one\n     2\tline two\n    10\tline ten";
        let expected = "line one\nline two\nline ten";
        assert_eq!(strip_read_line_numbers(input), expected);
    }

    #[test]
    fn test_strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("no ansi here"), "no ansi here");
        assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[0m"), "bold green");
    }

    #[test]
    fn test_strip_ansi_empty() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_lone_esc() {
        assert_eq!(strip_ansi("\x1b"), "");
    }

    #[test]
    fn test_strip_ansi_osc_bel() {
        assert_eq!(strip_ansi("\x1b]0;title\x07rest"), "rest");
    }

    #[test]
    fn test_strip_ansi_osc_st() {
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\rest"), "rest");
    }

    #[test]
    fn test_strip_ansi_mixed() {
        assert_eq!(
            strip_ansi("before\x1b[31merror\x1b[0mmiddle\x1b]0;title\x07after"),
            "beforeerrormiddleafter"
        );
    }
}
