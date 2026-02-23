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
        Command::Info => request(&sock, &ipc::Request::Info)?,
        Command::Doctor => request(&sock, &ipc::Request::Doctor)?,
        Command::Stop => request(&sock, &ipc::Request::Stop)?,

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
                outputs: sel_from_option(output),
                target: ipc::SetTarget::Colour(rgb),
                mode: ipc::Mode::Fill,
                bg_colour: None,
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
            let bg = colour.map(|c| parse_rgb(&c)).transpose()?;
            let mode_ipc = map_mode(mode);
            let req = ipc::Request::Set(ipc::SetRequest {
                outputs: sel_from_option(output),
                target: ipc::SetTarget::ImagePath(target),
                mode: mode_ipc,
                bg_colour: bg,
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
