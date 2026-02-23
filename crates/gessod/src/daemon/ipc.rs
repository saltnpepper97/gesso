use std::collections::{HashMap, HashSet};

use eventline::scope;

use gesso_core::{
    decode_image, scale_image, Colour, RenderEngine, ScaleMode, Target,
    Transition as CoreTransition,
};
use gesso_core::render::OldSnapshot;
use gesso_ipc::protocol as ipc;
use gesso_wl::WlBackend;

use crate::daemon::persist::{resolve_image_path, save_state};
use crate::daemon::snapshot::snapshot_pixels_for_output;
use crate::daemon::transitions::{
    ipc_transition_from_persisted, persisted_transition_from_ipc, to_core_transition,
};
use crate::daemon::types::{PersistedSet, PersistedTarget, PersistedTransition};

pub fn handle_request(
    eng: &mut RenderEngine,
    wl: &mut WlBackend,
    outputs: &[gesso_wl::OutputInfo],
    active: &mut HashSet<String>,
    current: &mut HashMap<String, ipc::CurrentTarget>,
    last_set: &mut HashMap<String, PersistedSet>,
    req: ipc::Request,
    quitting: &mut bool,
) -> ipc::Response {
    scope!("gessod.ipc.handle", {
        match req {
            ipc::Request::Outputs => {
                let mut list = outputs
                    .iter()
                    .map(|o| ipc::OutputInfo {
                        name: o.name.clone(),
                        width: o.width,
                        height: o.height,
                        scale: o.scale,
                    })
                    .collect::<Vec<_>>();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                ipc::Response::Outputs(list)
            }

            ipc::Request::Info => {
                let mut list: Vec<ipc::OutputFullInfo> = outputs
                    .iter()
                    .map(|o| {
                        let cur = current.get(&o.name).cloned().unwrap_or(ipc::CurrentTarget::Unset);
                        let (mode, bg_colour, transition) = match last_set.get(&o.name) {
                            Some(ps) => (ps.mode, ps.bg_colour, ipc_transition_from_persisted(&ps.transition)),
                            None => (None, None, ipc::Transition::None),
                        };
                        ipc::OutputFullInfo {
                            name: o.name.clone(),
                            width: o.width,
                            height: o.height,
                            scale: o.scale,
                            current: cur,
                            mode,
                            bg_colour,
                            transition,
                        }
                    })
                    .collect();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                ipc::Response::Info(list)
            }

            ipc::Request::Doctor => {
                let health = wl.health();
                let mut warnings = Vec::new();
                if !health.has_compositor { warnings.push("wl_compositor not found".to_string()); }
                if !health.has_shm       { warnings.push("wl_shm not found".to_string()); }
                if !health.has_layer_shell { warnings.push("zwlr_layer_shell_v1 not found".to_string()); }
                if !health.has_xdg_output_manager {
                    warnings.push("zxdg_output_manager_v1 not found (output names may be unavailable)".to_string());
                }
                if outputs.is_empty() { warnings.push("no wl_outputs detected".to_string()); }

                ipc::Response::Doctor(ipc::DoctorReport {
                    socket_ok: true,
                    has_compositor: health.has_compositor,
                    has_shm: health.has_shm,
                    has_layer_shell: health.has_layer_shell,
                    has_xdg_output_manager: health.has_xdg_output_manager,
                    shm_formats: health.shm_formats,
                    warnings,
                })
            }

            ipc::Request::Stop => {
                *quitting = true;
                ipc::Response::Ok
            }

            ipc::Request::Restore => {
                ipc::Response::Error { message: "internal: restore should be handled by run loop".into() }
            }

            ipc::Request::Unset { outputs: sel } => {
                let selected = match select_outputs(outputs, &sel) {
                    Ok(v) => v,
                    Err(msg) => return ipc::Response::Error { message: msg },
                };
                if selected.is_empty() {
                    return ipc::Response::Error { message: "no outputs selected".into() };
                }

                for name in selected {
                    // 1) Commit a black frame so "unset" actually changes what you see.
                    //    This avoids the compositor just keeping the last committed buffer forever.
                    if let Some(outinfo) = outputs.iter().find(|o| o.name == name) {
                        // Best-effort: try a few times in case we need configure/frame-ready.
                        for _ in 0..8 {
                            match wl.present_rendered(&name, outinfo.width, outinfo.height, |dst| {
                                dst.fill(0); // XRGB8888 black
                                Ok(())
                            }) {
                                Ok(true) => break,
                                Ok(false) => {
                                    // compositor not ready; wait for events then retry
                                    let _ = wl.blocking_dispatch();
                                }
                                Err(_e) => break,
                            }
                        }
                    }

                    // 2) Now tear down / stop tracking.
                    active.remove(&name);
                    current.insert(name.clone(), ipc::CurrentTarget::Unset);
                    last_set.insert(
                        name.clone(),
                        PersistedSet {
                            target: PersistedTarget::Unset,
                            mode: None,
                            bg_colour: None,
                            transition: PersistedTransition::None,
                        },
                    );

                    // Destroy the layer surface/buffers on this output.
                    let _ = wl.unset(&name);
                }

                let _ = save_state(last_set);
                ipc::Response::Ok
            }

            ipc::Request::Set(set) => {
                let selected = match select_outputs(outputs, &set.outputs) {
                    Ok(v) => v,
                    Err(msg) => return ipc::Response::Error { message: msg },
                };
                if selected.is_empty() {
                    return ipc::Response::Error { message: "no outputs selected".into() };
                }

                let tr_ipc = set.transition.clone();
                let tr_core = to_core_transition(tr_ipc.clone());
                let tr_persist = persisted_transition_from_ipc(tr_ipc);

                match set.target {
                    ipc::SetTarget::Colour(rgb) => {
                        let col = Colour { r: rgb.r, g: rgb.g, b: rgb.b };

                        for name in selected {
                            let Some(outinfo) = outputs.iter().find(|o| o.name == name) else { continue };

                            if matches!(tr_core, CoreTransition::None) {
                                let _ = eng.set_now(&name, Target::Colour(col));
                            } else {
                                let from = snapshot_pixels_for_output(outinfo, last_set.get(&name));
                                let _ = eng.set_with_transition_from(
                                    &name,
                                    OldSnapshot::Image(from),
                                    Target::Colour(col),
                                    tr_core.clone(),
                                );
                            }

                            active.insert(name.clone());
                            current.insert(name.clone(), ipc::CurrentTarget::Colour(rgb));

                            last_set.insert(
                                name.clone(),
                                PersistedSet {
                                    target: PersistedTarget::Colour { r: rgb.r, g: rgb.g, b: rgb.b },
                                    mode: None,
                                    bg_colour: None,
                                    transition: tr_persist.clone(),
                                },
                            );
                        }

                        let _ = save_state(last_set);
                        ipc::Response::Ok
                    }

                    ipc::SetTarget::ImagePath(ref path) => {
                        let Some(resolved) = resolve_image_path(path) else {
                            return ipc::Response::Error { message: format!("image not found: {path}") };
                        };

                        let decoded = match decode_image(&resolved) {
                            Ok(d) => d,
                            Err(e) => return ipc::Response::Error { message: format!("decode failed: {e}") },
                        };

                        let canonical = resolved.to_string_lossy().into_owned();

                        for name in selected {
                            let Some(outinfo) = outputs.iter().find(|o| o.name == name) else { continue };

                            let bg = set.bg_colour.unwrap_or(ipc::Rgb { r: 0, g: 0, b: 0 });
                            let pixels = scale_image(
                                &decoded,
                                outinfo.width,
                                outinfo.height,
                                to_scale_mode(set.mode),
                                Colour { r: bg.r, g: bg.g, b: bg.b },
                            );
                            let target = Target::image(
                                outinfo.width,
                                outinfo.height,
                                outinfo.width as usize * 4,
                                pixels,
                            );

                            if matches!(tr_core, CoreTransition::None) {
                                let _ = eng.set_now(&name, target);
                            } else {
                                let from = snapshot_pixels_for_output(outinfo, last_set.get(&name));
                                let _ = eng.set_with_transition_from(
                                    &name,
                                    OldSnapshot::Image(from),
                                    target,
                                    tr_core.clone(),
                                );
                            }

                            active.insert(name.clone());
                            current.insert(name.clone(), ipc::CurrentTarget::ImagePath(canonical.clone()));

                            last_set.insert(
                                name.clone(),
                                PersistedSet {
                                    target: PersistedTarget::ImagePath { path: canonical.clone() },
                                    mode: Some(set.mode),
                                    bg_colour: set.bg_colour,
                                    transition: tr_persist.clone(),
                                },
                            );
                        }

                        let _ = save_state(last_set);
                        ipc::Response::Ok
                    }

                    ipc::SetTarget::Unset => {
                        let sel = ipc::OutputSel::Named(selected);
                        handle_request(
                            eng,
                            wl,
                            outputs,
                            active,
                            current,
                            last_set,
                            ipc::Request::Unset { outputs: sel },
                            quitting,
                        )
                    }
                }
            }
        }
    })
}

