use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct List<T> {
    pub data: Vec<T>,
    #[serde(default)]
    pub meta: Meta,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Meta {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub per_page: u32,
    #[serde(default)]
    pub total: u32,
}
