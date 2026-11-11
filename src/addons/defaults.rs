//! Built-in recipes for component slots with conventional prefix integration.
//!
//! Acquisition copies these steps and effects into a release when a catalog
//! artifact does not provide steps, or when a local component is imported.
//! Existing releases therefore keep their original recipe if these defaults
//! later change.

use std::sync::LazyLock;

use crate::{addons::Slot, proto::DllOverrideMode};

use super::{Effect, recipe::InstallStep};

static DXVK_STEPS: LazyLock<Vec<InstallStep>> = LazyLock::new(|| {
    vec![
        InstallStep::Copy {
            source: "x64/d3d8.dll".into(),
            destination: "drive_c/windows/system32/d3d8.dll".into(),
        },
        InstallStep::Copy {
            source: "x64/d3d9.dll".into(),
            destination: "drive_c/windows/system32/d3d9.dll".into(),
        },
        InstallStep::Copy {
            source: "x64/d3d10core.dll".into(),
            destination: "drive_c/windows/system32/d3d10core.dll".into(),
        },
        InstallStep::Copy {
            source: "x64/d3d11.dll".into(),
            destination: "drive_c/windows/system32/d3d11.dll".into(),
        },
        InstallStep::Copy {
            source: "x64/dxgi.dll".into(),
            destination: "drive_c/windows/system32/dxgi.dll".into(),
        },
        InstallStep::Copy {
            source: "x32/d3d8.dll".into(),
            destination: "drive_c/windows/syswow64/d3d8.dll".into(),
        },
        InstallStep::Copy {
            source: "x32/d3d9.dll".into(),
            destination: "drive_c/windows/syswow64/d3d9.dll".into(),
        },
        InstallStep::Copy {
            source: "x32/d3d10core.dll".into(),
            destination: "drive_c/windows/syswow64/d3d10core.dll".into(),
        },
        InstallStep::Copy {
            source: "x32/d3d11.dll".into(),
            destination: "drive_c/windows/syswow64/d3d11.dll".into(),
        },
        InstallStep::Copy {
            source: "x32/dxgi.dll".into(),
            destination: "drive_c/windows/syswow64/dxgi.dll".into(),
        },
    ]
});

static VKD3D_STEPS: LazyLock<Vec<InstallStep>> = LazyLock::new(|| {
    vec![
        InstallStep::Copy {
            source: "x64/d3d12.dll".into(),
            destination: "drive_c/windows/system32/d3d12.dll".into(),
        },
        InstallStep::Copy {
            source: "x64/d3d12core.dll".into(),
            destination: "drive_c/windows/system32/d3d12core.dll".into(),
        },
        InstallStep::Copy {
            source: "x86/d3d12.dll".into(),
            destination: "drive_c/windows/syswow64/d3d12.dll".into(),
        },
        InstallStep::Copy {
            source: "x86/d3d12core.dll".into(),
            destination: "drive_c/windows/syswow64/d3d12core.dll".into(),
        },
    ]
});

static NVAPI_STEPS: LazyLock<Vec<InstallStep>> = LazyLock::new(|| {
    vec![
        InstallStep::Copy {
            source: "nvapi64.dll".into(),
            destination: "drive_c/windows/system32/nvapi64.dll".into(),
        },
        InstallStep::Copy {
            source: "nvapi.dll".into(),
            destination: "drive_c/windows/syswow64/nvapi.dll".into(),
        },
    ]
});

static LATENCY_FLEX_STEPS: LazyLock<Vec<InstallStep>> = LazyLock::new(|| {
    vec![
        InstallStep::Copy {
            source: "latencyflex_layer.dll".into(),
            destination: "drive_c/windows/system32/latencyflex_layer.dll".into(),
        },
        InstallStep::Copy {
            source: "latencyflex_wine.dll".into(),
            destination: "drive_c/windows/system32/latencyflex_wine.dll".into(),
        },
        InstallStep::Copy {
            source: "latencyflex_layer.so".into(),
            destination: ".bottles/latency-flex/lib/latencyflex_layer.so".into(),
        },
        InstallStep::Copy {
            source: "liblatencyflex_layer.so".into(),
            destination: ".bottles/latency-flex/lib/liblatencyflex_layer.so".into(),
        },
        InstallStep::Copy {
            source: "latencyflex.json".into(),
            destination: ".bottles/latency-flex/layers/latencyflex.json".into(),
        },
    ]
});

static DXVK_EFFECTS: LazyLock<Vec<Effect>> =
    LazyLock::new(|| vec![native(&["d3d8", "d3d9", "d3d10core", "d3d11", "dxgi"])]);

static VKD3D_EFFECTS: LazyLock<Vec<Effect>> =
    LazyLock::new(|| vec![native(&["d3d12", "d3d12core"])]);

static NVAPI_EFFECTS: LazyLock<Vec<Effect>> = LazyLock::new(|| {
    vec![
        environment("DXVK_ENABLE_NVAPI", "1"),
        environment("PROTON_ENABLE_NVAPI", "1"),
        native(&["nvapi", "nvapi64"]),
    ]
});

static LATENCY_FLEX_EFFECTS: LazyLock<Vec<Effect>> = LazyLock::new(|| {
    vec![
        environment("VK_ADD_LAYER_PATH", ".bottles/latency-flex/layers"),
        environment("LD_LIBRARY_PATH", ".bottles/latency-flex/lib"),
        environment("LFX", "1"),
    ]
});

fn native(dlls: &[&str]) -> Effect {
    Effect::DllOverrides {
        dlls: dlls.iter().map(|&dll| dll.to_owned()).collect(),
        mode: DllOverrideMode::Native,
        when: None,
    }
}

fn environment(name: &str, value: &str) -> Effect {
    Effect::Environment {
        name: name.to_owned(),
        value: value.to_owned(),
        when: None,
    }
}

/// Returns the built-in recipe for `slot`.
///
/// Runtime addons are consumed directly and have no component slot.
pub(crate) fn steps(slot: Slot) -> &'static [InstallStep] {
    match slot {
        Slot::Dxvk => &DXVK_STEPS,
        Slot::Vkd3d => &VKD3D_STEPS,
        Slot::Nvapi => &NVAPI_STEPS,
        Slot::LatencyFlex => &LATENCY_FLEX_STEPS,
    }
}

/// Returns the effects of the built-in recipe for `slot`.
pub(crate) fn effects(slot: Slot) -> &'static [Effect] {
    match slot {
        Slot::Dxvk => &DXVK_EFFECTS,
        Slot::Vkd3d => &VKD3D_EFFECTS,
        Slot::Nvapi => &NVAPI_EFFECTS,
        Slot::LatencyFlex => &LATENCY_FLEX_EFFECTS,
    }
}
