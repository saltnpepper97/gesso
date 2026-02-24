// Author: Dustin Pilgrim
// License: MIT

use clap::Parser;
use gesso_ipc::{default_socket_path, request};
use gesso_ipc::protocol as ipc;
mod cli;
mod defaults;
mod format;
mod parse;
use cli::{Cli, Command};
use defaults::{build_transition_colour, build_transition_image};
use format::print_response;
use parse::{map_mode, parse_rgb, sel_from_option};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let sock: std::path::PathBuf = if let Ok(p) = std::env::var("GESSO_SOCKET") {
        p.into()
    } else {
        default_socket_path()?
    };
    let resp = match cli.cmd {
        Command::Outputs => request(&sock, &ipc::Request::Outputs)?,
        Command::Info    => request(&sock, &ipc::Request::Info)?,
        Command::Doctor  => request(&sock, &ipc::Request::Doctor)?,
        Command::Stop    => request(&sock, &ipc::Request::Stop)?,
        Command::Unset { output } => {
            request(&sock, &ipc::Request::Unset { outputs: sel_from_option(output) })?
        }
        Command::Colour {
            colour,
            transition,
            duration,
            transition_steps,
            from,
            output,
        } => {
            let rgb = parse_rgb(&colour)?;
            let req = ipc::Request::Set(ipc::SetRequest {
                outputs:    sel_from_option(output),
                target:     ipc::SetTarget::Colour(rgb),
                mode:       ipc::Mode::Fill,
                bg_colour:  None,
                transition: build_transition_colour(transition, duration, from, transition_steps),
            });
            request(&sock, &req)?
        }
        Command::Set {
            target,
            mode,
            colour,
            transition,
            duration,
            transition_steps,
            from,
            output,
        } => {
            let resolved = resolve_image_path(&target)?;

            let bg       = colour.map(|c| parse_rgb(&c)).transpose()?;
            let mode_ipc = map_mode(mode);
            let req = ipc::Request::Set(ipc::SetRequest {
                outputs:    sel_from_option(output),
                target:     ipc::SetTarget::ImagePath(resolved),
                mode:       mode_ipc,
                bg_colour:  bg,
                transition: build_transition_image(
                    transition,
                    duration,
                    from,
                    transition_steps,
                    mode_ipc,
                ),
            });
            request(&sock, &req)?
        }
    };
    print_response(resp)?;
    Ok(())
}

/// Resolve an image path to send to the daemon.
///
/// Three cases:
///
/// - Already absolute (`/home/user/foo.png`) — canonicalize to resolve symlinks
///   and `..`, then send. Daemon receives a clean absolute path.
///
/// - Explicitly relative (`./foo.png`, `../images/foo.png`) — join with cwd and
///   canonicalize. The daemon runs in a different working directory so relative
///   paths are meaningless to it; we must expand them here.
///
/// - Bare name (`wallpaper.png`, `summer/beach.jpg`) — return unchanged. The
///   daemon's `resolve_image_path` in persist.rs will search `GESSO_DIRS` for
///   these. If we expanded them here we'd send `/cwd/wallpaper.png` and the
///   GESSO_DIRS search would never run.
fn resolve_image_path(raw: &str) -> anyhow::Result<String> {
    let p = std::path::Path::new(raw);

    // Case 1: already absolute.
    if p.is_absolute() {
        return match std::fs::canonicalize(p) {
            Ok(c)  => Ok(c.to_string_lossy().into_owned()),
            Err(_) => Ok(raw.to_owned()), // file may not exist yet; let daemon error
        };
    }

    // Case 2: explicitly relative — starts with ./ or ../
    // Note: "." and ".." on their own are also explicitly relative.
    let explicitly_relative = raw.starts_with("./")
        || raw.starts_with("../")
        || raw == "."
        || raw == "..";

    if explicitly_relative {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("cannot determine current directory: {e}"))?;
        let abs = cwd.join(p);
        return match std::fs::canonicalize(&abs) {
            Ok(c)  => Ok(c.to_string_lossy().into_owned()),
            Err(_) => Ok(abs.to_string_lossy().into_owned()), // let daemon error
        };
    }

    // Case 3: bare name — leave alone for daemon GESSO_DIRS search.
    Ok(raw.to_owned())
}
