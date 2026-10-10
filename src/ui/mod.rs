//! Show progress and rich text while preserving a predictable machine output mode

use std::cell::Cell;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::Result;

mod report;

pub use report::write_report;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Dawn,
    Moss,
    Tide,
    Dusk,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    #[default]
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportState {
    Active,
    Complete,
    Stopped,
}

#[derive(Debug)]
pub struct Ui {
    theme: Theme,
    mode: OutputMode,
    styled: bool,
    current: usize,
    total: usize,
    determinate: bool,
    motion: bool,
    line_open: Cell<bool>,
    terminal: Arc<Mutex<()>>,
    animation: Mutex<Option<Animation>>,
}

#[derive(Debug)]
struct Animation {
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<()>,
}

impl Ui {
    #[must_use]
    pub fn new(theme: Theme, mode: OutputMode, total: usize) -> Self {
        let styled = styled_terminal(
            mode,
            theme,
            io::stderr().is_terminal(),
            std::env::var_os("NO_COLOR").is_some(),
        );
        let motion = terminal_motion(
            styled,
            std::env::var_os("DRUKAL_REDUCED_MOTION").is_some(),
            std::env::var_os("CI").is_some(),
        );
        Self {
            theme,
            mode,
            styled,
            current: 0,
            total: total.max(1),
            determinate: true,
            motion,
            line_open: Cell::new(false),
            terminal: Arc::new(Mutex::new(())),
            animation: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn indeterminate(theme: Theme, mode: OutputMode) -> Self {
        let mut ui = Self::new(theme, mode, 1);
        ui.determinate = false;
        ui
    }

    pub fn title(&self, title: &str, subtitle: &str) {
        if self.mode == OutputMode::Json {
            return;
        }
        self.finish_progress();
        let title = terminal_text(title);
        let subtitle = terminal_text(subtitle);
        let _terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.styled {
            eprintln!(
                "\n  \x1b[{}m(o>\x1b[0m  \x1b[1mDrukal\x1b[0m\n  \x1b[{}m/ )\x1b[0m  \x1b[1m{title}\x1b[0m\n  \x1b[{}m^^\x1b[0m   {subtitle}\n",
                self.accent(),
                self.accent(),
                self.accent()
            );
        } else {
            eprintln!("Drukal\n{title}\n{subtitle}\n");
        }
    }

    pub fn stage(&mut self, label: &str) {
        self.current = if self.determinate {
            (self.current + 1).min(self.total)
        } else {
            self.current.saturating_add(1)
        };
        if self.mode == OutputMode::Json {
            return;
        }
        let label = terminal_text(label);
        if self.styled {
            self.finish_progress();
            {
                let _terminal = self
                    .terminal
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                self.write_stage(&mut io::stderr().lock(), &label);
                if self.motion {
                    write_activity(&mut io::stderr().lock(), self.accent(), 0);
                    self.line_open.set(true);
                }
            }
            if self.motion && !self.start_animation() {
                self.finish_progress();
            }
        } else if !self.determinate {
            eprintln!("→ {label}");
        } else {
            eprintln!("[step {}/{}] {label}", self.current, self.total);
        }
    }

    pub fn success(&self, message: &str) {
        self.message("✓", "1;32", message);
    }

    pub fn note(&self, message: &str) {
        self.message("·", "0", message);
    }

    pub fn warning(&self, message: &str) {
        self.message("!", "1;33", message);
    }

    fn message(&self, symbol: &str, style: &str, message: &str) {
        if self.mode == OutputMode::Json {
            return;
        }
        self.finish_progress();
        let message = terminal_text(message);
        let _terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.styled {
            eprintln!("  \x1b[{style}m{symbol}\x1b[0m  {message}");
        } else {
            eprintln!("{symbol} {message}");
        }
    }

    #[must_use]
    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    #[must_use]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    fn accent(&self) -> &'static str {
        terminal_accent(self.theme)
    }

    fn write_stage(&self, output: &mut impl Write, label: &str) {
        let accent = self.accent();
        if self.determinate {
            let _ = writeln!(
                output,
                "  \x1b[{accent}m{}/{}\x1b[0m  {label}",
                self.current, self.total
            );
        } else {
            let _ = writeln!(output, "  \x1b[{accent}m•\x1b[0m  {label}");
        }
    }

    pub(crate) fn finish_progress(&self) {
        self.stop_animation();
        if self.styled && self.line_open.replace(false) {
            let _terminal = self
                .terminal
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut output = io::stderr().lock();
            let _ = write!(output, "\r\x1b[2K");
            let _ = output.flush();
        }
    }

    fn stop_animation(&self) {
        let animation = self
            .animation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(animation) = animation {
            let _ = animation.stop.send(());
            let _ = animation.worker.join();
        }
    }

    fn start_animation(&self) -> bool {
        let (stop, stopped) = mpsc::channel();
        let terminal = Arc::clone(&self.terminal);
        let accent = self.accent();
        let Ok(worker) = thread::Builder::new()
            .name("drukal-progress".into())
            .spawn(move || {
                let mut frame = 0;
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    stopped.recv_timeout(Duration::from_millis(240))
                {
                    frame = (frame + 1) % 4;
                    let _terminal = terminal.lock().unwrap_or_else(|error| error.into_inner());
                    write_activity(&mut io::stderr().lock(), accent, frame);
                }
            })
        else {
            return false;
        };
        *self
            .animation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Animation { stop, worker });
        true
    }
}

