//! Building a project's changes into what a target asked for.
//!
//! The caller lists every project file that differs from vanilla, and hands
//! over a way to read any file, edited copy first. An edited file sits at the
//! same project path its vanilla copy would, so a rebuild takes the edited
//! copy where there is one and the vanilla copy everywhere else.
//!
//! An unchanged file is never rebuilt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tpmt_binary::Compression;
use tpmt_disc::{Boot, Metadata};
use tpmt_report::{Progress, Step};

use crate::{Error, fs};

mod implode;
mod targets;
mod tree;

pub use implode::EncodeError;
pub use targets::Target;

use tree::Tree;

/// Every project file a build reads, by project path.
pub trait Files<E>: Sync {
    /// One file's bytes, the edited copy where there is one.
    ///
    /// # Errors
    ///
    /// Whatever the caller fails with, like a file in neither copy.
    fn read(&self, path: &str) -> Result<Box<[u8]>, E>;

    /// Whether either copy holds a file at `path`.
    fn is_file(&self, path: &str) -> bool;
}

/// The disc a project was unpacked from, which an image is laid out from.
pub struct Source<'a> {
    /// Where it was last seen.
    pub game_image: &'a Path,
    /// The game id and revision it held at the unpack.
    pub id: &'a str,
    pub revision: u8,
}

impl Source<'_> {
    /// Whether `boot` is the version this project was unpacked from.
    #[must_use]
    pub fn matches(&self, boot: &Boot) -> bool {
        self.id == boot.id && self.revision == boot.revision
    }
}

/// Everything a build is handed.
pub struct Job<'a, E> {
    pub files: &'a dyn Files<E>,
    /// Every project file that differs from vanilla, sorted.
    pub changes: &'a [String],
    /// The preamble values a build cannot derive.
    pub metadata: &'a Metadata,
    /// The disc files that arrived compressed, so a rebuild puts the same
    /// wrapper back on each.
    pub compressed: &'a BTreeMap<String, Compression>,
    pub source: Source<'a>,
    /// What to call a build that is one file, like an image.
    pub name: &'a str,
    /// Where each step reports how far it has got.
    pub progress: &'a Progress,
}

/// What a build produced.
#[derive(Debug)]
pub struct Built {
    /// What to point somebody at, relative to the output: empty for a patch,
    /// the file for an image.
    pub path: PathBuf,
    /// The disc files that were written again, at their disc paths.
    pub rebuilt: Vec<String>,
}

/// A job, with the disc files its changes touch worked out.
struct Context<'a, E> {
    job: &'a Job<'a, E>,
    tree: Tree<'a, E>,
    /// The disc files the changes touch, each to be rebuilt.
    rebuilt: BTreeSet<String>,
}

pub fn run<E>(target: Target, job: &Job<'_, E>, out: &Path) -> Result<Built, E>
where
    E: From<Error> + Send,
{
    let tree = Tree::new(job.files, job.changes);
    let rebuilt = tree.rebuilt();
    let context = Context { job, tree, rebuilt };

    let path = match target {
        Target::Patch => targets::patch::write(&context, out)?,
        Target::Image => targets::image::write(&context, out)?,
        Target::Dusk => targets::dusk::write(&context, out)?,
    };

    Ok(Built {
        path,
        rebuilt: context.rebuilt.into_iter().collect(),
    })
}

/// Assembles every disc file the changes touch, writing each into `into` at
/// its own project path.
///
/// One disc file has nothing to do with the next, so they go in parallel, the
/// same way an unpack takes them apart.
fn rebuild<E>(context: &Context<'_, E>, into: &Path) -> Result<(), E>
where
    E: From<Error> + Send,
{
    let job = context.job;
    let rebuilding = job
        .progress
        .begin(Step::Rebuild, context.rebuilt.len() as u64);
    context
        .rebuilt
        .par_iter()
        .map(|path| {
            let compression = job.compressed.get(path).copied();
            let bytes = implode::disc_file(&context.tree, path, compression)?;
            fs::write(&into.join(path), &bytes)?;
            rebuilding.add(1);
            Ok(())
        })
        .collect()
}
