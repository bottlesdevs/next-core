//! Gamescope command-line configuration and lowering.

use crate::command::{Command, Wrapper};
use serde::{Deserialize, Serialize};

pub(crate) struct Gamescope {
    config: GamescopeConfig,
    mangoapp: bool,
}

impl From<GamescopeConfig> for Gamescope {
    fn from(config: GamescopeConfig) -> Self {
        Self {
            config,
            mangoapp: false,
        }
    }
}

impl Gamescope {
    /// Uses gamescope's integrated `MangoApp` overlay.
    pub(crate) fn with_mangoapp(mut self) -> Self {
        self.mangoapp = true;
        self
    }
}

impl Into<Command> for Gamescope {
    fn into(self) -> Command {
        let args = self.config.to_args();
        Command::new("gamescope")
            .args(args)
            .args(self.mangoapp.then_some("--mangoapp"))
            .arg("--")
    }
}

impl Wrapper for Gamescope {}

/// Configuration translated into arguments for the `gamescope` executable.
///
/// Numeric values are passed through unchanged; validation is left to
/// gamescope. The [`enabled`](Self::enabled) flag controls whether
/// [`Wrappers`](super::Wrappers) applies this configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct GamescopeConfig {
    /// Whether gamescope wraps the launched command.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
    /// Width presented to the game, passed with `-w`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_width: Option<u32>,
    /// Height presented to the game, passed with `-h`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_height: Option<u32>,
    /// Width of gamescope's output, passed with `-W`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_width: Option<u32>,
    /// Height of gamescope's output, passed with `-H`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_height: Option<u32>,
    /// Focused refresh-rate limit, passed with `-r`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<u32>,
    /// Unfocused refresh-rate limit, passed with `-o`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unfocused_frame_rate: Option<u32>,
    /// Scaling mode, passed with `-S`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaler: Option<Scaler>,
    /// Upscaling filter, passed with `-F`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    /// Filter sharpness passed with `--sharpness`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<u8>,
    /// Whether to request a borderless window with `-b`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub borderless: bool,
    /// Whether to request fullscreen output with `-f`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fullscreen: bool,
}

/// Gamescope scaling policy used when input and output sizes differ.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaler {
    /// Lets gamescope choose the scaling policy.
    Auto,
    /// Scales by whole-number factors.
    Integer,
    /// Fits the entire game image inside the output while preserving aspect ratio.
    Fit,
    /// Fills the output while preserving aspect ratio, cropping when necessary.
    Fill,
    /// Stretches the game image to the output dimensions.
    Stretch,
}

impl Scaler {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Integer => "integer",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
        }
    }
}

/// Filter gamescope uses while scaling the game image.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Filter {
    /// Linear interpolation.
    Linear,
    /// Nearest-neighbor sampling.
    Nearest,
    /// AMD `FidelityFX` Super Resolution.
    Fsr,
    /// NVIDIA Image Scaling.
    Nis,
    /// Pixel-oriented scaling.
    Pixel,
}

impl Filter {
    fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Nearest => "nearest",
            Self::Fsr => "fsr",
            Self::Nis => "nis",
            Self::Pixel => "pixel",
        }
    }
}

impl GamescopeConfig {
    fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        for (flag, value) in [
            ("-w", self.game_width),
            ("-h", self.game_height),
            ("-W", self.output_width),
            ("-H", self.output_height),
            ("-r", self.frame_rate),
            ("-o", self.unfocused_frame_rate),
        ] {
            if let Some(value) = value {
                args.extend([flag.to_string(), value.to_string()]);
            }
        }

        if let Some(scaler) = self.scaler {
            args.extend(["-S".into(), scaler.as_str().into()]);
        }
        if let Some(filter) = self.filter {
            args.extend(["-F".into(), filter.as_str().into()]);
        }
        if let Some(sharpness) = self.sharpness {
            args.extend(["--sharpness".into(), sharpness.to_string()]);
        }
        if self.borderless {
            args.push("-b".into());
        }
        if self.fullscreen {
            args.push("-f".into());
        }

        args
    }
}
