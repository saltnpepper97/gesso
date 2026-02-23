use crate::WlResult;
use crate::state::WlState;

use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_output, wl_region, wl_surface};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity},
};

pub struct LayerSurface {
    pub surface: wl_surface::WlSurface,
    #[allow(dead_code)]
    pub layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,

    // Not currently used (we track configure in WlState), but kept for future.
    #[allow(dead_code)]
    pub configured: bool,
    #[allow(dead_code)]
    pub last_configure_serial: u32,
}

pub fn create_wallpaper_layer_surface(
    state: &WlState,
    qh: &QueueHandle<WlState>,
    output: &wl_output::WlOutput,
    out_global: u32,
    _width: u32,
    _height: u32,
) -> WlResult<LayerSurface> {
    let compositor = state.require_compositor()?.clone();
    let layer_shell = state.require_layer_shell()?.clone();

    let surface = compositor.create_surface(qh, ());

    // IMPORTANT: match buffer scale to the output scale.
    // If we don't, many compositors will resample/filter our shm buffer,
    // which can create "ghost edges"/double-front illusions during wipes.
    let scale = state
        .outputs
        .get(&out_global)
        .map(|(_, raw)| raw.scale)
        .unwrap_or(1)
        .max(1);

    surface.set_buffer_scale(scale as i32);

    // Click-through (pointer): empty input region.
    let region: wl_region::WlRegion = compositor.create_region(qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();

    let layer = layer_shell.get_layer_surface(
        &surface,
        Some(output),
        zwlr_layer_shell_v1::Layer::Background,
        "gesso".into(),
        qh,
        out_global, // userdata: output wl_global
    );

    // Click-through (keyboard): no focus.
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);

    // IMPORTANT: don't force pixel dimensions here.
    // Let the compositor pick the correct logical size via configure.
    // We anchor to all edges so it covers the whole output.
    layer.set_size(0, 0);
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);

    // Background shouldn't reserve space.
    layer.set_exclusive_zone(-1);

    // Trigger compositor configure.
    surface.commit();

    Ok(LayerSurface {
        surface,
        layer,
        configured: false,
        last_configure_serial: 0,
    })
}
