//! The `:` command grammar — a single source of truth for the palette.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::screens::timer::TimerVerb;
use crate::app::screens::ScreenKind;
use crate::ui::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Nav,
    Action,
    Housekeeping,
}

#[derive(Debug, Clone, Copy)]
pub enum Arg {
    None,
    Enum(&'static [&'static str]),
    Text,
}

/// Data rather than closures, so the table stays a `const`.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Nav(ScreenKind),
    Timer,
    Note,
    Log,
    /// Named for what it dispatches rather than the `:target` verb, to avoid a
    /// `Target::Target` stutter.
    Declare,
    Quit,
    Write,
    Logs,
    Logout,
    Help,
}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub verb: &'static str,
    pub aliases: &'static [&'static str],
    pub kind: Kind,
    pub arg: Arg,
    pub help: &'static str,
    pub target: Target,
}

/// Order is `:help` display order.
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
        verb: "week",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "the week board — planned vs done",
        target: Target::Nav(ScreenKind::Week),
    },
    Entry {
        verb: "inbox",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "triage the assisted-capture drafts",
        target: Target::Nav(ScreenKind::Inbox),
    },
    Entry {
        verb: "queue",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "the offline write queue — inspect, retry, drop, reconcile",
        target: Target::Nav(ScreenKind::Queue),
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
        verb: "audit",
        aliases: &[],
        kind: Kind::Nav,
        arg: Arg::None,
        help: "flagged segments (Progress ▸ audit)",
        target: Target::Nav(ScreenKind::Audit),
    },
    Entry {
        verb: "timer",
        aliases: &["t"],
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
        verb: "log",
        aliases: &[],
        kind: Kind::Action,
        arg: Arg::None,
        help: "log a completed activity",
        target: Target::Log,
    },
    Entry {
        verb: "target",
        aliases: &[],
        kind: Kind::Action,
        arg: Arg::None,
        help: "declare a weekly target (Progress)",
        target: Target::Declare,
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

const NAV_VERBS: &[&str] = &[
    "home",
    "books",
    "activities",
    "notes",
    "review",
    "progress",
    "week",
    "inbox",
    "queue",
    "timer",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Nav(ScreenKind),
    Timer(TimerVerb),
    Note(Option<String>),
    Log,
    Target,
    Quit,
    Write,
    Logs,
    Logout,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    Empty,
    Run(Command),
    Unknown(String),
    Ambiguous(Vec<&'static str>),
    BadArg {
        verb: &'static str,
        expected: &'static [&'static str],
        got: String,
    },
    AmbiguousArg {
        verb: &'static str,
        matches: Vec<&'static str>,
    },
}

enum VerbHit {
    One(&'static Entry),
    Many(Vec<&'static str>),
    None,
}

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

fn build(entry: &Entry, arg: &str) -> Parse {
    match entry.target {
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
        Target::Log => Parse::Run(Command::Log),
        Target::Declare => Parse::Run(Command::Target),
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

/// Tab completion to the longest common prefix — vim's `wildmode=longest`, so
/// no cycle state is tracked between keystrokes.
pub fn complete(input: &str) -> String {
    if let Some((verb_part, rest)) = input.split_once(char::is_whitespace) {
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

pub struct Hint {
    pub text: String,
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

pub fn hint(input: &str) -> Hint {
    if input.trim().is_empty() {
        return plain(format!("{} · :help · Tab completes", NAV_VERBS.join(" · ")));
    }
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
    match resolve_verb(input.trim()) {
        VerbHit::One(entry) if entry.verb == input.trim() => plain(format!("→ {}", entry.help)),
        VerbHit::One(entry) => plain(format!("→ {} — {}", entry.verb, entry.help)),
        VerbHit::Many(candidates) => plain(candidates.join(" · ")),
        VerbHit::None => warn("unknown — try :help"),
    }
}

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
        assert_eq!(cmd("week"), Command::Nav(ScreenKind::Week));
        assert_eq!(cmd("timer"), Command::Nav(ScreenKind::Timer));
        assert_eq!(cmd("queue"), Command::Nav(ScreenKind::Queue));
    }

    #[test]
    fn queue_resolves_by_prefix_and_leaves_the_quit_alias_alone() {
        assert_eq!(cmd("q"), Command::Quit);
        assert_eq!(parse("qu"), Parse::Ambiguous(vec!["queue", "quit"]));
        assert_eq!(cmd("que"), Command::Nav(ScreenKind::Queue));
        assert_eq!(cmd("qui"), Command::Quit);
    }

    #[test]
    fn week_resolves_by_prefix_and_leaves_the_write_alias_alone() {
        assert_eq!(cmd("we"), Command::Nav(ScreenKind::Week));
        assert_eq!(cmd("wee"), Command::Nav(ScreenKind::Week));
        assert_eq!(cmd("w"), Command::Write);
    }

    #[test]
    fn unambiguous_prefixes_resolve() {
        assert_eq!(cmd("act"), Command::Nav(ScreenKind::Activities));
        assert_eq!(cmd("ac"), Command::Nav(ScreenKind::Activities));
        assert_eq!(cmd("au"), Command::Nav(ScreenKind::Audit));
        assert!(matches!(parse("a"), Parse::Ambiguous(_)));
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
        assert_eq!(cmd("t start"), Command::Timer(TimerVerb::Start));
        assert_eq!(cmd("timer p"), Command::Timer(TimerVerb::Pause));
        assert_eq!(cmd("timer r"), Command::Timer(TimerVerb::Resume));
    }

    #[test]
    fn timer_ambiguous_argument_reports_candidates() {
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
        assert_eq!(
            cmd("note  two  spaces"),
            Command::Note(Some("two  spaces".into()))
        );
    }

    #[test]
    fn ambiguous_prefixes_report_all_candidates() {
        assert_eq!(parse("l"), Parse::Ambiguous(vec!["log", "logs", "logout"]));
        assert_eq!(parse("h"), Parse::Ambiguous(vec!["home", "help"]));
        assert_eq!(parse("n"), Parse::Ambiguous(vec!["notes", "note"]));
    }

    #[test]
    fn exact_note_wins_over_the_notes_prefix() {
        assert_eq!(cmd("note"), Command::Note(None));
        assert_eq!(cmd("notes"), Command::Nav(ScreenKind::Notes));
    }

    #[test]
    fn unknown_verb_is_reported() {
        assert_eq!(parse("wobble"), Parse::Unknown("wobble".into()));
    }

    #[test]
    fn completion_extends_to_the_longest_common_prefix() {
        assert_eq!(complete("t"), "t");
        assert_eq!(complete("ti"), "timer ");
        assert_eq!(complete("time"), "timer ");
        assert_eq!(complete("ta"), "target");
        assert_eq!(complete("boo"), "books");
        assert_eq!(complete("act"), "activities");
        assert_eq!(complete("n"), "note");
        assert_eq!(complete("lo"), "log");
        assert_eq!(complete("timer s"), "timer st");
        assert_eq!(complete("timer p"), "timer pause");
        assert_eq!(complete("zzz"), "zzz");
    }

    #[test]
    fn hint_states_read_as_the_four_line_states() {
        let empty = hint("");
        assert!(empty.text.contains("home"));
        assert!(empty.text.contains("timer"));
        assert!(!empty.warn);
        assert!(hint("l").text.contains("logs"));
        assert!(hint("l").text.contains("logout"));
        assert!(hint("books").text.contains("browse books"));
        assert!(hint("timer ").text.contains("start"));
        assert!(hint("zzz").warn);
        assert!(hint("zzz").text.contains(":help"));
    }

    #[test]
    fn help_summary_lists_the_whole_table() {
        let s = help_summary();
        assert!(s.contains("home"));
        assert!(s.contains("timer[start|pause|resume|stop]"));
        assert!(s.contains("note <text>"));
        assert!(s.contains("log"));
        assert!(s.contains("target"));
        assert!(s.contains("logout"));
    }

    #[test]
    fn log_and_target_verbs_resolve() {
        assert_eq!(cmd("log"), Command::Log);
        assert_eq!(cmd("log 45m reading"), Command::Log);
        assert_eq!(cmd("target"), Command::Target);
        assert_eq!(cmd("ta"), Command::Target);
        assert_eq!(cmd("tar"), Command::Target);
    }

    #[test]
    fn target_is_a_full_word_and_never_steals_the_timer_prefix() {
        assert_eq!(cmd("t"), Command::Nav(ScreenKind::Timer));
        assert_eq!(cmd("t start"), Command::Timer(TimerVerb::Start));
        assert_eq!(cmd("ti"), Command::Nav(ScreenKind::Timer));
        assert_eq!(cmd("tim"), Command::Nav(ScreenKind::Timer));
        let target = ENTRIES.iter().find(|e| e.verb == "target").unwrap();
        assert!(target.aliases.is_empty());
        assert!(matches!(
            parse("t"),
            Parse::Run(Command::Nav(ScreenKind::Timer))
        ));
    }

    #[test]
    fn log_verb_hint_and_completion_follow_the_table() {
        assert!(hint("log").text.contains("log a completed activity"));
        assert!(hint("target").text.contains("declare a weekly target"));
        assert!(hint("t").text.contains("timer"));
    }

    #[test]
    fn a_nav_verb_ignores_a_stray_argument() {
        assert_eq!(cmd("books rust"), Command::Nav(ScreenKind::Books));
    }

    #[test]
    fn the_empty_line_hint_advertises_only_table_nav_verbs() {
        for verb in NAV_VERBS {
            let entry = ENTRIES
                .iter()
                .find(|e| e.verb == *verb)
                .unwrap_or_else(|| panic!(":{verb} is not in the table"));
            assert_eq!(entry.kind, Kind::Nav, ":{verb}");
        }
    }

    #[test]
    fn help_summary_names_every_verb_in_the_table() {
        let s = help_summary();
        let words: Vec<&str> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        for e in ENTRIES {
            assert!(words.contains(&e.verb), ":{} missing from {s}", e.verb);
        }
    }
}
