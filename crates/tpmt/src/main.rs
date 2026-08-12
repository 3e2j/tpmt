//! Command line front end for the Twilight Princess Modding Toolkit.
//!
//! Reads an invocation and hands it to the pipeline. Nothing here knows what a
//! disc, an archive or a codec is. If a decision needs any of that, it belongs
//! downstream, and if a message about one needs printing, it travels up as an
//! error rather than as a call back into this crate.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tpmt_pipeline::{Change, ChangeKind, RevertPlan};

// A bad invocation already exits 2 through clap, so this is only for work that
// was asked for correctly and then failed.
const EXIT_FAILURE: u8 = 1;

#[derive(Parser)]
#[command(name = "tpmt", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Unpack a disc into a new project
    New {
        /// Disc image to unpack
        iso: PathBuf,
        /// Directory to create, defaults to the image's name
        dir: Option<PathBuf>,
    },
    /// List files that differ from vanilla
    Status {
        /// Project to check, defaults to the current directory
        dir: Option<PathBuf>,
    },
    /// Restore a file, or every file under a directory, from the disc
    Revert {
        /// File or directory in the project to put back
        path: PathBuf,
        /// Project the path is in, defaults to the current directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<PathBuf>,
        /// Don't ask for confirmation
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// Pack the changes into a ready to install mod
    Build {
        /// Project to pack, defaults to the current directory
        dir: Option<PathBuf>,
        /// Where to write the mod, defaults to out/ inside the project
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Pack the changes into a playable disc image
    Image {
        /// Project to pack, defaults to the current directory
        dir: Option<PathBuf>,
        /// Where to write the image, defaults to out/ inside the project
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tpmt: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::New { iso, dir } => {
            // Defaulting to the image's stem means `tpmt new game.iso` lands in
            // ./game rather than scattering a project over the current folder.
            let project = match dir {
                Some(dir) => dir,
                None => iso
                    .file_stem()
                    .map(PathBuf::from)
                    .ok_or_else(|| Error::NamelessIso(iso.clone()))?,
            };

            tpmt_pipeline::unpack(&iso, &project)?;
            println!("unpacked {} into {}", iso.display(), project.display());
            Ok(())
        }
        Command::Status { dir } => {
            let changes = tpmt_pipeline::status(project(&dir))?;
            print_status(&changes);
            Ok(())
        }
        Command::Revert { path, dir, yes } => revert(project(&dir), &path, yes),
        // Both default to the project around us, the way every other tool that
        // works on a checkout does, and take one somewhere else if named.
        Command::Build { dir, output } => {
            let out = tpmt_pipeline::build(project(&dir), output.as_deref())?;
            println!("built {}", out.display());
            Ok(())
        }
        Command::Image { dir, output } => {
            let out = tpmt_pipeline::image(project(&dir), output.as_deref())?;
            println!("wrote {}", out.display());
            Ok(())
        }
    }
}

/// The project a command works on, which is the current directory unless one
/// was named.
fn project(dir: &Option<PathBuf>) -> &Path {
    match dir {
        Some(dir) => dir,
        None => Path::new("."),
    }
}

/// Prints a status listing, colored red/yellow/green for deleted, modified
/// and added when standard out is a terminal somebody is looking at.
fn print_status(changes: &[Change]) {
    if changes.is_empty() {
        println!("nothing changed from vanilla");
        return;
    }

    let color = io::stdout().is_terminal();
    for change in changes {
        let (tag, code) = match change.kind {
            ChangeKind::Added => ("A", "32"),
            ChangeKind::Modified => ("M", "33"),
            ChangeKind::Deleted => ("D", "31"),
        };
        match color {
            true => println!("\x1b[{code}m{tag}\x1b[0m {}", change.path),
            false => println!("{tag} {}", change.path),
        }
    }
}

/// Reverts `path` in `project`, asking first unless `yes` was given.
fn revert(project: &Path, path: &Path, yes: bool) -> Result<(), Error> {
    let target = path.to_string_lossy().into_owned();
    let plan = tpmt_pipeline::revert_plan(project, &target)?;

    let Some(cascade) = confirm(&plan, yes)? else {
        return Ok(());
    };

    tpmt_pipeline::revert(project, &plan, cascade)?;
    match plan.restore.as_slice() {
        [one] => println!("Reverted {one}"),
        many => println!("Reverted {} files under {target}", many.len()),
    }
    if !plan.skip.is_empty() {
        println!("Left {} untracked file(s) alone", plan.skip.len());
    }
    Ok(())
}

/// Asks before a revert goes ahead, `None` meaning the user said no.
///
/// A single archive member gets one combined prompt covering its own bytes
/// and, if there is one, the sidecar entry restorable alongside it, since
/// the two are one decision to make, not two. A directory or archive gets
/// one prompt for the whole batch, emphasised with a count and, when short
/// enough to read at a glance, the list itself.
fn confirm(plan: &RevertPlan, yes: bool) -> Result<Option<bool>, Error> {
    if yes {
        return Ok(Some(plan.arc_sidecar_entry.is_some()));
    }

    if let [only] = plan.restore.as_slice() {
        let mut prompt = format!("Revert {only} back to vanilla?");
        if let Some(entry) = &plan.arc_sidecar_entry {
            prompt.push_str(&format!(
                "\nthis will also restore its entry in {}, leaving other members untouched.",
                entry.path
            ));
        }
        let cascade = plan.arc_sidecar_entry.is_some();
        return Ok(ask(&format!("{prompt} [y/N] "))?.then_some(cascade));
    }

    println!(
        "This will revert {} files under {}:",
        plan.restore.len(),
        plan.path
    );
    if plan.restore.len() <= 20 {
        for path in &plan.restore {
            println!("  {path}");
        }
    }
    if !plan.skip.is_empty() {
        println!(
            "{} untracked file(s) under this path will be left alone",
            plan.skip.len()
        );
    }
    Ok(ask("proceed? [y/N] ")?.then_some(false))
}

fn ask(prompt: &str) -> Result<bool, Error> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Pipeline(#[from] tpmt_pipeline::Error),

    // PathBuf has no Display, and lossy is the right call in an error message.
    #[error("`{}` has no filename to borrow, so name the project directory yourself", .0.display())]
    NamelessIso(PathBuf),

    #[error("could not read the answer to a prompt: {0}")]
    Io(#[from] io::Error),
}
