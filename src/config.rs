use crate::model::{
    BindingAction, Cli, HighlightRule, KeyBinding, KeyChord, OutputTransformKind,
    OutputTransformRule, Rgb, Runtime, ScreenshotKey,
};
use anyhow::{Context, Result};
use regex::Regex;
use std::{fs, path::Path};

pub(crate) fn build_runtime(cli: Cli) -> Result<Runtime> {
    let highlight_color = parse_rgb(&cli.highlight_color).context("invalid --highlight-color")?;
    let highlight_rules = cli
        .highlight
        .iter()
        .map(|pattern| {
            Ok(HighlightRule {
                key: pattern.clone(),
                pattern: pattern.clone(),
                regex: Regex::new(pattern)
                    .with_context(|| format!("invalid highlight regex: {pattern}"))?,
                color: highlight_color,
                command: parse_highlight_command(cli.highlight_command.as_deref())
                    .context("invalid --highlight-command")?,
                capture_tui_screenshot: cli.highlight_capture_tui_screenshot,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if !cli.screenshot_dir.exists() {
        fs::create_dir_all(&cli.screenshot_dir).with_context(|| {
            format!(
                "failed to create screenshot dir: {}",
                cli.screenshot_dir.display()
            )
        })?;
    } else if !Path::new(&cli.screenshot_dir).is_dir() {
        anyhow::bail!(
            "--screenshot-dir is not a directory: {}",
            cli.screenshot_dir.display()
        );
    }

    let key_bindings =
        compile_key_bindings(&cli.bind, cli.screenshot_key).context("failed to compile --bind")?;
    let output_transforms = compile_output_transforms(&cli.replace, &cli.mask, &cli.mask_char)
        .context("failed to compile replace/mask rules")?;

    Ok(Runtime {
        command: cli.command,
        startup_capture_ms: cli.startup_capture_ms,
        screenshot_dir: cli.screenshot_dir,
        screenshot_prefix: cli.screenshot_prefix,
        highlight_rules,
        key_bindings,
        output_transforms,
    })
}

fn parse_highlight_command(value: Option<&str>) -> Result<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parts = shell_words::split(value)?;
    if parts.is_empty() {
        anyhow::bail!("command must not be empty");
    }
    Ok(Some(parts))
}

fn parse_rgb(value: &str) -> Result<Rgb> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        anyhow::bail!("expected 6-digit hex color");
    }

    let r = u8::from_str_radix(&hex[0..2], 16)?;
    let g = u8::from_str_radix(&hex[2..4], 16)?;
    let b = u8::from_str_radix(&hex[4..6], 16)?;
    Ok(Rgb(r, g, b))
}

fn compile_key_bindings(
    bind_specs: &[String],
    screenshot_key: ScreenshotKey,
) -> Result<Vec<KeyBinding>> {
    let mut bindings = Vec::new();
    for spec in bind_specs {
        let (left, right) = spec
            .split_once('=')
            .with_context(|| format!("binding must be FROM=TO: {spec}"))?;
        let trigger = parse_key_chord(left.trim())?;
        let action = parse_binding_action(right.trim())?;
        bindings.push(KeyBinding { trigger, action });
    }

    let default_trigger = screenshot_key_to_chord(screenshot_key);
    let has_override = bindings
        .iter()
        .any(|binding| binding.trigger.bytes == default_trigger.bytes);
    if !has_override {
        bindings.push(KeyBinding {
            trigger: default_trigger,
            action: BindingAction::Screenshot,
        });
    }

    Ok(bindings)
}

fn compile_output_transforms(
    replace_specs: &[String],
    mask_specs: &[String],
    mask_char: &str,
) -> Result<Vec<OutputTransformRule>> {
    let mut rules = Vec::new();
    let mask_char = parse_mask_char(mask_char)?;

    if !replace_specs.len().is_multiple_of(2) {
        anyhow::bail!("--replace expects PATTERN TEXT pairs");
    }

    for pair in replace_specs.chunks_exact(2) {
        let pattern = pair[0].clone();
        let replacement = pair[1].clone();
        rules.push(OutputTransformRule {
            regex: Regex::new(&pattern)
                .with_context(|| format!("invalid replace regex: {pattern}"))?,
            kind: OutputTransformKind::Replace(replacement),
        });
    }

    for pattern in mask_specs {
        rules.push(OutputTransformRule {
            regex: Regex::new(pattern).with_context(|| format!("invalid mask regex: {pattern}"))?,
            kind: OutputTransformKind::Mask(mask_char),
        });
    }

    Ok(rules)
}

fn parse_binding_action(value: &str) -> Result<BindingAction> {
    if value.eq_ignore_ascii_case("screenshot") {
        return Ok(BindingAction::Screenshot);
    }

    if let Some(text) = value.strip_prefix("text:") {
        return Ok(BindingAction::Send(text.as_bytes().to_vec()));
    }

    let key_list = value.strip_prefix("send:").unwrap_or(value);
    let mut bytes = Vec::new();
    for item in key_list.split(',') {
        let chord = parse_key_chord(item.trim())?;
        bytes.extend_from_slice(&chord.bytes);
    }
    Ok(BindingAction::Send(bytes))
}

fn screenshot_key_to_chord(key: ScreenshotKey) -> KeyChord {
    match key {
        ScreenshotKey::CtrlG => KeyChord {
            label: "ctrl-g".to_string(),
            bytes: vec![0x07],
        },
        ScreenshotKey::CtrlT => KeyChord {
            label: "ctrl-t".to_string(),
            bytes: vec![0x14],
        },
        ScreenshotKey::CtrlBackslash => KeyChord {
            label: "ctrl-\\".to_string(),
            bytes: vec![0x1c],
        },
    }
}

fn parse_key_chord(value: &str) -> Result<KeyChord> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        anyhow::bail!("key binding token must not be empty");
    }

    let bytes = match normalized.as_str() {
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "esc" => vec![0x1b],
        "space" => vec![b' '],
        "backspace" => vec![0x7f],
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        token if token.starts_with("ctrl-") => parse_ctrl_key(token)?,
        token => parse_literal_key(token)?,
    };

    Ok(KeyChord {
        label: normalized,
        bytes,
    })
}

