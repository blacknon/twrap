use clap::Parser;
use regex::Regex;
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Clone, Parser)]
#[command(
    name = "twrap",
    version,
    about = "Wrap an existing TUI and add highlights plus screenshots"
)]
pub(crate) struct Cli {
    #[arg(short = 'e', long = "highlight")]
    pub(crate) highlight: Vec<String>,

    #[arg(short = 'H', long, default_value = "#fff59d")]
    pub(crate) highlight_color: String,

    #[arg(short = 'x', long = "highlight-command")]
    pub(crate) highlight_command: Option<String>,

    #[arg(short = 'S', long, default_value_t = false)]
    pub(crate) highlight_capture_tui_screenshot: bool,

    #[arg(
        short = 'o',
        long = "screenshot-dir",
        default_value = "tmp/twrap-artifacts"
    )]
    pub(crate) screenshot_dir: PathBuf,

    #[arg(short = 'p', long = "screenshot-prefix")]
    pub(crate) screenshot_prefix: Option<String>,

    #[arg(short = 'k', long = "screenshot-key", default_value = "ctrl-g")]
    pub(crate) screenshot_key: ScreenshotKey,

    #[arg(short = 'b', long = "bind")]
    pub(crate) bind: Vec<String>,

    #[arg(
        short = 'R',
        long = "replace",
        value_names = ["PATTERN", "TEXT"],
        num_args = 2
    )]
    pub(crate) replace: Vec<String>,

    #[arg(short = 'M', long = "mask")]
    pub(crate) mask: Vec<String>,

    #[arg(short = 'c', long = "mask-char", default_value = "*")]
    pub(crate) mask_char: String,

    #[arg(short = 'C', long = "startup-capture-ms", default_value_t = 0)]
    pub(crate) startup_capture_ms: u64,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub(crate) command: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenshotKey {
    CtrlG,
    CtrlT,
    CtrlBackslash,
}

impl std::str::FromStr for ScreenshotKey {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ctrl-g" => Ok(Self::CtrlG),
            "ctrl-t" => Ok(Self::CtrlT),
            "ctrl-\\" => Ok(Self::CtrlBackslash),
            _ => Err("supported values: ctrl-g, ctrl-t, ctrl-\\".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

#[derive(Debug, Clone)]
pub(crate) struct HighlightRule {
    pub(crate) key: String,
    pub(crate) pattern: String,
    pub(crate) regex: Regex,
    pub(crate) color: Rgb,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) capture_tui_screenshot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyBinding {
    pub(crate) trigger: KeyChord,
    pub(crate) action: BindingAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyChord {
    pub(crate) label: String,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingAction {
    Send(Vec<u8>),
    Screenshot,
}

#[derive(Debug, Clone)]
pub(crate) struct OutputTransformRule {
    pub(crate) regex: Regex,
    pub(crate) kind: OutputTransformKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputTransformKind {
    Replace(String),
    Mask(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenLine {
    pub(crate) text: String,
    pub(crate) char_spans: Vec<(usize, usize)>,
    pub(crate) cell_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Runtime {
    pub(crate) command: Vec<OsString>,
    pub(crate) startup_capture_ms: u64,
    pub(crate) screenshot_dir: PathBuf,
    pub(crate) screenshot_prefix: Option<String>,
    pub(crate) highlight_rules: Vec<HighlightRule>,
    pub(crate) key_bindings: Vec<KeyBinding>,
    pub(crate) output_transforms: Vec<OutputTransformRule>,
}
