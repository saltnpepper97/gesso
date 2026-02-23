use gesso_ipc::protocol as ipc;

use crate::cli::ModeArg;

pub fn sel_from_option(output: Option<String>) -> ipc::OutputSel {
    match output {
        Some(o) => ipc::OutputSel::Named(vec![o]),
        None => ipc::OutputSel::All,
    }
}

pub fn map_mode(m: ModeArg) -> ipc::Mode {
    match m {
        ModeArg::Fill => ipc::Mode::Fill,
        ModeArg::Fit => ipc::Mode::Fit,
        ModeArg::Stretch => ipc::Mode::Stretch,
        ModeArg::Center => ipc::Mode::Center,
        ModeArg::Tile => ipc::Mode::Tile,
    }
}

pub fn parse_rgb(s: &str) -> anyhow::Result<ipc::Rgb> {
    let t = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if t.len() != 6 {
        anyhow::bail!("colour must be #RRGGBB");
    }
    Ok(ipc::Rgb {
        r: u8::from_str_radix(&t[0..2], 16)?,
        g: u8::from_str_radix(&t[2..4], 16)?,
        b: u8::from_str_radix(&t[4..6], 16)?,
    })
}
