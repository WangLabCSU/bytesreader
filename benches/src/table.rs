#[cfg(unix)]
use std::fs::File;
use std::io::{self, IsTerminal};
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub struct Row<const N: usize> {
    pub cells: [String; N],
    pub fastest: bool,
    pub subgroup: Option<usize>,
}

fn terminal_width() -> usize {
    static WIDTH: OnceLock<usize> = OnceLock::new();
    *WIDTH.get_or_init(|| {
        // COLUMNS also lets redirected output use a chosen width.
        if let Some(width) = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|&width| width > 0)
        {
            return width;
        }
        #[cfg(unix)]
        if io::stdout().is_terminal()
            && let Ok(tty) = File::open("/dev/tty")
            && let Ok(output) = Command::new("stty")
                .arg("size")
                .stdin(tty)
                .stderr(Stdio::null())
                .output()
            && output.status.success()
            && let Some(width) = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|&width| width > 0)
        {
            return width;
        }
        80
    })
}

// All benchmark labels and numbers are ASCII. Prefer whitespace and Rust
// punctuation as line breaks; never shorten a label or discard its contents.
fn wrap(value: &str, width: usize) -> Vec<&str> {
    debug_assert!(value.is_ascii());
    let width = width.max(1);
    let mut lines = Vec::new();
    for mut rest in value.split('\n') {
        while rest.len() > width {
            let prefix = &rest[..width];
            let end = prefix
                .rfind(' ')
                .filter(|&at| at > 0)
                .or_else(|| prefix.rfind([',', '(', ')', ':', '+']).map(|at| at + 1))
                .unwrap_or(width);
            lines.push(rest[..end].trim_end());
            rest = rest[end..].trim_start();
        }
        lines.push(rest);
    }
    lines
}

pub fn text(value: &str) {
    for line in wrap(value, terminal_width()) {
        println!("{line}");
    }
}

// Formatting happens after timing. Keep numeric columns intact and give the
// widest text column the remaining terminal width. ANSI escapes stay outside
// padded cells so color cannot change alignment.
pub fn print<const N: usize>(
    title: &str,
    headers: [&str; N],
    rows: &[Row<N>],
    numeric_start: usize,
    group_columns: usize,
) {
    let color = io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    if color {
        print!("\n\x1b[1;36m");
        text(title);
        println!("\x1b[0m");
    } else {
        println!();
        text(title);
        println!();
    }
    let mut widths: [usize; N] = std::array::from_fn(|column| {
        rows.iter()
            .flat_map(|row| row.cells[column].lines().map(str::len))
            .chain([headers[column].len(), 3])
            .max()
            .unwrap()
    });
    let flexible = (0..numeric_start)
        .max_by_key(|&column| widths[column])
        .unwrap();
    let fixed = widths.iter().sum::<usize>() - widths[flexible] + 3 * N + 1;
    let remaining = terminal_width().saturating_sub(fixed);
    // At extremely narrow widths, stack fields instead of splitting numbers.
    if remaining < headers[flexible].len() {
        for row in rows {
            for (header, cell) in headers.iter().zip(&row.cells) {
                text(&format!("{header}: {cell}"));
            }
            println!();
        }
        return;
    }
    widths[flexible] = remaining;
    let print_border = |left: char, middle: char, right: char, horizontal: char| {
        print!("{left}");
        for (column, width) in widths.iter().enumerate() {
            print!("{}", horizontal.to_string().repeat(width + 2));
            print!("{}", if column + 1 == N { right } else { middle });
        }
        println!();
    };
    let print_row = |cells: [&str; N], header: bool, fastest: bool| {
        let lines: [Vec<&str>; N] =
            std::array::from_fn(|column| wrap(cells[column], widths[column]));
        let height = lines.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            print!("│");
            for (column, cell) in lines.iter().enumerate() {
                let cell = cell.get(line).copied().unwrap_or("");
                let width = widths[column];
                let style = if color && header {
                    "\x1b[1m"
                } else if color && fastest && column == numeric_start {
                    "\x1b[32m"
                } else {
                    ""
                };
                let reset = if style.is_empty() { "" } else { "\x1b[0m" };
                if column >= numeric_start {
                    print!(" {style}{cell:>width$}{reset} │");
                } else {
                    print!(" {style}{cell:<width$}{reset} │");
                }
            }
            println!();
        }
    };
    print_border('┌', '┬', '┐', '─');
    print_row(headers, true, false);
    print_border('├', '┼', '┤', '─');
    for (index, row) in rows.iter().enumerate() {
        if index > 0 && row.cells[..group_columns] != rows[index - 1].cells[..group_columns] {
            print_border('├', '┼', '┤', '─');
        } else if index > 0 && row.subgroup != rows[index - 1].subgroup {
            print_border('├', '┼', '┤', '╌');
        }
        print_row(
            std::array::from_fn(|column| row.cells[column].as_str()),
            false,
            row.fastest,
        );
    }
    print_border('└', '┴', '┘', '─');
}
