//! Headless `engineer note` — the notes module's §C twin (docs/designs/notes.dc.html
//! §Headless): five-second capture and the browse reads as one-shots, so a shell
//! user never needs the TUI for a thought.
//!
//! Mirrors the `engineer timer` contract exactly: an `Args` root with a
//! `Subcommand`, a testable `dispatch() -> Outcome`, `--json` beside piped-plain
//! rows, TTY-gated ANSI colour (never when `NO_COLOR` is set), and meaningful
//! exit codes — `0` on success, `1` on a refusal with the reason on stderr.
//!
//! `capture` is the only write, routed through `QueuedClient` like every other
//! mutation: offline it queues a loose thought and prints the provisional line.
//! The reads (`list` / `search` / `show`) are deliberately live-only — there is
//! no note read-cache, so offline they refuse honestly rather than lie.

use std::io::{IsTerminal, Read};

use clap::{Args, Subcommand};
use color_eyre::eyre::Result;

use crate::api::{derive_title_content, Anchor, ApiClient, ApiError, Note, NoteFilters, NoteInput};
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::queue::QueuedClient;

#[derive(Args)]
pub struct NoteArgs {
    /// Emit JSON — a stable object per note (an array for `list`/`search`).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<NoteCmd>,
}

#[derive(Subcommand)]
enum NoteCmd {
    /// Capture a thought — the five-second write. Takes the text inline, from
    /// `-`/piped stdin, or (on a TTY with no text) opens `$EDITOR` git-commit
    /// style. `--book`/`--page` anchor it to a place in a book.
    Capture(CaptureArgs),
    /// Browse your active notes — the derived title, the one-line anchor
    /// read-back (the server's `address_label`), and an age.
    List(ListArgs),
    /// Search your notes by text (the server's `q=`).
    Search(SearchArgs),
    /// Read one note in full — its content and every citation.
    Show { id: i64 },
}

#[derive(Args, Default)]
pub struct CaptureArgs {
    /// The note text. `-` reads stdin; omit entirely to open `$EDITOR` on a TTY.
    text: Option<String>,
    /// Anchor the note to a book — a live search, first match wins (offline
    /// refuses rather than guess the wrong book).
    #[arg(long)]
    book: Option<String>,
    /// Anchor to a page in the book (needs `--book`; dropped without one).
    #[arg(long)]
    page: Option<u32>,
}

#[derive(Args, Default)]
pub struct ListArgs {
    /// Only notes anchored to this book — a live search, first match wins.
    #[arg(long)]
    book: Option<String>,
    /// Include archived: `all` (active + archived) or `only` (archived alone).
    #[arg(long)]
    archived: Option<String>,
}

#[derive(Args, Default)]
pub struct SearchArgs {
    /// The search text (the server's `q=`).
    query: String,
    /// Include archived: `all` (active + archived) or `only` (archived alone).
    #[arg(long)]
    archived: Option<String>,
}

pub async fn run(cfg: &Config, args: NoteArgs) -> Result<i32> {
    let provider = TokenProvider::new(cfg.clone()).await?;
    let token = provider.access_token().await?;
    let api = ApiClient::with_token(cfg.api_url.clone(), token);
    let queued = QueuedClient::new(&api).map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    // Resolve a capture's text (stdin / `$EDITOR`) up front, on the real streams —
    // the buffered, testable `dispatch` only ever sees already-resolved text. An
    // aborted or empty editor is nothing to write: exit 1, nothing queued.
    let cmd = match args.cmd {
        Some(NoteCmd::Capture(mut c)) => match resolve_capture_text(c.text.as_deref())? {
            Some(text) => {
                c.text = Some(text);
                Some(NoteCmd::Capture(c))
            }
            None => {
                eprintln!("nothing captured — editor aborted, or an empty draft");
                return Ok(1);
            }
        },
        other => other,
    };

    let outcome = dispatch(&api, &queued, cmd, args.json, colored).await?;
    for line in &outcome.out {
        println!("{line}");
    }
    for line in &outcome.err {
        eprintln!("{line}");
    }
    Ok(outcome.code)
}

struct Outcome {
    out: Vec<String>,
    err: Vec<String>,
    code: i32,
}

