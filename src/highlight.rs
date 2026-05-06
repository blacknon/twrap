use crate::{
    model::{HighlightRule, Rgb, ScreenLine},
    template::expand_template,
};
use anyhow::Result;
use serde::Serialize;
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub(crate) struct HighlightEvaluation {
    pub(crate) colors: Vec<Vec<Option<Rgb>>>,
    pub(crate) triggers: Vec<HighlightTrigger>,
}

#[derive(Debug, Clone)]
pub(crate) struct HighlightTrigger {
    pub(crate) key: String,
    pub(crate) pattern: String,
    pub(crate) matched_texts: Vec<String>,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) capture_tui_screenshot: bool,
    pub(crate) fingerprint: u64,
}

#[derive(Debug, Default)]
pub(crate) struct TriggerState {
    seen: HashMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct TriggerManifest<'a> {
    backend: &'a str,
    key: &'a str,
    pattern: &'a str,
    matches: &'a [String],
    capture_path: Option<String>,
}

pub(crate) fn evaluate_lines(lines: &[ScreenLine], rules: &[HighlightRule]) -> HighlightEvaluation {
    let mut colors = lines
        .iter()
        .map(|line| vec![None; line.cell_count])
        .collect::<Vec<_>>();
    let mut triggers = Vec::new();

    for rule in rules {
        let mut matched_texts = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            for found in rule.regex.find_iter(&line.text) {
                matched_texts.push(found.as_str().to_string());
                let start = byte_to_char_idx(&line.text, found.start());
                let end = byte_to_char_idx(&line.text, found.end());
                let Some((start_col, end_col)) = char_range_to_cell_range(line, start, end) else {
                    continue;
                };
                for col in start_col..end_col.min(colors[row].len()) {
                    colors[row][col] = Some(rule.color);
                }
            }
        }

        if !matched_texts.is_empty() {
            triggers.push(HighlightTrigger {
                key: rule.key.clone(),
                pattern: rule.pattern.clone(),
                matched_texts: matched_texts.clone(),
                command: rule.command.clone(),
                capture_tui_screenshot: rule.capture_tui_screenshot,
                fingerprint: fingerprint(&rule.key, &matched_texts),
            });
        }
    }

    HighlightEvaluation { colors, triggers }
}

pub(crate) fn filter_new_triggers(
    state: &mut TriggerState,
    triggers: Vec<HighlightTrigger>,
) -> Vec<HighlightTrigger> {
    let mut fresh = Vec::new();
    for trigger in triggers {
        let changed = state
            .seen
            .get(&trigger.key)
            .is_none_or(|prev| *prev != trigger.fingerprint);
        if changed {
            state.seen.insert(trigger.key.clone(), trigger.fingerprint);
            fresh.push(trigger);
        }
    }
    fresh
}

pub(crate) fn dispatch_tui_triggers(
    output_dir: &Path,
    output_prefix: Option<&str>,
    triggers: &[HighlightTrigger],
    screenshot_svg: &str,
) -> Result<Vec<PathBuf>> {
    let mut capture_paths = Vec::new();
    for trigger in triggers {
        let capture_path = if trigger.capture_tui_screenshot {
            Some(write_capture_file(
                output_dir,
                output_prefix,
                &trigger.key,
                "tui",
                "highlight_svg",
                "svg",
                screenshot_svg.as_bytes(),
            )?)
        } else {
            None
        };
        let manifest_path = write_manifest(
            output_dir,
            output_prefix,
            "tui",
            trigger,
            capture_path.as_deref(),
        )?;
        run_trigger_command(trigger, "tui", &manifest_path, capture_path.as_deref())?;
        if let Some(path) = capture_path {
            capture_paths.push(path);
        }
    }
    Ok(capture_paths)
}

pub(crate) fn write_manual_screenshot(
    output_dir: &Path,
    output_prefix: Option<&str>,
    svg: &str,
) -> Result<PathBuf> {
    write_capture_file(
        output_dir,
        output_prefix,
        "manual",
        "tui",
        "manual_svg",
        "svg",
        svg.as_bytes(),
    )
}

