// Author: Dustin Pilgrim
// License: MIT

pub mod decode;
pub mod mem;
pub mod model;
pub mod render;
pub mod hex;
pub mod paths;

pub use decode::{DecodedImage, decode_image, DecodeError};
pub use model::{
    Colour,
    OutputDesc,
    OutputSel,
    SetTarget,
    SetRequest,
    State,
    SavedTarget,
};
pub use render::{
    Surface,
    Transition,
    WaveDir,
    RenderCtx,
    render_transition,
};
pub use render::{RenderEngine, Target};
pub use render::{scale_image, ScaleMode};
