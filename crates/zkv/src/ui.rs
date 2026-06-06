//! Terminal output helpers: balance formatting, ANSI styling, status lines,
//! and an animated spinner for long-running operations.
//!
//! All status output goes to **stderr** (stdout stays a clean machine-readable
//! channel; see the crate conventions). Colour is applied only when stderr is
//! an interactive terminal and the environment hasn't opted out, so piped or
//! redirected output stays free of escape sequences.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use zcash_protocol::value::ZatBalance;

use crate::data::Network;

const COIN: u64 = 1_0000_0000;

pub fn format_zec(value: impl TryInto<ZatBalance>, network: Network) -> String {
    let value = i64::from(
        value
            .try_into()
            .map_err(|_| ())
            .expect("Values are formattable"),
    );
    let abs_value = value.unsigned_abs();
    let abs_zec = abs_value / COIN;
    let frac = abs_value % COIN;
    let zec = if value.is_negative() {
        -(abs_zec as i64)
    } else {
        abs_zec as i64
    };
    format!("{zec:3}.{frac:08} {}", network.ticker())
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// The pure colour-policy decision, factored out of [`want_color`] so it can be
/// unit-tested without touching process env: honours the
/// [`NO_COLOR`](https://no-color.org/) convention and `TERM=dumb`, allows an
/// explicit `CLICOLOR_FORCE`, and otherwise requires the stream to be a TTY.
fn decide_color(
    no_color: bool,
    clicolor_force: Option<&str>,
    is_tty: bool,
    term: Option<&str>,
) -> bool {
    // An explicit opt-out always wins.
    if no_color {
        return false;
    }
    // An explicit opt-in (any non-"0" value) forces colour even off a TTY.
    if let Some(force) = clicolor_force {
        if force != "0" {
            return true;
        }
    }
    if !is_tty {
        return false;
    }
    term != Some("dumb")
}

/// Which standard stream a colour decision targets. On Windows this selects
/// the console handle whose Virtual Terminal mode we enable.
#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// Core policy shared by both streams, reading the relevant environment, then
/// (for a TTY-driven "yes") confirming the stream can actually render ANSI.
fn want_color(stream: Stream, is_tty: bool) -> bool {
    let force = std::env::var("CLICOLOR_FORCE").ok();
    let decided = decide_color(
        std::env::var_os("NO_COLOR").is_some(),
        force.as_deref(),
        is_tty,
        std::env::var("TERM").ok().as_deref(),
    );
    if !decided {
        return false;
    }
    // An explicit force is honoured as-is, even off a console (e.g. piping to a
    // pager that interprets ANSI).
    if force.as_deref().is_some_and(|v| v != "0") {
        return true;
    }
    // Otherwise the "yes" came from the stream being an interactive terminal.
    // ANSI only renders if that terminal understands it; on Windows we must
    // turn on Virtual Terminal processing first. If that fails (a legacy
    // console, or output we couldn't enable), fall back to plain text so the
    // stream shows readable logs instead of raw escape codes.
    ansi_capable(stream)
}

/// Whether ANSI can render on `stream`. Always true off Windows (Unix TTYs
/// handle ANSI natively); on Windows, enables Virtual Terminal processing for
/// the stream's console and reports whether that succeeded.
#[cfg(windows)]
fn ansi_capable(stream: Stream) -> bool {
    match stream {
        Stream::Stdout => winansi::enable(winansi::STD_OUTPUT_HANDLE),
        Stream::Stderr => winansi::enable(winansi::STD_ERROR_HANDLE),
    }
}

#[cfg(not(windows))]
fn ansi_capable(_stream: Stream) -> bool {
    true
}

/// Minimal kernel32 console bindings: just enough to turn on ANSI (Virtual
/// Terminal) output for a standard handle, so the Windows console renders the
/// escape codes the CLI emits instead of printing them literally. Avoids a
/// `windows-sys` dependency for three calls.
#[cfg(windows)]
mod winansi {
    use std::ffi::c_void;

    pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    /// Enable ANSI on the given standard handle. Returns whether ANSI will
    /// render afterwards: false when the handle isn't a console (redirected to
    /// a file or pipe) or VT can't be turned on (a pre-VT legacy console).
    pub fn enable(std_handle: u32) -> bool {
        // SAFETY: `GetStdHandle` returns a borrowed handle we never close;
        // `mode` is a valid local out-param. All three are documented kernel32
        // console calls.
        unsafe {
            let handle = GetStdHandle(std_handle);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return false; // not a console
            }
            if (mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0 {
                return true; // already enabled
            }
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
}

/// Whether ANSI colour/styling should be emitted on **stderr** (status lines,
/// the spinner). Decided once and cached.
pub fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| want_color(Stream::Stderr, std::io::stderr().is_terminal()))
}

/// Whether ANSI colour/styling should be emitted on **stdout**. Decided once
/// and cached. Kept distinct from [`color_enabled`] so machine-readable stdout
/// (e.g. `zkv roles`, `zkv history`) stays plain when piped even if stderr is a
/// terminal.
pub fn stdout_color() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| want_color(Stream::Stdout, std::io::stdout().is_terminal()))
}

