//! CLI frontend for the Twilight Princess Modding Toolkit.
//!
//! Reads an invocation and hands it to `tpmt-ops` to run.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, Subcommand};
use tpmt_ops::{Built, Change, ChangeKind, Project, Target};

mod progress;

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
        /// Don't ask before overwriting an existing project
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// List files that differ from vanilla
    Status {
        /// Project to check, defaults to the current directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<PathBuf>,
    },
    /// Pack the changes for one target
    Build {
        /// What to build
        #[arg(value_parser = target_parser())]
        target: Target,
        /// Project to pack, defaults to the current directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<PathBuf>,
        /// An empty or new directory to write it to, defaults to
        /// build/targets/<target>/ in the project
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Offers exactly the build targets, so a new one needs nothing here.
fn target_parser() -> impl TypedValueParser<Value = Target> {
    PossibleValuesParser::new(Target::ALL.map(Target::name))
        .try_map(|name| Target::from_name(&name).ok_or("not a build target"))
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

/// Matches the command given and runs it on the project.
fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::New { iso, dir, yes } => {
            // Defaulting to the image's stem means `tpmt new game.iso` lands in
            // ./game rather than scattering a project over the current folder.
            let project = match dir {
                Some(dir) => dir,
                None => iso
                    .file_stem()
                    .map(PathBuf::from)
                    .ok_or_else(|| Error::NamelessIso(iso.clone()))?,
            };

            if tpmt_ops::is_project(&project) {
                let overwrite = yes
                    || ask(&format!(
                        "`{}` is already a project. Overwrite it? [y/N] ",
                        project.display()
                    ))?;
                if !overwrite {
                    return Ok(());
                }
            }

            progress::show(|progress| tpmt_ops::unpack(&iso, &project, progress))?;
            println!("unpacked {} into {}", iso.display(), project.display());
        }
        Command::Status { dir } => {
            let changes = tpmt_ops::status(&project(dir.as_ref())?)?;
            print_status(&changes);
        }
        Command::Build {
            target,
            dir,
            output,
        } => {
            let project = project(dir.as_ref())?;
            let built = progress::show(|progress| {
                tpmt_ops::build(&project, target, output.as_deref(), progress)
            })?;
            print_built(&built);
        }
    }
    Ok(())
}

/// The project a command works on, discovered from `dir`, or the current
/// directory if none was named.
fn project(dir: Option<&PathBuf>) -> Result<Project, Error> {
    let start = dir.map_or_else(|| Path::new("."), PathBuf::as_path);
    Ok(tpmt_ops::discover(start)?)
}

/// Prints a status listing, colored for terminals.
fn print_status(changes: &[Change]) {
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
fn print_built(built: &Built) {
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
    Ops(#[from] tpmt_ops::Error),

    // PathBuf has no Display, and lossy is the right call in an error message.
    #[error("`{}` has no filename to borrow, so name the project directory yourself", .0.display())]
    NamelessIso(PathBuf),

    #[error("could not read the answer to a prompt: {0}")]
    Io(#[from] io::Error),
}
