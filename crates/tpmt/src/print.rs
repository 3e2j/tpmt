//! Everything a command prints, and the one question it can ask.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use tpmt_ops::{Built, Change, ChangeKind};

/// Prints where an unpack went.
pub fn unpacked(iso: &Path, project: &Path) {
    println!("unpacked {} into {}", iso.display(), project.display());
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
///
/// A file in `changes/` that matches vanilla goes to standard error rather
/// than standard out: it is not what was asked for, and somebody who put it
/// there meant to change something.
pub fn built(built: &Built) {
    for path in &built.unchanged {
        eprintln!("tpmt: `{path}` is identical to vanilla, so it changes nothing");
    }

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