impl Outcome {
    fn ok(line: impl Into<String>) -> Self {
        Self {
            out: vec![line.into()],
            err: vec![],
            code: 0,
        }
    }

    fn lines(out: Vec<String>) -> Self {
        Self {
            out,
            err: vec![],
            code: 0,
        }
    }

    fn refuse(reason: impl Into<String>) -> Self {
        Self {
            out: vec![],
            err: vec![reason.into()],
            code: 1,
        }
    }
}

async fn dispatch(
    api: &ApiClient,
    queued: &QueuedClient,
    cmd: Option<NoteCmd>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    match cmd {
        None => list(api, None, None, json, colored).await,
        Some(NoteCmd::Capture(c)) => capture(api, queued, c, json, colored).await,
        Some(NoteCmd::List(a)) => list(api, a.book, a.archived, json, colored).await,
        Some(NoteCmd::Search(a)) => search(api, a, json, colored).await,
        Some(NoteCmd::Show { id }) => show(api, id, json, colored).await,
    }
}

// ---------------------------------------------------------------- capture

async fn capture(
    api: &ApiClient,
    queued: &QueuedClient,
    args: CaptureArgs,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let text = args.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(Outcome::refuse("note is empty — type a thought first"));
    }

    // The `--book` anchor is a *live* candidates read (the same book search the
    // capture overlay uses). Offline we can't fuzzy-match, and guessing would
    // anchor the wrong book — so refuse with the way forward, exactly like a
    // query'd `timer start`. A loose capture needs no read and rides the queue.
    let book_id = match resolve_book(api, args.book.as_deref()).await? {
        Resolved::Id(id) => id,
        Resolved::Refuse(reason) => return Ok(Outcome::refuse(reason)),
    };

    // One spelling of the content-first rule, shared with the TUI overlay.
    let (title, content) = derive_title_content(&text);
    // The simplest faithful anchor is a book plus a page; a page with no book
    // can't be anchored, so it's dropped (the overlay's `build_input` rule).
    let anchors = match (book_id, args.page) {
        (Some(_), Some(p)) => Some(vec![Anchor {
            page: Some(p),
            ..Default::default()
        }]),
        _ => None,
    };
    let input = NoteInput {
        title: title.clone(),
        content,
        book_id,
        anchors,
        ..Default::default()
    };

    match queued.create_note(&input).await {
        Ok(out) => {
            let provisional = out.is_provisional();
            let note = out.value();
            if json {
                let mut v = json_note(note);
                if provisional {
                    v["queued"] = true.into();
                }
                return Ok(Outcome::ok(v.to_string()));
            }
            let mut line = format!(
                "{} captured · \"{}\" · {}",
                paint("✓", COLOR_OK, colored),
                truncate(&title, 48),
                anchor_label(note).unwrap_or_else(|| "loose".into()),
            );
            if provisional {
                line.push_str(&paint("  · queued (offline)", COLOR_MUTED, colored));
            }
            Ok(Outcome::ok(line))
        }
        Err(e) => write_refuse(e),
    }
}

/// The book-search result: a resolved id (`None` when no `--book` was given), or
/// a refusal reason (no match, or offline).
enum Resolved {
    Id(Option<i64>),
    Refuse(String),
}

/// Resolve a book query to its id via the live book search — the same
/// candidates/search the capture overlay's picker uses (`list_books`, first
/// match wins). `None` query → no filter; offline → a refusal, never a guess.
async fn resolve_book(api: &ApiClient, query: Option<&str>) -> Result<Resolved, ApiError> {
    let Some(q) = query else {
        return Ok(Resolved::Id(None));
    };
    match api.list_books(None, Some(q)).await {
        Ok(list) => match list.data.first() {
            Some(book) => Ok(Resolved::Id(Some(book.id))),
            None => Ok(Resolved::Refuse(format!("no book matches \"{q}\""))),
        },
        Err(ApiError::Transport(_)) => Ok(Resolved::Refuse(format!(
            "offline — can't resolve book \"{q}\"; capture loose or retry online"
        ))),
        Err(e) => Err(e),
    }
}

