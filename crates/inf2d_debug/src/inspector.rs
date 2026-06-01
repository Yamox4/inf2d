use bevy::gizmos::config::GizmoConfigStore;
use bevy::input::common_conditions::input_toggle_active;
use bevy::prelude::*;
use bevy_inspector_egui::quick::WorldInspectorPlugin;

/// Tracks whether the world inspector overlay is visible. The actual visibility is gated
/// by [`input_toggle_active`] keyed to `F3`; this resource exists so other systems can
/// observe (or seed) the state without re-deriving it from key state.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct InspectorState {
    pub visible: bool,
}

/// Adds the `bevy-inspector-egui` world inspector, toggled by `F3`.
///
/// Workaround: `bevy_gizmos 0.18` doesn't `register_type::<GizmoConfigStore>()` itself,
/// but `bevy-inspector-egui 0.36` unconditionally calls `register_type_data` for it on
/// startup — which panics if the type wasn't registered first. We pre-register it here.
pub fn add_world_inspector(app: &mut App) {
    app.register_type::<GizmoConfigStore>()
        .init_resource::<InspectorState>()
        .add_plugins(
            WorldInspectorPlugin::new().run_if(input_toggle_active(false, KeyCode::F3)),
        );
}
