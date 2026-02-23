use serde::{Deserialize, Serialize};

use crate::model::output::OutputSel;
use crate::model::colour::Colour;
use crate::render::transition::Transition; 

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SetTarget {
    ImagePath(String),
    Colour(Colour),
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRequest {
    pub outputs: OutputSel,
    pub target: SetTarget,
    pub transition: Transition,
    pub remember: bool,
}
