use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct List<T> {
    pub data: Vec<T>,
    // Pagination metadata is parsed but not yet surfaced in the UI.
    #[allow(dead_code)]
    #[serde(default)]
    pub meta: Meta,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone, Default)]
pub struct Meta {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub per_page: u32,
    #[serde(default)]
    pub total: u32,
}
