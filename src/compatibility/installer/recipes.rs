use std::sync::LazyLock;

use crate::{
    compatibility::{components::catalog::ComponentKind, installer::InstallStep},
    proto::DllOverrideMode,
};

static DXVK_STEPS: LazyLock<Vec<InstallStep>> = LazyLock::new(|| {
    vec![
        InstallStep::SetDllOverrides {
            dlls: ["d3d8", "d3d9", "d3d10core", "d3d11", "dxgi"]
                .into_iter()
                .map(String::from)
                .collect(),
            mode: DllOverrideMode::Native,
        },
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
        InstallStep::SetDllOverrides {
            dlls: ["d3d12", "d3d12core"]
                .into_iter()
                .map(String::from)
                .collect(),
            mode: DllOverrideMode::Native,
        },
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
        InstallStep::SetEnvironment {
            name: "DXVK_ENABLE_NVAPI".into(),
            value: "1".into(),
        },
        InstallStep::SetEnvironment {
            name: "PROTON_ENABLE_NVAPI".into(),
            value: "1".into(),
        },
        InstallStep::SetDllOverrides {
            dlls: ["nvapi", "nvapi64"].into_iter().map(String::from).collect(),
            mode: DllOverrideMode::Native,
        },
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
        InstallStep::SetEnvironment {
            name: "VK_ADD_LAYER_PATH".into(),
            value: ".bottles/latency-flex/layers".into(),
        },
        InstallStep::SetEnvironment {
            name: "LD_LIBRARY_PATH".into(),
            value: ".bottles/latency-flex/lib".into(),
        },
        InstallStep::SetEnvironment {
            name: "LFX".into(),
            value: "1".into(),
        },
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

pub(in crate::compatibility) fn component_steps(
    kind: ComponentKind,
) -> Option<&'static [InstallStep]> {
    match kind {
        ComponentKind::Dxvk => Some(&DXVK_STEPS),
        ComponentKind::Vkd3d => Some(&VKD3D_STEPS),
        ComponentKind::Nvapi => Some(&NVAPI_STEPS),
        ComponentKind::LatencyFlex => Some(&LATENCY_FLEX_STEPS),
        ComponentKind::Runner { .. } | ComponentKind::Winebridge | ComponentKind::Umu => None,
    }
}