/// Resolve capture text the way `week reflect` resolves a reflection body: an
/// explicit `-` or piped stdin reads the whole stream; a bare positional is the
/// text; nothing on a TTY opens `$EDITOR` (git-commit style). `None` means the
/// editor aborted — nothing to capture.
fn resolve_capture_text(inline: Option<&str>) -> std::io::Result<Option<String>> {
    resolve_capture_text_with(inline, std::io::stdin().is_terminal(), read_stdin, || {
        crate::editor::edit("")
    })
}

/// The pure core of [`resolve_capture_text`], with the stdin-TTY check and the
/// stdin/editor readers injected so the resolution idiom is unit-testable.
fn resolve_capture_text_with(
    inline: Option<&str>,
    stdin_is_tty: bool,
    read_stdin: impl FnOnce() -> std::io::Result<String>,
    open_editor: impl FnOnce() -> std::io::Result<crate::editor::EditorOutcome>,
) -> std::io::Result<Option<String>> {
    match inline {
        // `engineer note capture -` — the explicit stdin form.
        Some("-") => Ok(Some(trim_trailing_newlines(&read_stdin()?))),
        Some(t) => Ok(Some(t.to_string())),
        None => {
            if !stdin_is_tty {
                // Piped with no positional (`git log -1 --format=%B | … capture`).
                Ok(Some(trim_trailing_newlines(&read_stdin()?)))
            } else {
                match open_editor()? {
                    crate::editor::EditorOutcome::Saved(body) => Ok(Some(body)),
                    crate::editor::EditorOutcome::Aborted => Ok(None),
                }
            }
        }
    }
}

fn read_stdin() -> std::io::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn trim_trailing_newlines(s: &str) -> String {
    s.trim_end_matches('\n').to_string()
}

// ------------------------------------------------------------------ reads

async fn list(
    api: &ApiClient,
    book: Option<String>,
    archived: Option<String>,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let book_id = match resolve_book(api, book.as_deref()).await? {
        Resolved::Id(id) => id,
        Resolved::Refuse(reason) => return Ok(Outcome::refuse(reason)),
    };
    let filters = NoteFilters {
        book_id,
        archived: normalize_archived(archived.as_deref()),
        ..Default::default()
    };
    match api.list_notes(&filters).await {
        Ok(list) => render_rows(&list.data, json, colored),
        Err(e) => read_refuse(e),
    }
}

async fn search(
    api: &ApiClient,
    args: SearchArgs,
    json: bool,
    colored: bool,
) -> Result<Outcome, ApiError> {
    let filters = NoteFilters {
        q: Some(args.query),
        archived: normalize_archived(args.archived.as_deref()),
        ..Default::default()
    };
    match api.list_notes(&filters).await {
        Ok(list) => render_rows(&list.data, json, colored),
        Err(e) => read_refuse(e),
    }
}

async fn show(api: &ApiClient, id: i64, json: bool, colored: bool) -> Result<Outcome, ApiError> {
    match api.get_note(id).await {
        Ok(note) if json => Ok(Outcome::ok(json_note_full(&note).to_string())),
        Ok(note) => Ok(Outcome::lines(show_lines(&note, colored))),
        Err(ApiError::Problem { status: 404, .. }) => {
            Ok(Outcome::refuse(format!("no note with id {id}")))
        }
        Err(e) => read_refuse(e),
    }
}

/// `list`/`search` share a rendering: a stable JSON array, or one piped-plain
/// row per note (the anchor read-back + age), or the calm "no notes" line.
fn render_rows(notes: &[Note], json: bool, colored: bool) -> Result<Outcome, ApiError> {
    if json {
        let arr: Vec<serde_json::Value> = notes.iter().map(json_note).collect();
        return Ok(Outcome::ok(serde_json::Value::Array(arr).to_string()));
    }
    if notes.is_empty() {
        return Ok(Outcome::ok(paint("no notes", COLOR_MUTED, colored)));
    }
    Ok(Outcome::lines(
        notes.iter().map(|n| note_row(n, colored)).collect(),
    ))
}

