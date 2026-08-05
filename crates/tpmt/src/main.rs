//! Command line front end for the Twilight Princess Modding Toolkit.
//!
//! Reads an invocation and hands it to the pipeline. Nothing here knows what a
//! disc, an archive or a codec is. If a decision needs any of that, it belongs
//! downstream, and if a message about one needs printing, it travels up as an
//! error rather than as a call back into this crate.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    Status,
    /// Restore a file from the disc
    Revert {
        /// File in the project to put back
        path: PathBuf,
    },
    /// Pack the changes into a ready to install mod
    Build,
    /// Pack the changes into a playable disc image
    Image,
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
        Command::Status => Err(Error::Unimplemented("status")),
        Command::Revert { path } => {
            let _ = path;
            Err(Error::Unimplemented("revert"))
        }
        Command::Build => Err(Error::Unimplemented("build")),
        Command::Image => Err(Error::Unimplemented("image")),
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Pipeline(#[from] tpmt_pipeline::Error),

    // PathBuf has no Display, and lossy is the right call in an error message.
    #[error("`{}` has no filename to borrow, so name the project directory yourself", .0.display())]
    NamelessIso(PathBuf),

    #[error("`{0}` is not implemented yet")]
    Unimplemented(&'static str),
}
