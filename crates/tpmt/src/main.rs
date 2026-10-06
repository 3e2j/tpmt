//! CLI frontend for Twilight Princess Modding Toolkit.
//!
//! Reads an invocation and hands it to `tpmt-ops` to run.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, Subcommand};
use tpmt_ops::report::Counts;
use tpmt_ops::{Project, Target};

mod print;
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
        game_image: PathBuf,
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
    /// Report errors and warnings in the changes, without building
    Check {
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
        Ok(Outcome::Done) => ExitCode::SUCCESS,
        Ok(Outcome::Failed) => ExitCode::from(EXIT_FAILURE),
        Err(error) => {
            if let Error::Ops(error) = &error {
                print::reports(&error.reports());
            }
            eprintln!("{error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// How a command that ran to the end went.
enum Outcome {
    Done,
    /// It ran, and what it found is the failure, like a check with errors.
    Failed,
}

/// Matches the command given and runs it on the project.
fn run(command: Command) -> Result<Outcome, Error> {
    match command {
        Command::New {
            game_image,
            dir,
            yes,
        } => {
            // Defaulting to the image's stem means `tpmt new game.iso` lands in
            // ./game rather than scattering a project over the current folder.
            let project = match dir {
                Some(dir) => dir,
                None => game_image
                    .file_stem()
                    .map(PathBuf::from)
                    .ok_or_else(|| Error::NamelessGameImage(game_image.clone()))?,
            };

            if tpmt_ops::is_project(&project) {
                let overwrite = yes
                    || print::ask(&format!(
                        "`{}` is already a project. Overwrite it? [y/N] ",
                        project.display()
                    ))?;
                if !overwrite {
                    return Ok(Outcome::Done);
                }
            }

            progress::show(|progress| tpmt_ops::unpack(&game_image, &project, progress))?;
            print::unpacked(&game_image, &project);
        }
        Command::Status { dir } => {
            let changes = tpmt_ops::status(&project(dir.as_ref())?)?;
            print::status(&changes);
        }
        Command::Check { dir } => {
            let reports = tpmt_ops::check(&project(dir.as_ref())?)?;
            print::reports(&reports);
            print::checked(&reports);
            if Counts::of(&reports).errors > 0 {
                return Ok(Outcome::Failed);
            }
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
            print::reports(&built.reports);
            print::built(&built);
        }
    }
    Ok(Outcome::Done)
}

/// The project a command works on, discovered from `dir`, or the current
/// directory if none was named.
fn project(dir: Option<&PathBuf>) -> Result<Project, Error> {
    let start = dir.map_or_else(|| Path::new("."), PathBuf::as_path);
    Ok(tpmt_ops::discover(start)?)
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Ops(#[from] tpmt_ops::Error),

    // PathBuf has no Display, and lossy is the right call in an error message.
    #[error("`{}` has no filename to borrow, so name the project directory yourself", .0.display())]
    NamelessGameImage(PathBuf),

    #[error("could not read the answer to a prompt: {0}")]
    Io(#[from] io::Error),
}
