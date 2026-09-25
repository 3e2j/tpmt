//! Drawing the pipeline's [`Progress`] on standard error while a command runs.
//!
//! One line for the current step, redrawn in place ten times a second.
//! Nothing is drawn when standard error is not a terminal, so piped output is
//! unchanged.

use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use tpmt_pipeline::{Progress, Snapshot, Unit};

const FRAME: Duration = Duration::from_millis(100);
const BAR_CELLS: u64 = 20;
const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
/// Back to the start of the line, and clear it.
const CLEAR: &str = "\r\x1b[K";

/// Runs `work` with a fresh [`Progress`], drawing it until `work` returns and
/// then erasing it, so whatever the command prints next starts on a clean
/// line.
pub fn show<T>(work: impl FnOnce(&Progress) -> T) -> T {
    let progress = &Progress::default();
    if !io::stderr().is_terminal() {
        return work(progress);
    }

    let (working, stopped) = mpsc::channel::<()>();
    thread::scope(|scope| {
        scope.spawn(move || draw_until(progress, &stopped));
        // The scope joins the drawer even when `work` panics, so it has to
        // stop on the sender dropping, which unwinding does too.
        let _working = working;
        work(progress)
    })
}

fn draw_until(progress: &Progress, stopped: &Receiver<()>) {
    for spin in SPINNER.into_iter().cycle() {
        if let Some(step) = progress.current() {
            write(&format!("{CLEAR}{}", line(step, spin)));
        }
        if stopped.recv_timeout(FRAME) == Err(RecvTimeoutError::Disconnected) {
            break;
        }
    }
    write(CLEAR);
}

fn line(Snapshot { step, done, total }: Snapshot, spin: char) -> String {
    let label = step.label();
    let done = done.min(total);
    let count = match step.unit() {
        Unit::None => return format!("{label:<16} {spin}"),
        Unit::Bytes => format!("{}/{} MiB", done >> 20, total >> 20),
        Unit::Files => format!("{done}/{total} files"),
    };

    let filled = done * BAR_CELLS / total.max(1);
    let bar: String = (0..BAR_CELLS)
        .map(|cell| if cell < filled { '#' } else { '.' })
        .collect();
    let percent = done * 100 / total.max(1);
    format!("{label:<16} [{bar}] {percent:>3}%  {count}")
}

/// A frame the terminal won't take is not worth failing the command over.
fn write(frame: &str) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(frame.as_bytes());
    let _ = stderr.flush();
}
