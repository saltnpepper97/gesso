// Author: Dustin Pilgrim
// License: MIT

use serde::{Deserialize, Serialize};

use crate::spec::Spec;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Apply { spec: Spec },
    Stop,
    Unset { output: Option<String> },
    Status,
    Doctor,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Status { current: Option<CurrentStatus> },
    Doctor { checks: Vec<DoctorCheck> },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CurrentStatus {
    pub spec: Spec,
    pub running: bool,
    pub note: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
