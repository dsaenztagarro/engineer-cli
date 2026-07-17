//! Notes — paper-first study notes, optionally anchored to places in a book.

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

// API model: fields mirror the wire format; the UI reads only a subset today.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Citation {
    pub id: i64,
    #[serde(default)]
    pub book_edition_id: Option<i64>,
    #[serde(default)]
    pub book_chapter_id: Option<i64>,
    #[serde(default)]
    pub book_section_id: Option<i64>,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub address_label: Option<String>,
}

// API model: fields mirror the wire format; the UI reads only a subset today.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Note {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub book_linked: bool,
    #[serde(default)]
    pub book_title: Option<String>,
    #[serde(default)]
    pub domain_id: Option<i64>,
    #[serde(default)]
    pub subdomain_id: Option<i64>,
    #[serde(default)]
    pub domain_name: Option<String>,
    #[serde(default)]
    pub subdomain_name: Option<String>,
    #[serde(default)]
    pub has_physical_paper: bool,
    #[serde(default)]
    pub paper_location_label: Option<String>,
    #[serde(default)]
    pub archived_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub citations: Vec<Citation>,
    #[serde(default)]
    pub updated_at: Option<jiff::Timestamp>,
}

/// A book anchor to build citations from. Needs at least one of chapter/section/page.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edition_ids: Vec<i64>,
}

// `Clone`/`PartialEq`/`Deserialize` beyond the base `Serialize` so a queued
// capture can ride an `IntentKind::NoteCreate { body: NoteInput }` and replay it
// verbatim (mirrors `ActivityCreate`). The `skip_serializing_if` attributes are
// Serialize-only; on the way back, the `Option`/`Vec` fields default naturally.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteInput {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_physical_paper: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_notebook: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_page: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_ids: Vec<i64>,
    // Omitted on a partial update leaves existing anchors untouched; sent replaces them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchors: Option<Vec<Anchor>>,
}

#[derive(Debug, Default, Clone)]
pub struct NoteFilters {
    pub book_id: Option<i64>,
    pub domain_id: Option<i64>,
    pub subdomain_id: Option<i64>,
    pub section_id: Option<i64>,
    pub has_physical_paper: bool,
    pub q: Option<String>,
    /// "true" for archived only, "all" for both; None = active only.
    pub archived: Option<String>,
}

/// Editions/chapters/sections a note anchor can point at — `GET /books/:id/anchor_data`.
// `editions` mirrors the wire format; the picker anchors over chapters/sections.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AnchorData {
    #[serde(default)]
    pub editions: Vec<AnchorEdition>,
    #[serde(default)]
    pub chapters: Vec<AnchorChapter>,
}

// API model: the edition list mirrors the wire format; unused by the picker today.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AnchorEdition {
    pub id: i64,
    pub label: String,
    #[serde(default)]
    pub canonical: bool,
    #[serde(default)]
    pub reflowable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnchorChapter {
    pub id: i64,
    #[serde(default)]
    pub number: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sections: Vec<AnchorSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnchorSection {
    pub id: i64,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Serialize)]
struct NoteBody<'a> {
    note: &'a NoteInput,
}

impl ApiClient {
    pub async fn list_notes(&self, f: &NoteFilters) -> Result<List<Note>, ApiError> {
        let mut q: Vec<(&str, String)> = vec![];
        if let Some(id) = f.book_id {
            q.push(("book_id", id.to_string()));
        }
        if let Some(id) = f.domain_id {
            q.push(("domain_id", id.to_string()));
        }
        if let Some(id) = f.subdomain_id {
            q.push(("subdomain_id", id.to_string()));
        }
        if let Some(id) = f.section_id {
            q.push(("section_id", id.to_string()));
        }
        if f.has_physical_paper {
            q.push(("has_physical_paper", "true".into()));
        }
        if let Some(s) = &f.q {
            if !s.is_empty() {
                q.push(("q", s.clone()));
            }
        }
        if let Some(a) = &f.archived {
            q.push(("archived", a.clone()));
        }
        self.get("/api/v1/notes", &q).await
    }

    pub async fn get_note(&self, id: i64) -> Result<Note, ApiError> {
        self.get(&format!("/api/v1/notes/{id}"), &[]).await
    }

    pub async fn create_note(&self, body: &NoteInput) -> Result<Note, ApiError> {
        self.post("/api/v1/notes", &NoteBody { note: body }).await
    }

    pub async fn update_note(&self, id: i64, body: &NoteInput) -> Result<Note, ApiError> {
        self.patch(&format!("/api/v1/notes/{id}"), &NoteBody { note: body })
            .await
    }

    pub async fn delete_note(&self, id: i64) -> Result<(), ApiError> {
        self.delete(&format!("/api/v1/notes/{id}")).await
    }

    pub async fn unlink_note(&self, id: i64) -> Result<Note, ApiError> {
        self.patch_empty(&format!("/api/v1/notes/{id}/unlink"))
            .await
    }

