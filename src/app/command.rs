//! The `:` command grammar — a single source of truth for the palette.
//!
//! One static table (`ENTRIES`) pins the verb inventory from the daily-loop
//! brief (§5, command-palette.html): navigation (`:home` `:books`
//! `:activities` `:notes` `:review` `:progress` `:timer`), actions
//! (`:timer start|pause|resume|stop`, `:note <text>`) and housekeeping (`:q`
//! `:logs` `:w` `:logout`, plus `:help`). The dispatcher, Tab completion, the
//! inline line-state hints, and `:help` all read this one table, so the grammar
//! never drifts between what runs and what the UI advertises.
//!
//! Resolution is vim-flavoured: an exact verb or alias wins; otherwise an
//! unambiguous prefix resolves (`:act` → activities, `:t start` → timer start),
//! and an ambiguous prefix (`:l` → logs/logout) reports its candidates rather
//! than guessing.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::screens::timer::TimerVerb;
use crate::app::screens::ScreenKind;
use crate::ui::theme;

/// The three verb families the brief groups the grammar into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Nav,
    Action,
    Housekeeping,
}

/// The argument shape a verb accepts.
#[derive(Debug, Clone, Copy)]
pub enum Arg {
    /// A bare verb — no argument.
    None,
    /// A fixed set of sub-verbs (e.g. timer `start|pause|resume|stop`). Bare is
    /// still valid for `timer` (it navigates); a non-empty argument is validated
    /// against this set with the same prefix rules as the verb itself.
    Enum(&'static [&'static str]),
    /// Free text captured verbatim (e.g. `:note <text>`).
    Text,
}

/// What a resolved verb dispatches to. Kept as data (not closures) so the table
/// stays a `const`; `build` maps a `Target` + argument to a runnable `Command`.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Nav(ScreenKind),
    /// Bare navigates to the Timer screen; an argument runs a timer action.
    Timer,
    /// Opens quick-capture, prefilled when text is supplied.
    Note,
    Quit,
    Write,
    Logs,
    Logout,
    Help,
}

/// One row of the grammar table.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub verb: &'static str,
    pub aliases: &'static [&'static str],
    pub kind: Kind,
    pub arg: Arg,
    pub help: &'static str,
    pub target: Target,
}

/// The pinned verb inventory. Order is display order (help + empty-line hint).
pub const ENTRIES: &[Entry] = &[
    Entry {
        verb: "home",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "go to Home",
        target: Target::Nav(ScreenKind::Home),
    },
    Entry {
        verb: "books",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "browse books",
        target: Target::Nav(ScreenKind::Books),
    },
    Entry {
        verb: "activities",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "the activities table",
        target: Target::Nav(ScreenKind::Activities),
    },
    Entry {
        verb: "notes",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "the notes browser",
        target: Target::Nav(ScreenKind::Notes),
    },
    Entry {
        verb: "review",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "the review dashboard",
        target: Target::Nav(ScreenKind::Review),
    },
    Entry {
        verb: "progress",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "pace meters",
        target: Target::Nav(ScreenKind::Progress),
    },
    Entry {
        verb: "settings",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "timer knobs (read-only)",
        target: Target::Nav(ScreenKind::Settings),
    },
    Entry {
        verb: "timer",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::Enum(TimerVerb::NAMES),
        help: "timer screen · start|pause|resume|stop",
        target: Target::Timer,
    },
    Entry {
        verb: "note",
        aliases: &[],
        kind: Kind::Action,
        arg: Arg::Text,
        help: "quick-capture, prefilled with <text>",
        target: Target::Note,
    },
    Entry {
        verb: "logs",
        aliases: &[],
        kind: Kind::Housekeeping,
        arg: Arg::None,
        help: "show the log directory",
        target: Target::Logs,
    },
    Entry {
        verb: "w",
        aliases: &["write"],
        kind: Kind::Housekeeping,
        arg: Arg::None,
        help: "submit the current form",
        target: Target::Write,
    },
    Entry {
        verb: "logout",
        aliases: &[],
        kind: Kind::Housekeeping,
        arg: Arg::None,
        help: "how to sign out",
        target: Target::Logout,
    },
    Entry {
        verb: "quit",
        aliases: &["q"],
        kind: Kind::Housekeeping,
        arg: Arg::None,
        help: "quit engineer",
        target: Target::Quit,
    },
    Entry {
        verb: "help",
        aliases: &[],
        kind: Kind::Housekeeping,
        arg: Arg::None,
        help: "list the commands",
        target: Target::Help,
    },
];

