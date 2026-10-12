use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Template written on first run. It doubles as the source of default values
/// (see `Config::default`), so it must always parse.
pub const DEFAULT_CONFIG_TOML: &str = r#"# Mic Hopper configuration
#
# Device names are matched as case-insensitive substrings against the names
# printed by `mic-hopper --list-devices`.
#
# `output` must be the *playback* side of a virtual audio cable
# (e.g. "CABLE Input (VB-Audio Virtual Cable)"). Point your chat app's
# microphone at the cable's *recording* side ("CABLE Output").
#
# Recommended: set the cable and both mics to 48000 Hz in
# Sound Control Panel > Properties > Advanced. Mismatched rates still work
# (inputs are resampled) but matching them gives the best quality.

[devices]
desktop = "CHANGE ME"
headset = "CHANGE ME"
output = "CABLE Input"

[audio]
# Target cushion of buffered audio in milliseconds. This is added latency.
# Lower = less latency, higher = more tolerance for scheduling jitter.
buffer_ms = 8
# Crossfade length when switching mics (avoids clicks).
crossfade_ms = 15

[switching]
# Which mic to use at startup and whenever no decision can be made.
start_with = "desktop"

# Calibration: added to the desktop mic level before comparing it with the
# headset. Run `mic-hopper --monitor`, sit at your normal "near" position,
# talk, and set this so the printed `diff` hovers around 0 dB.
desktop_offset_db = 0.0

# The headset mic must be louder than this (dBFS) to count as "speaking".
# Decisions are only made while speaking, since silence tells us nothing.
# Only the headset gates this, so room noise at the desktop mic cannot cause
# a switch, and muting the headset freezes the current choice (you can use
# the tray menu to force a mic at any time).
voice_gate_db = -45.0

# Evidence thresholds, in dB, for (desktop + offset) - headset:
#   diff >= near_margin_db  -> you are near the desktop mic
#   diff <  far_margin_db   -> you are away from the desktop mic
# Anything in between is a dead zone (hysteresis) that keeps the current mic.
near_margin_db = -6.0
far_margin_db = -16.0

# Debounce: evidence for the other mic must persist this long (counted only
# while speaking) before switching.
switch_hold_ms = 500
# Debounce: minimum time between two switches.
min_dwell_ms = 2000

# Level smoothing (envelope follower).
attack_ms = 20
release_ms = 250
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub devices: Devices,
    #[serde(default)]
    pub audio: Audio,
    #[serde(default)]
    pub switching: Switching,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Devices {
    pub desktop: String,
    pub headset: String,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub buffer_ms: f32,
    pub crossfade_ms: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Switching {
    pub start_with: StartWith,
    pub desktop_offset_db: f32,
    pub voice_gate_db: f32,
    pub near_margin_db: f32,
    pub far_margin_db: f32,
    pub switch_hold_ms: f32,
    pub min_dwell_ms: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartWith {
    Desktop,
    Headset,
}

// Defaults are literal values here; a test asserts the template above agrees.
impl Default for Audio {
    fn default() -> Self {
        Self { buffer_ms: 8.0, crossfade_ms: 15.0 }
    }
}

impl Default for Switching {
    fn default() -> Self {
        Self {
            start_with: StartWith::Desktop,
            desktop_offset_db: 0.0,
            voice_gate_db: -45.0,
            near_margin_db: -6.0,
            far_margin_db: -16.0,
            switch_hold_ms: 500.0,
            min_dwell_ms: 2000.0,
            attack_ms: 20.0,
            release_ms: 250.0,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let s = &self.switching;
        anyhow::ensure!(
            s.far_margin_db < s.near_margin_db,
            "switching.far_margin_db must be less than switching.near_margin_db"
        );
        anyhow::ensure!(self.audio.buffer_ms >= 0.0, "audio.buffer_ms must be >= 0");
        anyhow::ensure!(self.audio.crossfade_ms >= 0.0, "audio.crossfade_ms must be >= 0");
        anyhow::ensure!(
            !self.devices.desktop.trim().is_empty() && !self.devices.headset.trim().is_empty(),
            "devices.desktop and devices.headset must be set"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_matches_defaults() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(cfg.devices.output, "CABLE Input");
        assert_eq!(cfg.audio, Audio::default());
        assert_eq!(cfg.switching, Switching::default());
        // The template's placeholder device names are non-empty, so it validates.
        cfg.validate().unwrap();
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [devices]
            desktop = "a"
            headset = "b"
            output = "c"
            [switching]
            voice_gate_db = -30.0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.switching.voice_gate_db, -30.0);
        assert_eq!(cfg.switching.min_dwell_ms, Switching::default().min_dwell_ms);
        assert_eq!(cfg.audio, Audio::default());
    }
}