    pub async fn archive_note(&self, id: i64) -> Result<Note, ApiError> {
        self.patch_empty(&format!("/api/v1/notes/{id}/archive"))
            .await
    }

    pub async fn unarchive_note(&self, id: i64) -> Result<Note, ApiError> {
        self.patch_empty(&format!("/api/v1/notes/{id}/unarchive"))
            .await
    }

    pub async fn book_anchor_data(&self, book_id: i64) -> Result<AnchorData, ApiError> {
        self.get(&format!("/api/v1/books/{book_id}/anchor_data"), &[])
            .await
    }
}

/// Longest first-line slice we lift into a note's title. The full text always
/// lands in `content`, so truncating the title never loses input.
pub(crate) const TITLE_MAX: usize = 120;

/// Split a captured thought into `(title, content)`: the title is the first
/// non-empty line (clipped to `TITLE_MAX`), and the full text is kept verbatim
/// in `content` so nothing is lost. The server's note model requires a title;
/// capture is content-first, so we derive one.
///
/// One spelling of the rule, home in the notes domain and reused by both note
/// surfaces: the quick-capture overlay (`src/app/capture.rs`) and the headless
/// `engineer note capture` (`src/note_cli.rs`).
pub(crate) fn derive_title_content(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    let first = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let title: String = first.chars().take(TITLE_MAX).collect();
    (title, Some(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> ApiClient {
        ApiClient::with_token(Url::parse(&server.uri()).unwrap(), "tok".into())
    }

    #[tokio::test]
    async fn create_note_wraps_in_note_key_with_anchors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/notes"))
            .and(body_partial_json(serde_json::json!({
                "note": { "title": "Blocks", "book_id": 3, "anchors": [{ "page": 12 }] }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "title": "Blocks", "book_id": 3, "book_linked": true, "citations": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let body = NoteInput {
            title: "Blocks".into(),
            book_id: Some(3),
            anchors: Some(vec![Anchor {
                page: Some(12),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let note = client(&server).create_note(&body).await.unwrap();
        assert_eq!(note.id, 1);
        assert!(note.book_linked);
    }

    #[test]
    fn derive_title_content_lifts_first_line_and_keeps_full_text() {
        let (title, content) = derive_title_content("closures are objects\n\nthe env model\n");
        assert_eq!(title, "closures are objects");
        assert_eq!(
            content.as_deref(),
            Some("closures are objects\n\nthe env model")
        );
    }

    #[test]
    fn derive_title_content_empty_is_empty() {
        let (title, content) = derive_title_content("   \n  ");
        assert!(title.is_empty());
        assert!(content.is_none());
    }

    #[tokio::test]
    async fn book_anchor_data_reads_chapters_and_sections() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/books/11/anchor_data"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "editions": [{ "id": 1, "label": "1st", "canonical": true }],
                "chapters": [{
                    "id": 3, "number": 3, "title": "Modularity, Objects, and State",
                    "sections": [{ "id": 32, "number": "3.2", "title": "The Environment Model" }]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let data = client(&server).book_anchor_data(11).await.unwrap();
        assert_eq!(data.chapters.len(), 1);
        assert_eq!(data.chapters[0].id, 3);
        assert_eq!(data.chapters[0].sections[0].id, 32);
        assert_eq!(data.chapters[0].sections[0].number.as_deref(), Some("3.2"));
    }

    #[tokio::test]
    async fn update_note_with_chapter_section_anchor_sends_the_richer_body() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/7"))
            .and(body_partial_json(serde_json::json!({
                "note": { "anchors": [{ "chapter_id": 3, "section_id": 32 }] }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "title": "MVCC", "book_id": 11
            })))
            .expect(1)
            .mount(&server)
            .await;

        let body = NoteInput {
            title: "MVCC".into(),
            book_id: Some(11),
            anchors: Some(vec![Anchor {
                chapter_id: Some(3),
                section_id: Some(32),
                ..Default::default()
            }]),
            ..Default::default()
        };
        client(&server).update_note(7, &body).await.unwrap();
    }

    #[tokio::test]
    async fn delete_note_hits_the_member_route() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/notes/9"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).delete_note(9).await.unwrap();
    }

    #[tokio::test]
    async fn unlink_note_patches_the_member_route_and_keeps_the_note() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/4/unlink"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 4, "title": "kept", "book_id": null, "book_linked": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let note = client(&server).unlink_note(4).await.unwrap();
        assert_eq!(note.id, 4);
        assert!(!note.book_linked);
        assert!(note.book_id.is_none());
    }

    #[tokio::test]
    async fn archive_note_patches_member_route_without_body() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/notes/5/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 5, "title": "T", "archived_at": "2026-07-01T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let note = client(&server).archive_note(5).await.unwrap();
        assert!(note.archived_at.is_some());
    }
}
