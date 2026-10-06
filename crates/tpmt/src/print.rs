//! Everything a command prints, and the one question it can ask.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use tpmt_ops::report::{Counts, Report, Severity};
use tpmt_ops::{Built, Change, ChangeKind};

/// Prints where an unpack went.
pub fn unpacked(game_image: &Path, project: &Path) {
    println!(
        "unpacked {} into {}",
        game_image.display(),
        project.display()
    );
}

/// Prints each report to standard error with its severity, colored for
/// terminals. They're about the input, not what the command was asked for.
pub fn reports(reports: &[Report]) {
    let color = io::stderr().is_terminal();
    for report in reports {
        let severity = report.severity.name();
        if color {
            let code = match report.severity {
                Severity::Error => "31",   // red
                Severity::Warning => "33", // yellow
                Severity::Info => "36",    // cyan
            };
            eprintln!("\x1b[{code}m{severity}\x1b[0m: {report}");
        } else {
            eprintln!("{severity}: {report}");
        }
    }
}

/// Prints what a check found, counted by severity.
// May remove later
pub fn checked(reports: &[Report]) {
    let Counts {
        errors,
        warnings,
        infos,
    } = Counts::of(reports);

    if reports.is_empty() {
        println!("nothing to report");
    } else {
        let count = |count: usize, severity: &str| {
            format!("{count} {severity}{}", if count <= 1 { "" } else { "s" })
        };

        println!(
            "{}, {}, {}",
            count(errors, Severity::Error.name()),
            count(warnings, Severity::Warning.name()),
            count(infos, Severity::Info.name())
        );
    }
}

/// Prints a status listing, colored for terminals.
pub fn status(changes: &[Change]) {
    if changes.is_empty() {
        println!("nothing changed from vanilla");
        return;
    }

    let color = io::stdout().is_terminal();
    for change in changes {
        let (tag, code) = match change.kind {
            ChangeKind::Added => ("A", "32"),    // green
            ChangeKind::Replaced => ("R", "33"), // yellow
            ChangeKind::Patched => ("P", "36"),  // cyan
        };
        if color {
            println!("\x1b[{code}m{tag}\x1b[0m {}", change.path);
        } else {
            println!("{tag} {}", change.path);
        }
    }
}

/// Prints what a build wrote.
pub fn built(built: &Built) {
    if built.rebuilt.is_empty() {
        println!("nothing in changes/ to build");
    }
    for path in &built.rebuilt {
        println!("rebuilt {path}");
    }
    println!("wrote {}", built.path.display());
}

/// Asks a yes/no question on standard out, and takes anything but a yes as
/// a no.
pub fn ask(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}
