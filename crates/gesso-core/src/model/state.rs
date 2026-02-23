use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::Colour;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SavedTarget {
    ImagePath(String),
    Colour(Colour),
    Unset,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    pub per_output: HashMap<String, SavedTarget>,
}
