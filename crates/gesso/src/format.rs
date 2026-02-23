use gesso_ipc::protocol as ipc;

// ---- output formatting ----

pub fn print_response(resp: ipc::Response) -> anyhow::Result<()> {
    match resp {
        ipc::Response::Ok => {}

        ipc::Response::Outputs(list) => {
            for o in list {
                println!("{} width={} height={} scale={}", o.name, o.width, o.height, o.scale);
            }
        }

        // Each output gets a block of key=value lines, all prefixed with the output name
        // so every line is independently greppable: `gesso info | grep DP-1`
        ipc::Response::Info(outputs) => {
            for (i, o) in outputs.iter().enumerate() {
                if i > 0 {
                    println!();
                }

                println!("{}", o.name);
                println!("    width={} height={} scale={}", o.width, o.height, o.scale);

                match &o.current {
                    ipc::CurrentTarget::Unset => {
                        println!("    target=unset");
                    }
                    ipc::CurrentTarget::Colour(rgb) => {
                        println!("    target=colour");
                        println!("    colour=#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b);
                    }
                    ipc::CurrentTarget::ImagePath(path) => {
                        println!("    target=image");
                        println!("    path={path}");
                        if let Some(mode) = o.mode {
                            println!("    mode={}", fmt_mode(mode));
                        }
                        if let Some(bg) = o.bg_colour {
                            println!("    bg=#{:02x}{:02x}{:02x}", bg.r, bg.g, bg.b);
                        }
                    }
                }

                match &o.transition {
                    ipc::Transition::None => {
                        println!("    transition=none");
                    }
                    ipc::Transition::Drop { duration_ms, steps } => {
                        println!("    transition=drop");
                        println!("    duration_ms={duration_ms}");
                        if let Some(s) = steps {
                            println!("    steps={s}");
                        }
                    }
                    ipc::Transition::Fade { duration_ms, steps } => {
                        println!("    transition=fade");
                        println!("    duration_ms={duration_ms}");
                        if let Some(s) = steps {
                            println!("    steps={s}");
                        }
                    }
                    ipc::Transition::Wave { duration_ms, dir, steps } => {
                        let dir_s = match dir {
                            ipc::WaveDir::Left => "left",
                            ipc::WaveDir::Right => "right",
                        };
                        println!("    transition=wave");
                        println!("    duration_ms={duration_ms}");
                        println!("    dir={dir_s}");
                        if let Some(s) = steps {
                            println!("    steps={s}");
                        }
                    }
                }
            }
        }

        ipc::Response::Doctor(rep) => {
            println!("socket={}", ok_str(rep.socket_ok));
            println!("compositor={}", ok_str(rep.has_compositor));
            println!("shm={}", ok_str(rep.has_shm));
            println!("layer_shell={}", ok_str(rep.has_layer_shell));
            println!("xdg_output_manager={}", ok_str(rep.has_xdg_output_manager));

            if rep.shm_formats.is_empty() {
                println!("shm_formats=none");
            } else {
                // Print as space-separated hex codes on one line — still greppable per format
                let fmts = rep
                    .shm_formats
                    .iter()
                    .map(|f| format!("{f:#010x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("shm_formats={fmts}");
            }

            if rep.warnings.is_empty() {
                println!("warnings=none");
            } else {
                for w in &rep.warnings {
                    println!("warning={w}");
                }
            }
        }

        ipc::Response::Error { message } => anyhow::bail!("{message}"),
    }
    Ok(())
}

fn ok_str(v: bool) -> &'static str {
    if v { "ok" } else { "missing" }
}

fn fmt_mode(m: ipc::Mode) -> &'static str {
    match m {
        ipc::Mode::Fill => "fill",
        ipc::Mode::Fit => "fit",
        ipc::Mode::Stretch => "stretch",
        ipc::Mode::Center => "center",
        ipc::Mode::Tile => "tile",
    }
}
