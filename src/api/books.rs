use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, List};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BookStatus {
    Unread,
    Reading,
    Completed,
    OnHold,
    Abandoned,
}

impl BookStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Reading => "reading",
            Self::Completed => "completed",
            Self::OnHold => "on hold",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Unread => Self::Reading,
            Self::Reading => Self::Completed,
            Self::Completed => Self::OnHold,
            Self::OnHold => Self::Abandoned,
            Self::Abandoned => Self::Unread,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Book {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    pub status: BookStatus,
    #[serde(default)]
    pub current_page: Option<u32>,
    #[serde(default)]
    pub page_count: Option<u32>,
    #[serde(default)]
    pub current_chapter_id: Option<i64>,
    #[serde(default)]
    pub progress_percent: Option<f32>,
    #[serde(default)]
    pub chapters_total: Option<u32>,
    #[serde(default)]
    pub current_chapter_number: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookChapter {
    pub id: i64,
    pub number: u32,
    pub title: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub skipped: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct BookUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<BookStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_chapter_id: Option<i64>,
}

#[derive(Serialize)]
struct BookUpdateBody<'a> {
    book: &'a BookUpdate,
}

impl ApiClient {
    pub async fn list_books(&self, status: Option<BookStatus>, q: Option<&str>) -> Result<List<Book>, ApiError> {
        let mut query = vec![];
        if let Some(s) = status {
            query.push(("status", s.label().replace(' ', "_")));
        }
        if let Some(q) = q {
            if !q.is_empty() {
                query.push(("q", q.to_string()));
            }
        }
        self.get("/api/v1/books", &query).await
    }

    pub async fn get_book(&self, id: i64) -> Result<Book, ApiError> {
        self.get(&format!("/api/v1/books/{id}"), &[]).await
    }

    pub async fn list_chapters(&self, book_id: i64) -> Result<List<BookChapter>, ApiError> {
        self.get(&format!("/api/v1/books/{book_id}/chapters"), &[]).await
    }

    pub async fn update_book(&self, id: i64, body: &BookUpdate) -> Result<Book, ApiError> {
        self.patch(&format!("/api/v1/books/{id}"), &BookUpdateBody { book: body }).await
    }
}
