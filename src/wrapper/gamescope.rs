use crate::runner::{Command, Wrapper};
use serde::{Deserialize, Serialize};

pub(crate) struct Gamescope {
    config: GamescopeConfig,
}

impl From<GamescopeConfig> for Gamescope {
    fn from(config: GamescopeConfig) -> Self {
        Self { config }
    }
}

impl Into<Command> for Gamescope {
    fn into(self) -> Command {
        let args = self.config.to_args();
        Command::new("gamescope").args(args).arg("--")
    }
}

impl Wrapper for Gamescope {}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct GamescopeConfig {
    pub(crate) enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) game_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) game_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unfocused_frame_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scaler: Option<Scaler>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sharpness: Option<u8>,
    pub(crate) borderless: bool,
    pub(crate) fullscreen: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Scaler {
    Auto,
    Integer,
    Fit,
    Fill,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Filter {
    Linear,
    Nearest,
    Fsr,
    Nis,
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
            if let Some(value) = value.filter(|value| *value > 0) {
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