/// Wrap `s` in the given SGR parameter(s) (e.g. `"32"`, `"1;36"`), resetting
/// afterwards. A no-op returning `s` unchanged when stderr colour is disabled.
fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// De-emphasised secondary text, a soft light gray (256-colour). Used for
/// spinner labels, sub-details, and hints. Deliberately a notch lighter than
/// the terminal's "dim" attribute (SGR 2), which renders quite dark on most
/// themes, so muted text stays legible.
const MUTED: &str = "38;5;245";

pub fn bold(s: &str) -> String {
    paint("1", s)
}
/// De-emphasised text. See `MUTED`.
pub fn dim(s: &str) -> String {
    paint(MUTED, s)
}
pub fn red(s: &str) -> String {
    paint("31", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}
/// Alias for [`dim`]: de-emphasised secondary text in the same soft gray.
pub fn gray(s: &str) -> String {
    paint(MUTED, s)
}

/// Styling helpers for content printed to **stdout**, gated on
/// [`stdout_color`] rather than [`color_enabled`]. Use these when a command's
/// human-facing stdout should be colourised interactively but stay plain when
/// piped (e.g. `zkv roles`, `zkv history`).
pub mod out {
    use super::{stdout_color, MUTED};

    fn paint(code: &str, s: &str) -> String {
        if stdout_color() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    pub fn bold(s: &str) -> String {
        paint("1", s)
    }
    pub fn dim(s: &str) -> String {
        paint(MUTED, s)
    }
    pub fn red(s: &str) -> String {
        paint("31", s)
    }
    pub fn green(s: &str) -> String {
        paint("32", s)
    }
    pub fn yellow(s: &str) -> String {
        paint("33", s)
    }
    pub fn cyan(s: &str) -> String {
        paint("36", s)
    }
    /// A bold section header, e.g. `Owners`.
    pub fn header(s: &str) -> String {
        paint("1;36", s)
    }
}

// ---------------------------------------------------------------------------
// Quiet mode (GUI)
// ---------------------------------------------------------------------------

/// Process-global "suppress interactive CLI chrome" latch. When set, the
/// animated [`Spinner`] and the `success`/`arrow`/`info`/`warn`/`failure`/
/// `hint` status lines below render nothing, leaving stderr for `tracing`
/// log events only.
///
/// The GUI transports flip this on at startup (see
/// [`Engine::new`](crate::gui::Engine::new)): a GUI drives sync from a
/// background loop while it may share the launching terminal, so without this
/// the CLI's human-facing chrome (most visibly the "Syncing… x / y" spinner)
/// bleeds onto a console meant to carry only appropriately-levelled logs.
/// Colour for the logs themselves is governed by [`color_enabled`], which is
/// deliberately *not* gated on this, so logs stay coloured on a TTY.
///
/// Write-once and never reset; CLI paths never set it, so their output is
/// unchanged.
static QUIET: AtomicBool = AtomicBool::new(false);

/// Suppress interactive CLI status output (the spinner + status lines) for the
/// rest of the process. See `QUIET`. Idempotent.
pub fn set_quiet() {
    QUIET.store(true, Ordering::Relaxed);
}

/// Whether interactive CLI status output is currently suppressed (GUI mode).
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Status lines (all to stderr)
// ---------------------------------------------------------------------------

/// Emit a fully-formatted status line to stderr, unless interactive output has
/// been suppressed for GUI mode (see [`is_quiet`]).
fn emit(line: String) {
    if !is_quiet() {
        eprintln!("{line}");
    }
}

/// `✓ <msg>`: a completed/confirmed step, check in green.
pub fn success(msg: impl AsRef<str>) {
    emit(format!("{} {}", green("✓"), msg.as_ref()));
}

/// `→ <msg>`: an action taken or a value produced, arrow in cyan.
pub fn arrow(msg: impl AsRef<str>) {
    emit(format!("{} {}", cyan("→"), msg.as_ref()));
}

/// `• <msg>`: an in-progress/neutral status note, bullet dimmed.
pub fn info(msg: impl AsRef<str>) {
    emit(format!("{} {}", gray("•"), msg.as_ref()));
}

/// `! <msg>`: a non-fatal warning, marker in yellow.
pub fn warn(msg: impl AsRef<str>) {
    emit(format!("{} {}", yellow("!"), yellow(msg.as_ref())));
}

/// `✗ <msg>`: an error/aborted action, cross in red.
pub fn failure(msg: impl AsRef<str>) {
    emit(format!("{} {}", red("✗"), msg.as_ref()));
}

/// A de-emphasised secondary line (gray), e.g. a hint or sub-detail. Indented
/// two spaces to read as a continuation of the line above.
pub fn hint(msg: impl AsRef<str>) {
    emit(format!("  {}", gray(msg.as_ref())));
}

/// Abbreviate a long hex id (txid, hash) to `head…tail`, leaving short ids
/// untouched. Mirrors the `96feedf9…584166214` style used across the CLI.
pub fn short_hash(s: &str) -> String {
    const HEAD: usize = 8;
    const TAIL: usize = 9;
    if s.len() <= HEAD + TAIL + 1 {
        return s.to_owned();
    }
    format!("{}…{}", &s[..HEAD], &s[s.len() - TAIL..])
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

/// Braille spinner frames (smooth single-cell rotation), Heroku/ora-style.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Frame cadence.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// The pure spinner-visibility decision, factored out of [`spinner_enabled`]
/// so it can be unit-tested without touching the process-global latch: the
/// spinner paints only with stderr colour/TTY support and when interactive
/// output hasn't been muted for GUI mode.
fn decide_spinner(color: bool, quiet: bool) -> bool {
    color && !quiet
}

/// Whether the animated spinner should render: requires stderr colour/TTY
/// support ([`color_enabled`]) and that interactive output hasn't been
/// suppressed for GUI mode ([`is_quiet`]).
fn spinner_enabled() -> bool {
    decide_spinner(color_enabled(), is_quiet())
}

/// An animated, single-line stderr spinner for long-running async work.
///
/// Repaints `⠋ <label>` in place every `SPINNER_INTERVAL` until dropped or
/// [`stop`](Spinner::stop)ped, which erases the line. The label is re-evaluated
/// on every frame, so callers can show live progress (see
/// [`start_with`](Spinner::start_with)). Rendering is suppressed entirely when
/// colour/TTY isn't available (so logs and pipes stay clean) or when
/// interactive output is muted for GUI mode (see [`is_quiet`]), and an optional
/// start delay lets fast operations finish before anything is shown (no flicker
/// on the happy path).
pub struct Spinner {
    handle: tokio::task::JoinHandle<()>,
}

impl Spinner {
    /// Start spinning immediately with a fixed label.
    pub fn start(message: impl Into<String>) -> Self {
        Self::start_after(message, Duration::ZERO)
    }

    /// Start spinning with a fixed label, but only paint the first frame once
    /// `delay` has elapsed. If the spinner is stopped before then, nothing is
    /// ever shown.
    pub fn start_after(message: impl Into<String>, delay: Duration) -> Self {
        let message = message.into();
        Self::start_with(move || message.clone(), delay)
    }

    /// Start spinning with a label re-computed from `label` on every frame, so
    /// the line can reflect live progress. As with [`start_after`], the first
    /// frame is withheld until `delay` elapses.
    pub fn start_with<F>(label: F, delay: Duration) -> Self
    where
        F: Fn() -> String + Send + 'static,
    {
        let enabled = spinner_enabled();
        let handle = tokio::spawn(async move {
            if !enabled {
                return;
            }
            tokio::time::sleep(delay).await;
            let mut i = 0usize;
            loop {
                let frame = SPINNER_FRAMES[i % SPINNER_FRAMES.len()];
                // Leading \r returns to column 0; the trailing escape clears any
                // residue from a previously longer line.
                eprint!("\r{} {}\x1b[K", cyan(frame), dim(&label()));
                let _ = std::io::stderr().flush();
                tokio::time::sleep(SPINNER_INTERVAL).await;
                i += 1;
            }
        });
        Spinner { handle }
    }

    /// Stop the spinner and erase its line.
    pub async fn stop(self) {
        self.handle.abort();
        let _ = self.handle.await;
        clear_line();
    }
}

/// Erase the current stderr line (only when the spinner would have painted, so
/// a muted/non-TTY caller emits no stray escape sequence).
fn clear_line() {
    if spinner_enabled() {
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_passes_through_short_ids() {
        assert_eq!(short_hash(""), "");
        assert_eq!(short_hash("deadbeef"), "deadbeef");
        // Boundary: HEAD(8) + TAIL(9) + 1 separator = 18 chars stays verbatim.
        let eighteen = "a".repeat(18);
        assert_eq!(short_hash(&eighteen), eighteen);
    }

    #[test]
    fn short_hash_abbreviates_long_ids() {
        let txid = "96feedf90000000000000000000000584166214"; // 40 chars
        assert_eq!(short_hash(txid), "96feedf9…584166214");
        // One past the boundary is the shortest input that abbreviates.
        let nineteen = "b".repeat(19);
        assert_eq!(short_hash(&nineteen), "bbbbbbbb…bbbbbbbbb");
    }

    #[test]
    fn decide_color_opt_out_always_wins() {
        // NO_COLOR beats both a TTY and an explicit force.
        assert!(!decide_color(true, None, true, None));
        assert!(!decide_color(true, Some("1"), true, None));
    }

    #[test]
    fn decide_color_force_overrides_non_tty() {
        assert!(decide_color(false, Some("1"), false, None));
        // "0" is the documented disable value; it does not force.
        assert!(!decide_color(false, Some("0"), false, None));
    }

    #[test]
    fn decide_color_requires_tty_by_default() {
        assert!(!decide_color(false, None, false, None));
        assert!(decide_color(false, None, true, None));
        assert!(decide_color(false, None, true, Some("xterm-256color")));
    }

    #[test]
    fn decide_color_dumb_terminal_is_plain() {
        assert!(!decide_color(false, None, true, Some("dumb")));
    }

    #[test]
    fn quiet_mutes_spinner_even_on_a_colour_tty() {
        // GUI mode (quiet) suppresses the spinner regardless of colour/TTY, so
        // the "Syncing… x / y" line never bleeds onto the log console.
        assert!(decide_spinner(true, false)); // colour TTY, CLI → render
        assert!(!decide_spinner(true, true)); // colour TTY, GUI → muted
        assert!(!decide_spinner(false, false)); // no colour → never renders
        assert!(!decide_spinner(false, true));
    }
}
