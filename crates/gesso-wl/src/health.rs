use crate::state::WlState;

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub ok: bool,
    pub has_compositor: bool,
    pub has_shm: bool,
    pub has_layer_shell: bool,
    pub has_xdg_output_manager: bool,
    pub shm_formats: Vec<u32>,
}

impl HealthReport {
    pub fn from_state(state: &WlState) -> Self {
        let has_compositor = state.compositor.is_some();
        let has_shm = state.shm.is_some();
        let has_layer_shell = state.layer_shell.is_some();
        let has_xdg_output_manager = state.xdg_output_manager.is_some();
        let ok = has_compositor && has_shm && has_layer_shell;
        Self {
            ok,
            has_compositor,
            has_shm,
            has_layer_shell,
            has_xdg_output_manager,
            shm_formats: state.shm_formats.clone(),
        }
    }
}