fn write_activity(output: &mut impl Write, accent: &str, frame: usize) {
    const FRAMES: [&str; 4] = ["●··", "·●·", "··●", "·●·"];
    let dots = FRAMES[frame % FRAMES.len()];
    let _ = write!(output, "\r\x1b[2K  \x1b[{accent}m{dots}\x1b[0m  Working");
    let _ = output.flush();
}

fn styled_terminal(mode: OutputMode, theme: Theme, is_terminal: bool, no_color: bool) -> bool {
    mode == OutputMode::Human && theme != Theme::Plain && is_terminal && !no_color
}

fn terminal_motion(styled: bool, reduced_motion: bool, ci: bool) -> bool {
    styled && !reduced_motion && !ci
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.finish_progress();
    }
}

pub fn print_markdown(markdown: &str, theme: Theme) -> Result<()> {
    let styled = theme != Theme::Plain
        && io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none();
    let mut output = io::stdout().lock();
    let accent = terminal_accent(theme);
    for line in markdown.lines() {
        let line = terminal_text(line);
        if !styled {
            writeln!(output, "{line}")?;
            continue;
        }
        let rendered = if let Some(heading) = line.strip_prefix("### ") {
            format!("\x1b[1;4;{accent}m{heading}\x1b[0m")
        } else if let Some(heading) = line.strip_prefix("## ") {
            format!("\x1b[1;{accent}m{heading}\x1b[0m")
        } else if let Some(heading) = line.strip_prefix("# ") {
            format!("\x1b[1;{accent}m{heading}\x1b[0m")
        } else {
            terminal_emphasis(&line)
        };
        writeln!(output, "{rendered}")?;
    }
    Ok(())
}

pub fn json_success_document(kind: &str, result: &serde_json::Value) -> Result<String> {
    #[derive(Serialize)]
    struct Document<'a> {
        schema: u8,
        status: &'static str,
        kind: &'a str,
        result: &'a serde_json::Value,
    }

    Ok(serde_json::to_string_pretty(&Document {
        schema: 1,
        status: "ok",
        kind,
        result,
    })?)
}

fn terminal_accent(theme: Theme) -> &'static str {
    match theme {
        Theme::Auto | Theme::Dawn => "33",
        Theme::Moss | Theme::Plain => "32",
        Theme::Tide => "36",
        Theme::Dusk => "35",
    }
}

fn terminal_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn terminal_emphasis(line: &str) -> String {
    let mut result = paired_markers(line, "<u>", "</u>", "\x1b[4m", "\x1b[24m");
    result = paired_markers(&result, "**", "**", "\x1b[1m", "\x1b[22m");
    paired_single_asterisks(&result)
}

fn paired_markers(
    value: &str,
    start: &str,
    end: &str,
    open_style: &str,
    close_style: &str,
) -> String {
    let mut result = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start_index) = remaining.find(start) {
        result.push_str(&remaining[..start_index]);
        let content = &remaining[start_index + start.len()..];
        let Some(end_index) = content.find(end) else {
            result.push_str(&remaining[start_index..]);
            return result;
        };
        result.push_str(open_style);
        result.push_str(&content[..end_index]);
        result.push_str(close_style);
        remaining = &content[end_index + end.len()..];
    }
    result.push_str(remaining);
    result
}

fn paired_single_asterisks(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start_index) = single_asterisk(remaining) {
        result.push_str(&remaining[..start_index]);
        let content = &remaining[start_index + 1..];
        let Some(end_index) = single_asterisk(content) else {
            result.push_str(&remaining[start_index..]);
            return result;
        };
        result.push_str("\x1b[3m");
        result.push_str(&content[..end_index]);
        result.push_str("\x1b[23m");
        remaining = &content[end_index + 1..];
    }
    result.push_str(remaining);
    result
}

fn single_asterisk(value: &str) -> Option<usize> {
    value.match_indices('*').find_map(|(index, _)| {
        let bytes = value.as_bytes();
        (index.checked_sub(1).and_then(|before| bytes.get(before)) != Some(&b'*')
            && bytes.get(index + 1) != Some(&b'*'))
        .then_some(index)
    })
}

#[cfg(test)]
mod tests;
