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
            // Resolve the path so relative paths (./foo.png, ../bar.gif, ~-less paths)
            // are expanded to an absolute path before being sent to the daemon.
            // The daemon runs in a different working directory so relative paths
            // would be meaningless by the time it tries to open the file.
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

/// Resolve an image path supplied by the user to an absolute path string.
///
/// Handles:
/// - `./foo.png`          relative to cwd
/// - `../images/foo.png`  relative to cwd
/// - `foo.png`            bare filename, relative to cwd
/// - `/absolute/foo.png`  already absolute — returned as-is after existence check
///
/// Uses `std::fs::canonicalize` when the path exists (resolves symlinks too).
/// Falls back to `std::env::current_dir().join(path)` when the file cannot be
/// found yet (lets the daemon give a cleaner "file not found" error rather than
/// us failing here with a cryptic canonicalize error).
fn resolve_image_path(raw: &str) -> anyhow::Result<String> {
    let p = std::path::Path::new(raw);

    // Already absolute — canonicalize to resolve any symlinks/..
    if p.is_absolute() {
        return match std::fs::canonicalize(p) {
            Ok(c)  => Ok(c.to_string_lossy().into_owned()),
            Err(_) => Ok(raw.to_owned()), // file may not exist yet; let daemon error
        };
    }

    // Relative path — make it absolute via cwd first, then canonicalize.
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cannot determine current directory: {e}"))?;
    let abs = cwd.join(p);

    match std::fs::canonicalize(&abs) {
        Ok(c)  => Ok(c.to_string_lossy().into_owned()),
        Err(_) => {
            // canonicalize fails if the file doesn't exist.
            // Return the manually-constructed absolute path so the daemon can
            // produce a proper "file not found" message.
            Ok(abs.to_string_lossy().into_owned())
        }
    }
}
