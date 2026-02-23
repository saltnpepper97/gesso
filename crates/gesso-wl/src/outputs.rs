use crate::state::WlState;

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub wl_global: u32,
}

pub struct Outputs {
    list: Vec<OutputInfo>,
}

impl Outputs {
    pub fn from_state(state: &WlState) -> Self {
        let mut v: Vec<(u32, (u32, u32, u32, Option<String>))> = state
            .outputs
            .iter()
            .map(|(global, (_obj, raw))| {
                (*global, (raw.width, raw.height, raw.scale, raw.name.clone()))
            })
            .collect();

        v.sort_by_key(|(global, _)| *global);

        let list = v
            .into_iter()
            .filter_map(|(global, (w, h, s, name_opt))| {
                let name = name_opt?; // skip unnamed outputs (no OUT-* fallback)
                Some(OutputInfo {
                    name,
                    width: w,
                    height: h,
                    scale: s,
                    wl_global: global,
                })
            })
            .collect();

        Self { list }
    }

    pub fn list(&self) -> Vec<OutputInfo> {
        self.list.clone()
    }

    pub fn by_name(&self, name: &str) -> Option<&OutputInfo> {
        self.list.iter().find(|o| o.name == name)
    }
}
