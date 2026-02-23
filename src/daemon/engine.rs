// Author: Dustin Pilgrim
// License: MIT

use anyhow::Result;
use std::path::Path;

use crate::spec::Spec;
use crate::wallpaper::Engine;

use super::state::{clear_current, save_current};

pub fn build_engine() -> Result<Engine> {
    let mut engine = Engine::new()?;
    let pr = engine.probe();

    eventline::info!(
        "wayland probe wayland_display={} compositor={} shm={} layer_shell={} outputs={}",
        pr.wayland_display,
        pr.compositor,
        pr.shm,
        pr.layer_shell,
        pr.outputs
    );

    // Do a couple roundtrips to pick up initial globals/output metadata,
    // but DO NOT add fixed sleeps here (wait_for_configured handles readiness).
    for i in 0..2 {
        if let Err(e) = engine.roundtrip() {
            eventline::warn!("initial roundtrip failed attempt={} err={:#}", i, e);
        }
    }

    Ok(engine)
}

pub fn apply_with_retry(engine: &mut Engine, spec: Spec, current_path: &Path) -> Result<()> {
    eventline::scope!(
        "gesso.apply",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            match engine.apply(spec.clone()) {
                Ok(_) => {
                    if let Err(e) = save_current(current_path, &spec) {
                        eventline::warn!("save_current failed err={:#}", e);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) if super::utils::is_broken_pipe(&e) => {
                    eventline::error!("wayland broken pipe; recreating engine err={:#}", e);
                    *engine = build_engine()?;

                    engine.apply(spec.clone())?;
                    if let Err(e2) = save_current(current_path, &spec) {
                        eventline::warn!("save_current failed err={:#}", e2);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) => Err(e),
            }
        }
    )
}

pub fn unset_with_retry(engine: &mut Engine, output: Option<&str>, current_path: &Path) -> Result<()> {
    eventline::scope!(
        "gesso.unset",
        success = "unset",
        failure = "failed",
        aborted = "aborted",
        {
            match engine.unset(output) {
                Ok(_) => {
                    if output.is_none() {
                        clear_current(current_path);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) if super::utils::is_broken_pipe(&e) => {
                    eventline::error!("wayland broken pipe; recreating engine err={:#}", e);
                    *engine = build_engine()?;

                    engine.unset(output)?;
                    if output.is_none() {
                        clear_current(current_path);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) => Err(e),
            }
        }
    )
}