/// One list row: the derived title, the server's one-line `address_label`
/// read-back (`—` for a loose thought — never a blank column), and an age.
/// Archived notes wear a muted `· archived` tail so the `--archived` fold reads.
fn note_row(n: &Note, colored: bool) -> String {
    let anchor = anchor_label(n).unwrap_or_else(|| "—".into());
    let mut meta = anchor;
    if n.archived_at.is_some() {
        meta.push_str(" · archived");
    }
    let age = age_label(n);
    format!(
        "{}  {}{}",
        truncate(&n.title, 48),
        paint(&meta, COLOR_MUTED, colored),
        if age.is_empty() {
            String::new()
        } else {
            format!("  {}", paint(&age, COLOR_MUTED, colored))
        }
    )
}

/// The full read (`show`, piped-plain): the note's content verbatim, then each
/// citation's `address_label` on its own line — the anchor is the payoff line.
fn show_lines(n: &Note, colored: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let body = n.content.clone().unwrap_or_else(|| n.title.clone());
    out.extend(body.lines().map(str::to_string));
    let mut anchored = false;
    for c in &n.citations {
        if let Some(label) = &c.address_label {
            out.push(paint(label, COLOR_MUTED, colored));
            anchored = true;
        }
    }
    // A book link with no citation still names its place.
    if !anchored {
        if let Some(book) = &n.book_title {
            out.push(paint(book, COLOR_MUTED, colored));
        }
    }
    if n.archived_at.is_some() {
        out.push(paint("archived", COLOR_MUTED, colored));
    }
    out
}

// ------------------------------------------------------------- shared bits

/// The one-line anchor read-back: the first citation's server-rendered
/// `address_label` (`SICP · ch 3 · p.142`), falling back to the book title when
/// a link carries no citation. `None` is a loose thought.
fn anchor_label(n: &Note) -> Option<String> {
    n.citations
        .iter()
        .find_map(|c| c.address_label.clone())
        .or_else(|| n.book_title.clone())
}

/// The compact stable object for `list`/`search`/`capture` — id, title, the
/// anchor read-back, the book id, and the archived flag.
fn json_note(n: &Note) -> serde_json::Value {
    serde_json::json!({
        "id": n.id,
        "title": n.title,
        "anchor": anchor_label(n),
        "book_id": n.book_id,
        "archived": n.archived_at.is_some(),
    })
}

/// The fuller object for `show` — the compact fields plus the full content and
/// every citation.
fn json_note_full(n: &Note) -> serde_json::Value {
    let citations: Vec<serde_json::Value> = n
        .citations
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "page": c.page,
                "address_label": c.address_label,
            })
        })
        .collect();
    serde_json::json!({
        "id": n.id,
        "title": n.title,
        "content": n.content,
        "anchor": anchor_label(n),
        "book_id": n.book_id,
        "book_title": n.book_title,
        "archived": n.archived_at.is_some(),
        "citations": citations,
    })
}

/// `--archived all` → both, `--archived only` (also `true`/`archived`) →
/// archived alone; anything else (or absent) reads active-only, the browse
/// default. Maps to the server's `archived=` param the `NoteFilters` carries.
fn normalize_archived(mode: Option<&str>) -> Option<String> {
    match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("all") => Some("all".into()),
        Some("only" | "true" | "archived") => Some("true".into()),
        _ => None,
    }
}

