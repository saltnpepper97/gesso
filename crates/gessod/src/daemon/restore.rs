use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gesso_core::{scale_image, Colour, RenderEngine, Target};
use gesso_core::decode::{decode, Decoded};
use gesso_core::render::OldSnapshot;
use gesso_ipc::protocol as ipc;

use crate::daemon::gif_player::GifPlayer;
use crate::daemon::ipc::to_scale_mode;
use crate::daemon::persist::resolve_image_path;
use crate::daemon::snapshot::snapshot_pixels_for_output;
use crate::daemon::transitions::to_core_transition_persisted;
use crate::daemon::types::{PersistedSet, PersistedState, PersistedTarget};

pub fn apply_persisted_state(
    eng:      &mut RenderEngine,
    active:   &mut HashSet<String>,
    current:  &mut HashMap<String, ipc::CurrentTarget>,
    last_set: &mut HashMap<String, PersistedSet>,
    gifs:     &mut HashMap<String, GifPlayer>,
    outputs:  &[gesso_wl::OutputInfo],
    st:       PersistedState,
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
                gifs.remove(&po.name);
                current.insert(po.name.clone(), ipc::CurrentTarget::Unset);
                active.remove(&po.name);
            }

            PersistedTarget::Colour { r, g, b } => {
                gifs.remove(&po.name);

                let col = Colour { r: *r, g: *g, b: *b };
                let tr  = to_core_transition_persisted(&po.set.transition);

                if matches!(tr, gesso_core::Transition::None) {
                    eng.set_now(&po.name, Target::Colour(col))?;
                } else {
                    eng.set_with_transition_from(
                        &po.name,
                        OldSnapshot::Image(from),
                        Target::Colour(col),
                        tr,
                    )?;
                }

                active.insert(po.name.clone());
                current.insert(
                    po.name.clone(),
                    ipc::CurrentTarget::Colour(ipc::Rgb { r: *r, g: *g, b: *b }),
                );
            }

            PersistedTarget::ImagePath { path } => {
                let Some(resolved) = resolve_image_path(path) else {
                    gifs.remove(&po.name);
                    current.insert(po.name.clone(), ipc::CurrentTarget::Unset);
                    active.remove(&po.name);
                    last_set.insert(po.name.clone(), po.set.clone());
                    continue;
                };

                let mode   = po.set.mode.unwrap_or(ipc::Mode::Fill);
                let bg     = po.set.bg_colour.unwrap_or(ipc::Rgb { r: 0, g: 0, b: 0 });
                let bg_col = Colour { r: bg.r, g: bg.g, b: bg.b };
                let scale  = to_scale_mode(mode);
                let tr     = to_core_transition_persisted(&po.set.transition);

                let decoded = decode(&resolved)
                    .map_err(|e| anyhow::anyhow!("decode failed: {e}"))?;

                gifs.remove(&po.name);

                match decoded {
                    Decoded::Still(img) => {
                        let pixels = scale_image(&img, out.width, out.height, scale, bg_col);
                        let target = Target::image(
                            out.width,
                            out.height,
                            out.width as usize * 4,
                            pixels,
                        );

                        if matches!(tr, gesso_core::Transition::None) {
                            eng.set_now(&po.name, target)?;
                        } else {
                            eng.set_with_transition_from(
                                &po.name,
                                OldSnapshot::Image(from),
                                target,
                                tr,
                            )?;
                        }

                        active.insert(po.name.clone());
                        current.insert(
                            po.name.clone(),
                            ipc::CurrentTarget::ImagePath(path.clone()),
                        );
                    }

                    Decoded::Animated(anim) => {
                        // Show first frame with transition if persisted.
                        let pixels = scale_image(
                            &anim.first_frame,
                            out.width,
                            out.height,
                            scale,
                            bg_col,
                        );
                        let target0 = Target::image(
                            out.width,
                            out.height,
                            out.width as usize * 4,
                            pixels,
                        );

                        if matches!(tr, gesso_core::Transition::None) {
                            eng.set_now(&po.name, target0)?;
                        } else {
                            eng.set_with_transition_from(
                                &po.name,
                                OldSnapshot::Image(from),
                                target0,
                                tr,
                            )?;
                        }

                        // Install the player. run loop skips tick() while
                        // is_transitioning() so frames won't race the intro.
                        let now = Instant::now();
                        match GifPlayer::new(
                            anim,
                            out.width,
                            out.height,
                            scale,
                            bg_col,
                            None, // loop forever
                            now,
                        ) {
                            Ok(player) => {
                                gifs.insert(po.name.clone(), player);
                            }
                            Err(e) => {
                                eventline::warn!(
                                    "restore: animation player init failed for {}: {e}",
                                    po.name
                                );
                            }
                        }

                        active.insert(po.name.clone());
                        current.insert(
                            po.name.clone(),
                            ipc::CurrentTarget::ImagePath(path.clone()),
                        );
                    }
                }
            }
        }

        last_set.insert(po.name.clone(), po.set.clone());
    }

    Ok(())
}
