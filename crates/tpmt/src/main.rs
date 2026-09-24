//! CLI frontend for the Twilight Princess Modding Toolkit.
//!
//! Reads an invocation and hands it to the pipeline to deal with.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, Subcommand};
use tpmt_pipeline::{Built, Change, ChangeKind, Target};

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

/// Offers exactly the pipeline's targets, so a new one needs nothing here.
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

/// Matches the command given and runs the pipeline.
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

            if project.exists() && tpmt_pipeline::is_project(&project) {
                let overwrite = yes
                    || ask(&format!(
                        "`{}` is already a project. Overwrite it? [y/N] ",
                        project.display()
                    ))?;
                if !overwrite {
                    return Ok(());
                }
            }

            tpmt_pipeline::unpack(&iso, &project)?;
            println!("unpacked {} into {}", iso.display(), project.display());
            Ok(())
        }
        Command::Status { dir } => {
            let root = project(dir.as_ref())?;
            let changes = tpmt_pipeline::status(&root)?;
            print_status(&changes);
            Ok(())
        }
        Command::Revert { path, dir, yes } => {
            let root = project(dir.as_ref())?;
            // `git -C` semantics: a relative path resolves against where you're
            // standing, not against the project root `-C`/`dir` points at.
            let cwd = std::env::current_dir()?;
            let absolute = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            revert(&root, &absolute, yes)
        }
        Command::Build {
            target,
            dir,
            output,
        } => {
            let root = project(dir.as_ref())?;
            let built = tpmt_pipeline::build(&root, target, output.as_deref())?;
            print_built(&built);
            Ok(())
        }
    }
}

/// The project a command works on: discovered by walking up from `dir` (or
/// the current directory, if none was named), the way `git -C` starts its own
/// search from wherever it is pointed rather than treating that spot as the
/// root itself.
fn project(dir: Option<&PathBuf>) -> Result<PathBuf, Error> {
    let start = dir.map_or_else(|| Path::new("."), PathBuf::as_path);
    Ok(tpmt_pipeline::discover(start)?)
}

/// Prints a status listing, colored yellow/green for modified and added when
/// standard out is a terminal somebody is looking at.
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
/// An overlay file that matches vanilla goes to standard error rather than
/// standard out: it is not what was asked for, and somebody who put it there
/// meant to change something.
fn print_built(built: &Built) {
    for path in &built.unchanged {
        eprintln!("tpmt: `{path}` is identical to vanilla, so it changes nothing");
    }

    if built.rebuilt.is_empty() {
        println!("nothing in the overlay to build");
    }
    for path in &built.rebuilt {
        println!("rebuilt {path}");
    }
    println!("wrote {}", built.path.display());
}

/// Reverts `target`, an absolute filesystem path, asking first unless `yes`
/// was given.
fn revert(project: &Path, target: &Path, yes: bool) -> Result<(), Error> {
    if !yes
        && !ask(&format!(
            "Revert {} back to vanilla? [y/N] ",
            target.display()
        ))?
    {
        return Ok(());
    }

    tpmt_pipeline::revert(project, target)?;
    println!("Reverted {}", target.display());
    Ok(())
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
