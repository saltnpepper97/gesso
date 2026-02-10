// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Result};
use clap::Parser;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use gesso::cli::{Cli, Command};
use gesso::path::paths;
use gesso::protocol::{Request, Response};
use gesso::spec::{Mode, Rgb, Spec, Transition, TransitionSpec};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let p = paths()?;

    let req = match cli.cmd {
        Command::Set {
            target,
            mode,
            colour,
            transition,
            duration,
            output,
        } => {
            let path = resolve_target(&target)?;
            let colour = match colour {
                Some(c) => Rgb::parse(&c)?,
                None => Rgb { r: 0, g: 0, b: 0 },
            };
            let transition = TransitionSpec {
                kind: Transition::from(transition),
                duration,
            };
            Request::Apply {
                spec: Spec::Image {
                    path,
                    mode: Mode::from(mode),
                    colour,
                    output,
                    transition,
                },
            }
        }

        Command::Colour {
            colour,
            transition,
            duration,
            output,
        } => {
            let transition = TransitionSpec {
                kind: Transition::from(transition),
                duration,
            };
            Request::Apply {
                spec: Spec::Colour {
                    colour: Rgb::parse(&colour)?,
                    output,
                    transition,
                },
            }
        }

        Command::Unset { output } => Request::Unset { output },

        Command::Stop => Request::Stop,
        Command::Status => Request::Status,
        Command::Doctor => Request::Doctor,
    };

    let mut stream = UnixStream::connect(&p.sock_path).map_err(|_| {
        anyhow::anyhow!(
            "gessod not running (socket missing at {})",
            p.sock_path.display()
        )
    })?;

    let msg = serde_json::to_string(&req)?;
    stream.write_all(msg.as_bytes())?;
    stream.write_all(b"\n")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let resp: Response = serde_json::from_str(line.trim())?;

    match resp {
        Response::Ok => Ok(()),
        Response::Status { current } => {
            if let Some(c) = current {
                println!("{:#?}", c);
            } else {
                println!("(no current wallpaper)");
            }
            Ok(())
        }
        Response::Doctor { checks } => {
            for c in checks {
                println!(
                    "{}: {} ({})",
                    c.name,
                    if c.ok { "ok" } else { "FAIL" },
                    c.detail
                );
            }
            Ok(())
        }
        Response::Error { message } => bail!(message),
    }
}

fn resolve_target(target: &str) -> Result<std::path::PathBuf> {
    let p = std::path::PathBuf::from(target);
    if p.is_absolute() || target.contains('/') {
        return Ok(std::fs::canonicalize(&p)?);
    }

    // Search GESSO_DIRS (colon-separated), else CWD
    if let Some(dirs) = std::env::var_os("GESSO_DIRS") {
        for dir in std::env::split_paths(&dirs) {
            let cand = dir.join(target);
            if cand.exists() {
                return Ok(std::fs::canonicalize(cand)?);
            }
        }
    }

    // fallback to CWD
    let cand = std::path::PathBuf::from(target);
    Ok(std::fs::canonicalize(cand)?)
}