/// The bare navigation verbs, in display order — the empty-line hint and the
/// help summary read from here.
const NAV_VERBS: &[&str] = &[
    "home",
    "books",
    "activities",
    "notes",
    "review",
    "progress",
    "timer",
];

/// A runnable command, produced once a verb (and any argument) fully resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Nav(ScreenKind),
    Timer(TimerVerb),
    /// `None` opens a blank capture; `Some` prefills it with the text.
    Note(Option<String>),
    Quit,
    Write,
    Logs,
    Logout,
    Help,
}

/// The outcome of parsing the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    /// Just `:` — nothing to run.
    Empty,
    /// A resolved, runnable command.
    Run(Command),
    /// No verb matched (carries what was typed).
    Unknown(String),
    /// A prefix matched several verbs.
    Ambiguous(Vec<&'static str>),
    /// A verb resolved but its argument is not one of the accepted sub-verbs.
    BadArg {
        verb: &'static str,
        expected: &'static [&'static str],
        got: String,
    },
    /// A verb resolved but its argument prefix matched several sub-verbs.
    AmbiguousArg {
        verb: &'static str,
        matches: Vec<&'static str>,
    },
}

enum VerbHit {
    /// Exact verb or alias, or unambiguous prefix.
    One(&'static Entry),
    Many(Vec<&'static str>),
    None,
}

/// Resolve the first token to a table entry: exact verb/alias, else a unique
/// prefix, else the ambiguous candidates (or nothing).
fn resolve_verb(tok: &str) -> VerbHit {
    if let Some(e) = ENTRIES
        .iter()
        .find(|e| e.verb == tok || e.aliases.contains(&tok))
    {
        return VerbHit::One(e);
    }
    let hits: Vec<&Entry> = ENTRIES
        .iter()
        .filter(|e| e.verb.starts_with(tok) || e.aliases.iter().any(|a| a.starts_with(tok)))
        .collect();
    match hits.as_slice() {
        [] => VerbHit::None,
        [one] => VerbHit::One(one),
        many => VerbHit::Many(many.iter().map(|e| e.verb).collect()),
    }
}

enum ArgHit<'a> {
    One(&'a str),
    Many(Vec<&'a str>),
    None,
}

/// Resolve a sub-verb argument against a fixed set, with the same exact-then-
/// prefix rules the verb uses (`:timer p` → pause, `:timer s` → start/stop).
fn resolve_arg<'a>(names: &'a [&'a str], tok: &str) -> ArgHit<'a> {
    if let Some(n) = names.iter().find(|n| **n == tok) {
        return ArgHit::One(n);
    }
    let hits: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| n.starts_with(tok))
        .collect();
    match hits.as_slice() {
        [] => ArgHit::None,
        [one] => ArgHit::One(one),
        _ => ArgHit::Many(hits),
    }
}

/// Parse a command-line buffer (the text after `:`, no leading colon) into a
/// [`Parse`] outcome.
pub fn parse(input: &str) -> Parse {
    let input = input.trim();
    if input.is_empty() {
        return Parse::Empty;
    }
    let (verb_tok, arg) = match input.split_once(char::is_whitespace) {
        Some((v, rest)) => (v, rest.trim()),
        None => (input, ""),
    };
    match resolve_verb(verb_tok) {
        VerbHit::None => Parse::Unknown(verb_tok.to_string()),
        VerbHit::Many(candidates) => Parse::Ambiguous(candidates),
        VerbHit::One(entry) => build(entry, arg),
    }
}

/// Map a resolved entry + argument to a runnable command (or an argument error).
fn build(entry: &Entry, arg: &str) -> Parse {
    match entry.target {
        // Nav verbs ignore any stray argument.
        Target::Nav(kind) => Parse::Run(Command::Nav(kind)),
        Target::Timer => {
            if arg.is_empty() {
                return Parse::Run(Command::Nav(ScreenKind::Timer));
            }
            match resolve_arg(TimerVerb::NAMES, arg) {
                ArgHit::One(name) => Parse::Run(Command::Timer(
                    TimerVerb::from_name(name).expect("table names map to a TimerVerb"),
                )),
                ArgHit::Many(matches) => Parse::AmbiguousArg {
                    verb: entry.verb,
                    matches,
                },
                ArgHit::None => Parse::BadArg {
                    verb: entry.verb,
                    expected: TimerVerb::NAMES,
                    got: arg.to_string(),
                },
            }
        }
        Target::Note => Parse::Run(Command::Note((!arg.is_empty()).then(|| arg.to_string()))),
        Target::Quit => Parse::Run(Command::Quit),
        Target::Write => Parse::Run(Command::Write),
        Target::Logs => Parse::Run(Command::Logs),
        Target::Logout => Parse::Run(Command::Logout),
        Target::Help => Parse::Run(Command::Help),
    }
}

fn takes_arg(entry: &Entry) -> bool {
    matches!(entry.arg, Arg::Enum(_) | Arg::Text)
}

/// Tab completion: extend the buffer toward the longest common prefix of the
/// matching verbs (vim's `wildmode=longest`), completing a lone match fully and
/// opening an argument slot for verbs that take one. When several verbs still
/// match, the buffer stops at the branch point and the inline hint lists them,
/// so no cycle state has to be tracked between keystrokes. Returns the buffer
/// unchanged when nothing matches.
pub fn complete(input: &str) -> String {
    if let Some((verb_part, rest)) = input.split_once(char::is_whitespace) {
        // Argument region — only enum arguments (timer) complete.
        let arg = rest.trim_start();
        if let VerbHit::One(entry) = resolve_verb(verb_part.trim()) {
            if let Arg::Enum(names) = entry.arg {
                let matches: Vec<&str> = names
                    .iter()
                    .copied()
                    .filter(|n| n.starts_with(arg))
                    .collect();
                if !matches.is_empty() {
                    return format!("{} {}", entry.verb, longest_common_prefix(&matches));
                }
            }
        }
        return input.to_string();
    }
    let hits: Vec<&Entry> = ENTRIES
        .iter()
        .filter(|e| e.verb.starts_with(input))
        .collect();
    match hits.as_slice() {
        [] => input.to_string(),
        [one] if takes_arg(one) => format!("{} ", one.verb),
        [one] => one.verb.to_string(),
        many => longest_common_prefix(&many.iter().map(|e| e.verb).collect::<Vec<_>>()),
    }
}

/// The inline tail shown after the cursor while typing — the four line states
/// from the brief: empty (top verbs), partial (matches / completion), a resolved
/// verb (its help + argument shape), and unknown (helpful, not hostile).
pub struct Hint {
    pub text: String,
    /// True for the unknown / bad-argument states, tinted to read as a soft warning.
    pub warn: bool,
}

fn plain(text: impl Into<String>) -> Hint {
    Hint {
        text: text.into(),
        warn: false,
    }
}

fn warn(text: impl Into<String>) -> Hint {
    Hint {
        text: text.into(),
        warn: true,
    }
}

/// Classify the current buffer into an inline [`Hint`].
pub fn hint(input: &str) -> Hint {
    // Empty line — advertise what's possible.
    if input.trim().is_empty() {
        return plain(format!("{} · :help · Tab completes", NAV_VERBS.join(" · ")));
    }
    // Argument region (verb + space).
    if let Some((verb_part, rest)) = input.split_once(char::is_whitespace) {
        let verb = verb_part.trim();
        if verb.is_empty() {
            return plain(format!("{} · :help · Tab completes", NAV_VERBS.join(" · ")));
        }
        let arg = rest.trim();
        return match resolve_verb(verb) {
            VerbHit::One(entry) => match entry.arg {
                Arg::Enum(names) => {
                    let matches: Vec<&str> = names
                        .iter()
                        .copied()
                        .filter(|n| n.starts_with(arg))
                        .collect();
                    if arg.is_empty() {
                        plain(names.join(" · "))
                    } else if matches.is_empty() {
                        warn(format!("? try {}", names.join("|")))
                    } else {
                        plain(matches.join(" · "))
                    }
                }
                Arg::Text => plain("<text> — opens quick-capture prefilled"),
                Arg::None => plain(format!("{} takes no argument", entry.verb)),
            },
            _ => warn("unknown — try :help"),
        };
    }
    // Single verb token.
    match resolve_verb(input.trim()) {
        VerbHit::One(entry) if entry.verb == input.trim() => plain(format!("→ {}", entry.help)),
        VerbHit::One(entry) => plain(format!("→ {} — {}", entry.verb, entry.help)),
        VerbHit::Many(candidates) => plain(candidates.join(" · ")),
        VerbHit::None => warn("unknown — try :help"),
    }
}

/// The command line as a styled footer row: `:input█` plus the inline hint.
pub fn render_line(input: &str) -> Line<'static> {
    let mut spans = vec![
        Span::styled(":", theme::focused()),
        Span::raw(input.to_string()),
        Span::styled("█", theme::muted()),
    ];
    let h = hint(input);
    if !h.text.is_empty() {
        let style = if h.warn {
            Style::default().fg(theme::WARN)
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!("   {}", h.text), style));
    }
    Line::from(spans)
}

/// A one-line reference of the whole table, shown by `:help`. Built from
/// `ENTRIES` so it can never drift from what actually runs.
pub fn help_summary() -> String {
    let mut nav = Vec::new();
    let mut action = Vec::new();
    let mut misc = Vec::new();
    for e in ENTRIES {
        let tok = match e.arg {
            Arg::Enum(names) => format!("{}[{}]", e.verb, names.join("|")),
            Arg::Text => format!("{} <text>", e.verb),
            Arg::None => e.verb.to_string(),
        };
        match e.kind {
            Kind::Nav => nav.push(tok),
            Kind::Action => action.push(tok),
            Kind::Housekeeping => misc.push(tok),
        }
    }
    format!(
        "nav: {} · actions: {} · misc: {}",
        nav.join(" "),
        action.join(" "),
        misc.join(" ")
    )
}

fn longest_common_prefix(items: &[&str]) -> String {
    let Some((first, rest)) = items.split_first() else {
        return String::new();
    };
    let mut len = first.len();
    for s in rest {
        let common = first
            .bytes()
            .zip(s.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        len = len.min(common);
    }
    first[..len].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(input: &str) -> Command {
        match parse(input) {
            Parse::Run(c) => c,
            other => panic!("expected a runnable command for {input:?}, got {other:?}"),
        }
    }

    #[test]
    fn empty_line_is_empty() {
        assert_eq!(parse(""), Parse::Empty);
        assert_eq!(parse("   "), Parse::Empty);
    }

    #[test]
    fn bare_nav_verbs_resolve() {
        assert_eq!(cmd("home"), Command::Nav(ScreenKind::Home));
        assert_eq!(cmd("books"), Command::Nav(ScreenKind::Books));
        assert_eq!(cmd("activities"), Command::Nav(ScreenKind::Activities));
        assert_eq!(cmd("notes"), Command::Nav(ScreenKind::Notes));
        assert_eq!(cmd("review"), Command::Nav(ScreenKind::Review));
        assert_eq!(cmd("progress"), Command::Nav(ScreenKind::Progress));
        assert_eq!(cmd("timer"), Command::Nav(ScreenKind::Timer));
    }

    #[test]
    fn unambiguous_prefixes_resolve() {
        // The ticket's worked example: :act and :a both reach activities now
        // that the unpinned :activity verb is gone.
        assert_eq!(cmd("act"), Command::Nav(ScreenKind::Activities));
        assert_eq!(cmd("a"), Command::Nav(ScreenKind::Activities));
        assert_eq!(cmd("b"), Command::Nav(ScreenKind::Books));
        assert_eq!(cmd("r"), Command::Nav(ScreenKind::Review));
        assert_eq!(cmd("p"), Command::Nav(ScreenKind::Progress));
        assert_eq!(cmd("t"), Command::Nav(ScreenKind::Timer));
    }

    #[test]
    fn housekeeping_verbs_and_aliases_resolve() {
        assert_eq!(cmd("q"), Command::Quit);
        assert_eq!(cmd("quit"), Command::Quit);
        assert_eq!(cmd("w"), Command::Write);
        assert_eq!(cmd("write"), Command::Write);
        assert_eq!(cmd("logs"), Command::Logs);
        assert_eq!(cmd("logout"), Command::Logout);
        assert_eq!(cmd("help"), Command::Help);
    }

    #[test]
    fn timer_actions_resolve_with_prefixes() {
        assert_eq!(cmd("timer start"), Command::Timer(TimerVerb::Start));
        assert_eq!(cmd("timer pause"), Command::Timer(TimerVerb::Pause));
        assert_eq!(cmd("timer resume"), Command::Timer(TimerVerb::Resume));
        assert_eq!(cmd("timer stop"), Command::Timer(TimerVerb::Stop));
        // Verb prefix + argument prefix together.
        assert_eq!(cmd("t start"), Command::Timer(TimerVerb::Start));
        assert_eq!(cmd("timer p"), Command::Timer(TimerVerb::Pause));
        assert_eq!(cmd("timer r"), Command::Timer(TimerVerb::Resume));
    }

    #[test]
    fn timer_ambiguous_argument_reports_candidates() {
        // start and stop both begin with s.
        match parse("timer s") {
            Parse::AmbiguousArg { verb, matches } => {
                assert_eq!(verb, "timer");
                assert_eq!(matches, vec!["start", "stop"]);
            }
            other => panic!("expected AmbiguousArg, got {other:?}"),
        }
    }

    #[test]
    fn timer_bad_argument_is_reported() {
        match parse("timer wobble") {
            Parse::BadArg { verb, got, .. } => {
                assert_eq!(verb, "timer");
                assert_eq!(got, "wobble");
            }
            other => panic!("expected BadArg, got {other:?}"),
        }
    }

    #[test]
    fn note_captures_prefill_text_verbatim() {
        assert_eq!(cmd("note"), Command::Note(None));
        assert_eq!(
            cmd("note closures are objects"),
            Command::Note(Some("closures are objects".into()))
        );
        // Internal spacing is preserved; only the ends are trimmed.
        assert_eq!(
            cmd("note  two  spaces"),
            Command::Note(Some("two  spaces".into()))
        );
    }

    #[test]
    fn ambiguous_prefixes_report_all_candidates() {
        assert_eq!(parse("l"), Parse::Ambiguous(vec!["logs", "logout"]));
        assert_eq!(parse("h"), Parse::Ambiguous(vec!["home", "help"]));
        // note (action) and notes (nav) share the whole "note" stem.
        assert_eq!(parse("n"), Parse::Ambiguous(vec!["notes", "note"]));
    }

    #[test]
    fn exact_note_wins_over_the_notes_prefix() {
        // :note is an exact verb, so it never resolves as a prefix of notes.
        assert_eq!(cmd("note"), Command::Note(None));
        assert_eq!(cmd("notes"), Command::Nav(ScreenKind::Notes));
    }

    #[test]
    fn unknown_verb_is_reported() {
        assert_eq!(parse("wobble"), Parse::Unknown("wobble".into()));
    }

    #[test]
    fn completion_extends_to_the_longest_common_prefix() {
        // Lone match that takes an argument opens the slot.
        assert_eq!(complete("t"), "timer ");
        assert_eq!(complete("time"), "timer ");
        // Lone match with no argument completes fully.
        assert_eq!(complete("boo"), "books");
        assert_eq!(complete("act"), "activities");
        // note / notes share the "note" stem.
        assert_eq!(complete("n"), "note");
        // Argument completion: start / stop share "st".
        assert_eq!(complete("timer s"), "timer st");
        assert_eq!(complete("timer p"), "timer pause");
        // Nothing matches — buffer is unchanged.
        assert_eq!(complete("zzz"), "zzz");
    }

    #[test]
    fn hint_states_read_as_the_four_line_states() {
        // Empty: top verbs.
        let empty = hint("");
        assert!(empty.text.contains("home"));
        assert!(empty.text.contains("timer"));
        assert!(!empty.warn);
        // Partial with several matches (Suggest).
        assert!(hint("l").text.contains("logs"));
        assert!(hint("l").text.contains("logout"));
        // Resolved verb shows its help.
        assert!(hint("books").text.contains("browse books"));
        // Timer argument region lists the sub-verbs.
        assert!(hint("timer ").text.contains("start"));
        // Unknown: helpful, flagged as a soft warning.
        assert!(hint("zzz").warn);
        assert!(hint("zzz").text.contains(":help"));
    }

    #[test]
    fn help_summary_lists_the_whole_table() {
        let s = help_summary();
        assert!(s.contains("home"));
        assert!(s.contains("timer[start|pause|resume|stop]"));
        assert!(s.contains("note <text>"));
        assert!(s.contains("logout"));
    }
}