fn run_trigger_command(
    trigger: &HighlightTrigger,
    backend: &str,
    manifest_path: &Path,
    capture_path: Option<&Path>,
) -> Result<()> {
    let Some(command) = trigger.command.as_ref() else {
        return Ok(());
    };
    if command.is_empty() {
        return Ok(());
    }

    let matches_json = serde_json::to_string(&trigger.matched_texts)?;
    let capture_path_string = capture_path.map(|path| path.display().to_string());
    let expanded_command = command
        .iter()
        .map(|part| {
            expand_command_part(
                part,
                backend,
                &trigger.key,
                &trigger.pattern,
                &matches_json,
                &manifest_path.display().to_string(),
                capture_path_string.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let mut cmd = std::process::Command::new(&expanded_command[0]);
    cmd.args(&expanded_command[1..])
        .env("TWRAP_HIGHLIGHT_BACKEND", backend)
        .env("TWRAP_HIGHLIGHT_KEY", &trigger.key)
        .env("TWRAP_HIGHLIGHT_PATTERN", &trigger.pattern)
        .env("TWRAP_HIGHLIGHT_MATCHES_JSON", &matches_json)
        .env("TWRAP_HIGHLIGHT_EVENT_JSON", manifest_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    if let Some(path) = capture_path {
        cmd.env("TWRAP_HIGHLIGHT_CAPTURE_PATH", path);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    let _child = cmd.spawn()?;
    Ok(())
}

fn expand_command_part(
    template: &str,
    backend: &str,
    key: &str,
    pattern: &str,
    matches_json: &str,
    event_json: &str,
    capture_path: Option<&str>,
) -> String {
    expand_template(template, |token| match token {
        "backend" => Some(backend.to_string()),
        "key" => Some(key.to_string()),
        "pattern" => Some(pattern.to_string()),
        "matches_json" => Some(matches_json.to_string()),
        "event_json" => Some(event_json.to_string()),
        "capture_path" => capture_path.map(str::to_string),
        _ => None,
    })
}

fn write_capture_file(
    dir: &Path,
    output_prefix: Option<&str>,
    key: &str,
    backend: &str,
    capture_kind: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let timestamp = timestamp_suffix();
    let prefix = render_output_prefix(output_prefix, key, backend, capture_kind, ext, timestamp);
    let filename = format!("{prefix}{}-{timestamp}.{ext}", sanitize_segment(key));
    let path = dir.join(filename);
    fs::write(&path, bytes)?;
    Ok(path)
}

fn write_manifest(
    dir: &Path,
    output_prefix: Option<&str>,
    backend: &str,
    trigger: &HighlightTrigger,
    capture_path: Option<&Path>,
) -> Result<PathBuf> {
    let timestamp = timestamp_suffix();
    let prefix = render_output_prefix(
        output_prefix,
        &trigger.key,
        backend,
        "event",
        "json",
        timestamp,
    );
    let filename = format!(
        "{prefix}{}-{timestamp}-event.json",
        sanitize_segment(&trigger.key)
    );
    let manifest = TriggerManifest {
        backend,
        key: &trigger.key,
        pattern: &trigger.pattern,
        matches: &trigger.matched_texts,
        capture_path: capture_path.map(|path| path.display().to_string()),
    };
    let path = dir.join(filename);
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(path)
}

pub(crate) fn render_output_prefix(
    template: Option<&str>,
    key: &str,
    backend: &str,
    capture_kind: &str,
    ext: &str,
    timestamp: u128,
) -> String {
    let Some(template) = template else {
        return String::new();
    };

    let rendered = expand_template(template, |token| match token {
        "key" => Some(sanitize_segment(key)),
        "backend" => Some(backend.to_string()),
        "capture_kind" => Some(capture_kind.to_string()),
        "ext" => Some(ext.to_string()),
        "timestamp" => Some(timestamp.to_string()),
        _ if token.starts_with("env:") => std::env::var(&token[4..])
            .ok()
            .map(|value| sanitize_segment(&value)),
        _ => None,
    });
    sanitize_segment(&rendered)
}

fn sanitize_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn byte_to_char_idx(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

fn char_range_to_cell_range(
    line: &ScreenLine,
    start_char: usize,
    end_char: usize,
) -> Option<(usize, usize)> {
    if start_char >= end_char {
        return None;
    }
    let start = line.char_spans.get(start_char)?.0;
    let end = line.char_spans.get(end_char.saturating_sub(1))?.1;
    Some((start, end))
}

fn fingerprint(key: &str, matches: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    for matched in matches {
        matched.hash(&mut hasher);
    }
    hasher.finish()
}

fn timestamp_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HighlightRule, Rgb};
    use regex::Regex;

    #[test]
    fn evaluate_lines_marks_matching_ranges() {
        let rules = vec![HighlightRule {
            key: "error".to_string(),
            pattern: "error".to_string(),
            regex: Regex::new("error").expect("regex should compile"),
            color: Rgb(1, 2, 3),
            command: None,
            capture_tui_screenshot: false,
        }];

        let evaluation = evaluate_lines(
            &[ScreenLine {
                text: "an error happened".to_string(),
                char_spans: (0..17).map(|idx| (idx, idx + 1)).collect(),
                cell_count: 17,
            }],
            &rules,
        );
        assert_eq!(evaluation.triggers.len(), 1);
        assert_eq!(evaluation.colors[0][3], Some(Rgb(1, 2, 3)));
    }

    #[test]
    fn evaluate_lines_marks_wide_char_matches_on_the_right_cells() {
        let rules = vec![HighlightRule {
            key: "warn".to_string(),
            pattern: "B".to_string(),
            regex: Regex::new("B").expect("regex should compile"),
            color: Rgb(9, 9, 9),
            command: None,
            capture_tui_screenshot: false,
        }];

        let evaluation = evaluate_lines(
            &[ScreenLine {
                text: "界B".to_string(),
                char_spans: vec![(0, 2), (2, 3)],
                cell_count: 3,
            }],
            &rules,
        );

        assert_eq!(evaluation.colors[0], vec![None, None, Some(Rgb(9, 9, 9))]);
    }

    #[test]
    fn render_output_prefix_replaces_tokens() {
        let rendered = render_output_prefix(
            Some("shot-{backend}-{capture_kind}-{key}-{timestamp}-"),
            "warn",
            "tui",
            "manual_svg",
            "svg",
            42,
        );

        assert_eq!(rendered, "shot-tui-manual_svg-warn-42-");
    }
}
