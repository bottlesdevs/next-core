use crate::runner::{Command, Wrapper};
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct GamescopeConfig {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unfocused_frame_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaler: Option<Scaler>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub borderless: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaler {
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
pub enum Filter {
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