pub fn to_scale_mode(m: ipc::Mode) -> ScaleMode {
    match m {
        ipc::Mode::Fill => ScaleMode::Fill,
        ipc::Mode::Fit => ScaleMode::Fit,
        ipc::Mode::Stretch => ScaleMode::Stretch,
        ipc::Mode::Center => ScaleMode::Center,
        ipc::Mode::Tile => ScaleMode::Tile,
    }
}

/// Strict selection:
/// - All => all outputs
/// - Named => error if any name is unknown (no silent "none" / "all" fallbacks)
pub fn select_outputs(
    outputs: &[gesso_wl::OutputInfo],
    sel: &ipc::OutputSel,
) -> Result<Vec<String>, String> {
    match sel {
        ipc::OutputSel::All => Ok(outputs.iter().map(|o| o.name.clone()).collect()),
        ipc::OutputSel::Named(names) => {
            if names.is_empty() {
                return Err("no outputs selected".into());
            }

            let mut picked = Vec::with_capacity(names.len());
            for want in names {
                if outputs.iter().any(|o| o.name == *want) {
                    picked.push(want.clone());
                } else {
                    return Err(format!(
                        "unknown output '{want}'. Run `gesso outputs` to see valid names."
                    ));
                }
            }
            Ok(picked)
        }
    }
}
