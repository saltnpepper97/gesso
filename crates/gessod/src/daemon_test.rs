#![cfg(test)]

use std::time::{Duration, Instant};

use eventline::{info, scope};
use gesso_core::{Colour, RenderEngine, Target, Transition, WipeDir};
use gesso_wl::{OutputInfo, WlBackend};

fn render_for(
    wl: &mut WlBackend,
    eng: &mut RenderEngine,
    outputs: &[OutputInfo],
    dur: Duration,
) -> anyhow::Result<()> {
    let until = Instant::now() + dur;
    while Instant::now() < until {
        wl.blocking_dispatch()?;
        for o in outputs {
            wl.present_rendered(&o.name, o.width, o.height, |dst| {
                eng.render_output_into(&o.name, dst);
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn run_demo_for(total: Duration) -> anyhow::Result<()> {
    scope!("gessod.demo_test", {
        info!("starting gessod demo test");

        let mut wl = WlBackend::connect()?;
        wl.roundtrip()?;

        let outputs = wl.outputs();
        if outputs.is_empty() {
            info!("no outputs detected");
            return Ok(());
        }
        for o in &outputs {
            info!("  {} ({}x{})", o.name, o.width, o.height);
        }

        let mut eng = RenderEngine::default();
        for o in &outputs {
            eng.register_output(&o.name, o.width, o.height);
        }

        let blue = Colour { r: 20, g: 40, b: 200 };
        let red = Colour { r: 255, g: 0, b: 0 };
        let green = Colour { r: 0, g: 200, b: 40 };

        for o in &outputs {
            eng.set_now(&o.name, Target::Colour(blue))?;
        }

        // --- Wait for configure + first committed frame ---
        info!("waiting for configure...");
        {
            let started = Instant::now();
            loop {
                if started.elapsed() > Duration::from_secs(5) {
                    anyhow::bail!("timed out waiting for compositor configure");
                }

                let output_list: Vec<_> = outputs
                    .iter()
                    .map(|o| (o.name.clone(), o.width, o.height))
                    .collect();

                let mut all = true;
                for (name, w, h) in &output_list {
                    let presented = wl.present_rendered(name, *w, *h, |dst| {
                        eng.render_output_into(name, dst);
                        Ok(())
                    })?;
                    if !presented {
                        all = false;
                    }
                }
                if all {
                    break;
                }
                wl.roundtrip()?;
            }
        }
        info!("configured; running demo sequence");

        // sequence (~2 + 2 + 1 + 2 = 7s), but we’ll stop at `total`
        let end = Instant::now() + total;

        // Hold blue
        if Instant::now() < end {
            render_for(&mut wl, &mut eng, &outputs, Duration::from_secs(2).min(end - Instant::now()))?;
        }

        // Wipe LEFT: blue -> red
        for o in &outputs {
            eng.set_with_transition(
                &o.name,
                Target::Colour(red),
                Transition::Wipe {
                    duration_ms: 2000,
                    dir: WipeDir::Left,
                    softness_px: 32,
                },
            )?;
        }
        if Instant::now() < end {
            render_for(&mut wl, &mut eng, &outputs, Duration::from_millis(2000).min(end - Instant::now()))?;
        }

        // Hold red
        if Instant::now() < end {
            render_for(&mut wl, &mut eng, &outputs, Duration::from_secs(1).min(end - Instant::now()))?;
        }

        // Wipe RIGHT: red -> green
        for o in &outputs {
            eng.set_with_transition(
                &o.name,
                Target::Colour(green),
                Transition::Wipe {
                    duration_ms: 2000,
                    dir: WipeDir::Right,
                    softness_px: 32,
                },
            )?;
        }
        if Instant::now() < end {
            render_for(&mut wl, &mut eng, &outputs, Duration::from_millis(2000).min(end - Instant::now()))?;
        }

        // Present a little longer if time remains
        if Instant::now() < end {
            render_for(&mut wl, &mut eng, &outputs, end - Instant::now())?;
        }

        Ok(())
    })
}

#[test]
fn wipe_demo() {
    // Don’t run by default: it requires a live Wayland session and will render.
    if std::env::var("RUN_GESSO_WL_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping wipe_demo; set RUN_GESSO_WL_TESTS=1 to run");
        return;
    }

    // Run for a bounded time so `cargo test` returns.
    run_demo_for(Duration::from_secs(8)).unwrap();
}
