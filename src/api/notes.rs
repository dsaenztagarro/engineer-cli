//! Notes — paper-first study notes, optionally anchored to places in a book.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

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

#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Default, Clone, Serialize)]
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

#[derive(Debug, Default, Serialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct AnchorData {
    #[serde(default)]
    pub editions: Vec<AnchorEdition>,
    #[serde(default)]
    pub chapters: Vec<AnchorChapter>,
}

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
