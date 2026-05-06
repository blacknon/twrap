use crate::model::{OutputTransformKind, OutputTransformRule};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn transform_line(line: &str, rules: &[OutputTransformRule]) -> String {
    let mut current = line.to_string();
    for rule in rules {
        let matches = rule
            .regex
            .find_iter(&current)
            .map(|m| (m.start(), m.end(), m.as_str().to_string()))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }

        let mut chars = current.chars().collect::<Vec<_>>();
        for (start, end, matched) in matches.into_iter().rev() {
            let start_idx = byte_to_char_idx(&current, start);
            let end_idx = byte_to_char_idx(&current, end);
            if start_idx >= end_idx || end_idx > chars.len() {
                continue;
            }
            let replacement = width_preserving_text(&matched, &rule.kind);
            chars.splice(start_idx..end_idx, replacement);
        }
        current = chars.into_iter().collect();
    }
    current
}

fn width_preserving_text(matched: &str, kind: &OutputTransformKind) -> Vec<char> {
    let width = UnicodeWidthStr::width(matched);
    match kind {
        OutputTransformKind::Replace(text) => fit_to_width(text, width),
        OutputTransformKind::Mask(mask_char) => vec![*mask_char; width],
    }
}

fn fit_to_width(text: &str, width: usize) -> Vec<char> {
    let mut chars = Vec::new();
    let mut used_width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used_width + ch_width > width {
            break;
        }
        chars.push(ch);
        used_width += ch_width;
    }
    while used_width < width {
        chars.push(' ');
        used_width += 1;
    }
    chars
}

pub(crate) fn split_cells_by_display_width(
    transformed: &str,
    cell_widths: &[usize],
) -> Vec<String> {
    let mut chars = transformed.chars().peekable();
    let mut cells = Vec::with_capacity(cell_widths.len());

    for &target_width in cell_widths {
        let mut cell = String::new();
        let mut used_width = 0;

        while let Some(&ch) = chars.peek() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch_width == 0 {
                cell.push(ch);
                chars.next();
                continue;
            }
            if used_width + ch_width > target_width {
                if used_width == 0 {
                    chars.next();
                }
                break;
            }
            cell.push(ch);
            chars.next();
            used_width += ch_width;
            if used_width == target_width {
                break;
            }
        }

        while used_width < target_width {
            cell.push(' ');
            used_width += 1;
        }
        cells.push(cell);
    }

    cells
}

fn byte_to_char_idx(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn replace_rule(pattern: &str, replacement: &str) -> OutputTransformRule {
        OutputTransformRule {
            regex: Regex::new(pattern).expect("regex should compile"),
            kind: OutputTransformKind::Replace(replacement.to_string()),
        }
    }

    fn mask_rule(pattern: &str, mask_char: char) -> OutputTransformRule {
        OutputTransformRule {
            regex: Regex::new(pattern).expect("regex should compile"),
            kind: OutputTransformKind::Mask(mask_char),
        }
    }

    #[test]
    fn replace_rule_pads_to_match_width() {
        let out = transform_line("token=abcdef", &[replace_rule("abcdef", "ok")]);
        assert_eq!(out, "token=ok    ");
    }

    #[test]
    fn replace_rule_truncates_to_match_width() {
        let out = transform_line("token=abc", &[replace_rule("abc", "REDACTED")]);
        assert_eq!(out, "token=RED");
    }

    #[test]
    fn mask_rule_preserves_width() {
        let out = transform_line("secret-1234", &[mask_rule(r"1234", '*')]);
        assert_eq!(out, "secret-****");
    }

    #[test]
    fn replace_rule_preserves_display_width_for_wide_text() {
        let out = transform_line("name=山田", &[replace_rule("山田", "AB")]);
        assert_eq!(out, "name=AB  ");
        assert_eq!(
            UnicodeWidthStr::width(out.as_str()),
            UnicodeWidthStr::width("name=山田")
        );
    }

    #[test]
    fn replace_rule_keeps_wide_replacement_when_it_fits_display_width() {
        let out = transform_line("id=ab", &[replace_rule("ab", "界")]);
        assert_eq!(out, "id=界");
        assert_eq!(
            UnicodeWidthStr::width(out.as_str()),
            UnicodeWidthStr::width("id=ab")
        );
    }

    #[test]
    fn split_cells_respects_display_width_boundaries() {
        let cells = split_cells_by_display_width("A界B", &[1, 2, 1]);
        assert_eq!(
            cells,
            vec!["A".to_string(), "界".to_string(), "B".to_string()]
        );
    }
}