/// A muted relative age from the note's `updated_at` — `2h`, `1d`, `2w`, `1mo`.
fn age_label(n: &Note) -> String {
    let Some(ts) = n.updated_at else {
        return String::new();
    };
    let secs = (jiff::Timestamp::now().as_second() - ts.as_second()).max(0);
    if secs < 3600 {
        format!("{}m", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 7 * 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs < 30 * 86_400 {
        format!("{}w", secs / (7 * 86_400))
    } else {
        format!("{}mo", secs / (30 * 86_400))
    }
}

/// Clip a title to `max` characters with an ellipsis, so a long first line never
/// blows out a row.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// The honest one-line reason for a failed read — offline is loud (there is no
/// note cache to fall back on), a `404` is handled by the caller, otherwise the
/// server's own problem text; auth and the unclassifiable rest propagate.
fn read_refuse(e: ApiError) -> Result<Outcome, ApiError> {
    match e {
        ApiError::Transport(_) => Ok(Outcome::refuse(
            "offline — notes reads need the server; retry online",
        )),
        ApiError::Problem { detail, .. } if !detail.is_empty() => Ok(Outcome::refuse(detail)),
        ApiError::Problem { title, .. } => Ok(Outcome::refuse(title)),
        e => Err(e),
    }
}

/// A failed write's reason — the server's problem text (a queued write never
/// reaches here; `create_note` only surfaces a live problem or an auth error).
fn write_refuse(e: ApiError) -> Result<Outcome, ApiError> {
    match e {
        ApiError::Problem { detail, .. } if !detail.is_empty() => Ok(Outcome::refuse(detail)),
        ApiError::Problem { title, .. } => Ok(Outcome::refuse(title)),
        e => Err(e),
    }
}

const COLOR_OK: u8 = 108;
const COLOR_MUTED: u8 = 244;

fn paint(s: &str, color: u8, colored: bool) -> String {
    if colored {
        format!("\x1b[38;5;{color}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    fn dead_api() -> ApiClient {
        ApiClient::with_token(Url::parse("http://127.0.0.1:1/").unwrap(), "tok".into())
    }

    /// A per-test scratch dir so the queue never touches the shared XDG state.
    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "engineer-note-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn queued_at(api: &ApiClient, dir: &std::path::Path) -> QueuedClient {
        QueuedClient::with_paths(
            api,
            crate::queue::QueueStore::at(dir.join("queue.json")),
            dir.join("timer-cache.json"),
        )
    }

    /// `dispatch` with an isolated, empty queue — what most tests need.
    async fn run_dispatch(
        api: &ApiClient,
        cmd: Option<NoteCmd>,
        json: bool,
    ) -> Result<Outcome, ApiError> {
        let dir = scratch();
        let queued = queued_at(api, &dir);
        dispatch(api, &queued, cmd, json, false).await
    }

    // ---------------------------------------------------------- capture

    #[tokio::test]
    async fn capture_creates_with_the_book_and_page_anchor_and_reads_the_address_label_back() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/books"))
            .and(query_param("q", "sicp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": 3, "title": "SICP", "status": "reading" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The derived title is the first line; the full text lands in content;
        // the book+page becomes a single-page anchor.
        Mock::given(method("POST"))
            .and(path("/api/v1/notes"))
            .and(body_partial_json(serde_json::json!({
                "note": {
                    "title": "MVCC keeps one version per read-tx",
                    "content": "MVCC keeps one version per read-tx\nsecond line",
                    "book_id": 3,
                    "anchors": [{ "page": 142 }]
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 9, "title": "MVCC keeps one version per read-tx", "book_id": 3,
                "book_title": "SICP", "book_linked": true,
                "citations": [{ "id": 1, "page": 142, "address_label": "SICP · ch 3 · p.142" }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cmd = Some(NoteCmd::Capture(CaptureArgs {
            text: Some("MVCC keeps one version per read-tx\nsecond line".into()),
            book: Some("sicp".into()),
            page: Some(142),
        }));
        let out = run_dispatch(&client(&server), cmd, false).await.unwrap();
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("captured"), "{}", out.out[0]);
        // The one-line anchor read-back comes from the server's address_label.
        assert!(out.out[0].contains("SICP · ch 3 · p.142"), "{}", out.out[0]);
    }

    #[tokio::test]
    async fn capture_json_carries_the_stable_object() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/notes"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 9, "title": "a loose thought", "citations": []
            })))
            .mount(&server)
            .await;

        let cmd = Some(NoteCmd::Capture(CaptureArgs {
            text: Some("a loose thought".into()),
            ..Default::default()
        }));
        let out = run_dispatch(&client(&server), cmd, true).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
        assert_eq!(v["id"], 9);
        assert_eq!(v["title"], "a loose thought");
        assert_eq!(v["anchor"], serde_json::Value::Null, "a loose note");
        assert_eq!(v["archived"], false);
    }

    #[tokio::test]
    async fn empty_capture_refuses() {
        let out = run_dispatch(
            &dead_api(),
            Some(NoteCmd::Capture(CaptureArgs {
                text: Some("   \n ".into()),
                ..Default::default()
            })),
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("empty"), "{}", out.err[0]);
    }

    #[tokio::test]
    async fn offline_loose_capture_enqueues_and_marks_it_queued() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let cmd = Some(NoteCmd::Capture(CaptureArgs {
            text: Some("teach CAP via a live partition demo".into()),
            ..Default::default()
        }));
        let out = dispatch(&api, &queued, cmd, false, false).await.unwrap();
        assert_eq!(out.code, 0, "a loose capture is never refused offline");
        assert!(out.out[0].contains("captured"), "{}", out.out[0]);
        assert!(out.out[0].contains("queued (offline)"), "{}", out.out[0]);

        let intents = crate::queue::QueueStore::at(dir.join("queue.json"))
            .pending()
            .unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].kind.word(), "capture");
        assert_eq!(intents[0].stream, "note");
    }

    #[tokio::test]
    async fn offline_book_capture_refuses_it_cannot_resolve() {
        let api = dead_api();
        let dir = scratch();
        let queued = queued_at(&api, &dir);

        let cmd = Some(NoteCmd::Capture(CaptureArgs {
            text: Some("MVCC".into()),
            book: Some("sicp".into()),
            page: Some(142),
        }));
        let out = dispatch(&api, &queued, cmd, false, false).await.unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("can't resolve book \"sicp\""));
        assert_eq!(
            queued.queue_summary().depth,
            0,
            "an anchored offline capture enqueues nothing"
        );
    }

    // ------------------------------------------------- stdin / editor idiom

    #[test]
    fn resolve_text_reads_stdin_on_dash_and_when_piped() {
        // Explicit `-`.
        let got = resolve_capture_text_with(
            Some("-"),
            true,
            || Ok("piped body\n".into()),
            || panic!("editor must not open for `-`"),
        )
        .unwrap();
        assert_eq!(
            got.as_deref(),
            Some("piped body"),
            "trailing newline trimmed"
        );

        // No positional, stdin is not a TTY (piped).
        let got = resolve_capture_text_with(
            None,
            false,
            || Ok("from a pipe".into()),
            || panic!("editor must not open when piped"),
        )
        .unwrap();
        assert_eq!(got.as_deref(), Some("from a pipe"));
    }

    #[test]
    fn resolve_text_prefers_the_inline_positional() {
        let got = resolve_capture_text_with(
            Some("inline thought"),
            false,
            || panic!("stdin must not be read for an inline positional"),
            || panic!("editor must not open"),
        )
        .unwrap();
        assert_eq!(got.as_deref(), Some("inline thought"));
    }

    #[test]
    fn resolve_text_opens_the_editor_on_a_tty_and_an_abort_is_nothing() {
        let saved = resolve_capture_text_with(
            None,
            true,
            || panic!("stdin must not be read on a TTY"),
            || Ok(crate::editor::EditorOutcome::Saved("from $EDITOR".into())),
        )
        .unwrap();
        assert_eq!(saved.as_deref(), Some("from $EDITOR"));

        let aborted = resolve_capture_text_with(
            None,
            true,
            || panic!("stdin must not be read on a TTY"),
            || Ok(crate::editor::EditorOutcome::Aborted),
        )
        .unwrap();
        assert!(aborted.is_none(), "an aborted editor captures nothing");
    }

    // ------------------------------------------------------------- reads

    /// A two-note list payload: one anchored, one loose.
    fn two_notes() -> serde_json::Value {
        serde_json::json!({
            "data": [
                { "id": 9, "title": "MVCC keeps one version per read-tx",
                  "book_id": 3, "book_title": "SICP",
                  "citations": [{ "id": 1, "page": 142, "address_label": "SICP · ch 3 · p.142" }] },
                { "id": 10, "title": "teach CAP via a live partition demo", "citations": [] }
            ]
        })
    }

    #[tokio::test]
    async fn list_default_reads_active_and_prints_the_anchor_readback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_notes()))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), None, false).await.unwrap();
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("MVCC keeps one version"));
        assert!(
            out.out[0].contains("SICP · ch 3 · p.142"),
            "the address_label reads back: {}",
            out.out[0]
        );
        // A loose thought shows the em dash, never a blank column.
        assert!(out.out[1].contains('—'), "{}", out.out[1]);
    }

    #[tokio::test]
    async fn list_json_is_a_stable_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_notes()))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), None, true).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 9);
        assert_eq!(arr[0]["anchor"], "SICP · ch 3 · p.142");
        assert_eq!(arr[1]["anchor"], serde_json::Value::Null, "the loose note");
    }

    #[tokio::test]
    async fn list_book_filter_resolves_the_book_and_sends_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/books"))
            .and(query_param("q", "sicp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": 3, "title": "SICP", "status": "reading" }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes"))
            .and(query_param("book_id", "3"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let cmd = Some(NoteCmd::List(ListArgs {
            book: Some("sicp".into()),
            archived: None,
        }));
        let out = run_dispatch(&client(&server), cmd, false).await.unwrap();
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("no notes"), "{}", out.out[0]);
    }

    #[tokio::test]
    async fn list_archived_all_sends_the_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes"))
            .and(query_param("archived", "all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_notes()))
            .expect(1)
            .mount(&server)
            .await;

        let cmd = Some(NoteCmd::List(ListArgs {
            book: None,
            archived: Some("all".into()),
        }));
        let out = run_dispatch(&client(&server), cmd, false).await.unwrap();
        assert_eq!(out.code, 0);
    }

    #[tokio::test]
    async fn search_sends_the_q_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes"))
            .and(query_param("q", "mvcc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_notes()))
            .expect(1)
            .mount(&server)
            .await;

        let cmd = Some(NoteCmd::Search(SearchArgs {
            query: "mvcc".into(),
            archived: None,
        }));
        let out = run_dispatch(&client(&server), cmd, false).await.unwrap();
        assert_eq!(out.code, 0);
        assert!(out.out[0].contains("MVCC keeps one version"));
    }

    #[tokio::test]
    async fn show_prints_the_content_then_the_anchor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes/9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 9, "title": "MVCC keeps one version per read-tx",
                "content": "MVCC keeps one version per read-tx\nreaders never block writers",
                "book_id": 3, "book_title": "SICP",
                "citations": [{ "id": 1, "page": 142, "address_label": "SICP · ch 3 · p.142" }]
            })))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), Some(NoteCmd::Show { id: 9 }), false)
            .await
            .unwrap();
        assert_eq!(out.code, 0);
        assert_eq!(out.out[0], "MVCC keeps one version per read-tx");
        assert_eq!(out.out[1], "readers never block writers");
        assert!(
            out.out.iter().any(|l| l.contains("SICP · ch 3 · p.142")),
            "the anchor line: {:?}",
            out.out
        );
    }

    #[tokio::test]
    async fn show_json_carries_content_and_citations() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes/9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 9, "title": "MVCC", "content": "the full body",
                "citations": [{ "id": 1, "page": 142, "address_label": "SICP · ch 3 · p.142" }]
            })))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), Some(NoteCmd::Show { id: 9 }), true)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.out[0]).unwrap();
        assert_eq!(v["content"], "the full body");
        assert_eq!(v["citations"][0]["address_label"], "SICP · ch 3 · p.142");
    }

    #[tokio::test]
    async fn show_a_missing_note_refuses_with_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/notes/404"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "title": "Not Found", "status": 404
            })))
            .mount(&server)
            .await;

        let out = run_dispatch(&client(&server), Some(NoteCmd::Show { id: 404 }), false)
            .await
            .unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("no note with id 404"), "{}", out.err[0]);
    }

    #[tokio::test]
    async fn reads_offline_refuse_honestly_there_is_no_note_cache() {
        let out = run_dispatch(&dead_api(), None, false).await.unwrap();
        assert_eq!(out.code, 1);
        assert!(out.err[0].contains("offline"), "{}", out.err[0]);
        assert!(out.err[0].contains("notes reads need the server"));
    }

    #[test]
    fn normalize_archived_maps_the_modes() {
        assert_eq!(normalize_archived(None), None);
        assert_eq!(normalize_archived(Some("all")).as_deref(), Some("all"));
        assert_eq!(normalize_archived(Some("only")).as_deref(), Some("true"));
        assert_eq!(
            normalize_archived(Some("ARCHIVED")).as_deref(),
            Some("true")
        );
        assert_eq!(normalize_archived(Some("nonsense")), None, "active default");
    }
}