fn parse_ctrl_key(value: &str) -> Result<Vec<u8>> {
    let suffix = &value[5..];
    if suffix.len() != 1 {
        anyhow::bail!("ctrl binding must have one character: {value}");
    }
    let ch = suffix.as_bytes()[0];
    if !ch.is_ascii_alphabetic() && ch != b'\\' && ch != b'[' && ch != b']' {
        anyhow::bail!("unsupported ctrl binding: {value}");
    }
    Ok(match ch {
        b'\\' => vec![0x1c],
        b'[' => vec![0x1b],
        b']' => vec![0x1d],
        _ => vec![ch.to_ascii_uppercase() - b'@'],
    })
}

fn parse_literal_key(value: &str) -> Result<Vec<u8>> {
    if value.chars().count() != 1 {
        anyhow::bail!("unsupported key binding token: {value}");
    }
    Ok(value.as_bytes().to_vec())
}

fn parse_mask_char(value: &str) -> Result<char> {
    let mut chars = value.chars();
    let ch = chars
        .next()
        .with_context(|| "mask char must not be empty".to_string())?;
    if chars.next().is_some() {
        anyhow::bail!("mask char must be a single character");
    }
    Ok(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rgb_accepts_hash_prefix() {
        let rgb = parse_rgb("#12abef").expect("color should parse");
        assert_eq!(rgb, Rgb(0x12, 0xab, 0xef));
    }

    #[test]
    fn compile_key_bindings_adds_default_screenshot_binding() {
        let bindings =
            compile_key_bindings(&[], ScreenshotKey::CtrlG).expect("bindings should compile");

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].action, BindingAction::Screenshot);
        assert_eq!(bindings[0].trigger.bytes, vec![0x07]);
    }

    #[test]
    fn compile_key_bindings_parses_send_and_screenshot_actions() {
        let bindings = compile_key_bindings(
            &[
                "j=down".to_string(),
                "ctrl-t=screenshot".to_string(),
                "g=text:gg".to_string(),
            ],
            ScreenshotKey::CtrlG,
        )
        .expect("bindings should compile");

        assert!(
            bindings
                .iter()
                .any(|binding| binding.action == BindingAction::Screenshot)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.action == BindingAction::Send(b"\x1b[B".to_vec()))
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.action == BindingAction::Send(b"gg".to_vec()))
        );
    }

    #[test]
    fn compile_output_transforms_parses_replace_and_mask() {
        let rules = compile_output_transforms(
            &["secret".to_string(), "xxxxx".to_string()],
            &["token=[^ ]+".to_string()],
            "#",
        )
        .expect("rules should compile");

        assert_eq!(rules.len(), 2);
        assert_eq!(
            rules[0].kind,
            OutputTransformKind::Replace("xxxxx".to_string())
        );
        assert_eq!(rules[1].kind, OutputTransformKind::Mask('#'));
    }
}
