use gesso_core::{decode_image, scale_image, Colour, RenderEngine, Target};
use gesso_core::render::OldSnapshot;
use gesso_ipc::protocol as ipc;

use crate::daemon::persist::resolve_image_path;
use crate::daemon::snapshot::snapshot_pixels_for_output;
use crate::daemon::transitions::to_core_transition_persisted;
use crate::daemon::types::{PersistedSet, PersistedState, PersistedTarget};

pub fn apply_persisted_state(
    eng: &mut RenderEngine,
    active: &mut std::collections::HashSet<String>,
    current: &mut std::collections::HashMap<String, ipc::CurrentTarget>,
    last_set: &mut std::collections::HashMap<String, PersistedSet>,
    outputs: &[gesso_wl::OutputInfo],
    st: PersistedState,
) -> anyhow::Result<()> {
    for po in st.outputs {
        let Some(out) = outputs.iter().find(|o| o.name == po.name) else {
            last_set.insert(po.name.clone(), po.set.clone());
            continue;
        };

        let prev = last_set.get(&po.name).cloned();
        let from = snapshot_pixels_for_output(out, prev.as_ref());

        match &po.set.target {
            PersistedTarget::Unset => {
                current.insert(po.name.clone(), ipc::CurrentTarget::Unset);
                active.remove(&po.name);
            }

            PersistedTarget::Colour { r, g, b } => {
                let col = Colour { r: *r, g: *g, b: *b };
                let tr = to_core_transition_persisted(&po.set.transition);

                if matches!(tr, gesso_core::Transition::None) {
                    eng.set_now(&po.name, Target::Colour(col))?;
                } else {
                    eng.set_with_transition_from(&po.name, OldSnapshot::Image(from), Target::Colour(col), tr)?;
                }

                active.insert(po.name.clone());
                current.insert(
                    po.name.clone(),
                    ipc::CurrentTarget::Colour(ipc::Rgb { r: *r, g: *g, b: *b }),
                );
            }

            PersistedTarget::ImagePath { path } => {
                let Some(resolved) = resolve_image_path(path) else {
                    current.insert(po.name.clone(), ipc::CurrentTarget::Unset);
                    active.remove(&po.name);
                    last_set.insert(po.name.clone(), po.set.clone());
                    continue;
                };

                let decoded = decode_image(&resolved)?;
                let mode = po.set.mode.unwrap_or(ipc::Mode::Fill);
                let bg = po.set.bg_colour.unwrap_or(ipc::Rgb { r: 0, g: 0, b: 0 });

                let pixels = scale_image(
                    &decoded,
                    out.width,
                    out.height,
                    to_scale_mode(mode),
                    Colour { r: bg.r, g: bg.g, b: bg.b },
                );

                let target = Target::image(out.width, out.height, out.width as usize * 4, pixels);
                let tr = to_core_transition_persisted(&po.set.transition);

                if matches!(tr, gesso_core::Transition::None) {
                    eng.set_now(&po.name, target)?;
                } else {
                    eng.set_with_transition_from(&po.name, OldSnapshot::Image(from), target, tr)?;
                }

                active.insert(po.name.clone());
                current.insert(po.name.clone(), ipc::CurrentTarget::ImagePath(path.clone()));
            }
        }

        last_set.insert(po.name.clone(), po.set.clone());
    }

    Ok(())
}

fn to_scale_mode(m: ipc::Mode) -> gesso_core::ScaleMode {
    match m {
        ipc::Mode::Fill => gesso_core::ScaleMode::Fill,
        ipc::Mode::Fit => gesso_core::ScaleMode::Fit,
        ipc::Mode::Stretch => gesso_core::ScaleMode::Stretch,
        ipc::Mode::Center => gesso_core::ScaleMode::Center,
        ipc::Mode::Tile => gesso_core::ScaleMode::Tile,
    }
}
