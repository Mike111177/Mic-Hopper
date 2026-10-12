//! Decides which microphone should be live, based purely on levels.
//!
//! The heuristic: the headset mic is always near the mouth, so it is a
//! reliable "am I speaking" detector and a reference level. While speaking,
//! compare the desktop mic level (plus a calibration offset) to the headset
//! level. If the desktop is within `near_margin_db` of the headset you are
//! close to it; if it is more than `far_margin_db` below you have walked away.
//! The band in between is a dead zone (hysteresis). Only the headset gates
//! decisions, so room noise cannot trigger a switch and a muted headset
//! freezes the current choice.
//!
//! Debouncing: evidence for the *other* mic must accumulate for
//! `switch_hold_ms` of speaking time, and at least `min_dwell_ms` must have
//! passed since the last switch.

use crate::config::Switching;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mic {
    Desktop = 0,
    Headset = 1,
}

impl Mic {
    pub fn other(self) -> Mic {
        match self {
            Mic::Desktop => Mic::Headset,
            Mic::Headset => Mic::Desktop,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Mic::Desktop => "Desktop",
            Mic::Headset => "Headset",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    /// Smoothed desktop level in dBFS (before offset).
    pub desktop_db: f32,
    /// Smoothed headset level in dBFS.
    pub headset_db: f32,
    /// `(desktop + offset) - headset`.
    pub diff_db: f32,
    pub speaking: bool,
}

pub struct Selector {
    cfg: Switching,
    current: Mic,
    candidate: Option<Mic>,
    candidate_ms: f32,
    since_switch_ms: f32,
    desk_env: f32,
    head_env: f32,
    initialized: bool,
}

impl Selector {
    pub fn new(cfg: Switching, start: Mic) -> Self {
        Self {
            cfg,
            current: start,
            candidate: None,
            candidate_ms: 0.0,
            since_switch_ms: f32::MAX / 2.0,
            desk_env: -120.0,
            head_env: -120.0,
            initialized: false,
        }
    }

    pub fn current(&self) -> Mic {
        self.current
    }

    pub fn levels(&self) -> Levels {
        let diff = self.desk_env + self.cfg.desktop_offset_db - self.head_env;
        Levels {
            desktop_db: self.desk_env,
            headset_db: self.head_env,
            diff_db: diff,
            // Only the headset gates decisions. It is always at the mouth, so
            // it rejects room noise (printers, speakers) that the desktop mic
            // would otherwise mistake for speech. A muted headset therefore
            // freezes switching, which also means you can never be "unmuted"
            // by an automatic hop to the desktop mic.
            speaking: self.head_env >= self.cfg.voice_gate_db,
        }
    }

    /// Externally override the current mic (forced mode, device loss).
    /// Resets debounce state so a later return to automatic mode starts clean.
    pub fn force(&mut self, mic: Mic) {
        if self.current != mic {
            self.current = mic;
            self.since_switch_ms = 0.0;
        }
        self.candidate = None;
        self.candidate_ms = 0.0;
    }

    /// Feed instantaneous (per-block RMS) levels. Returns `Some(mic)` on the
    /// tick where a switch happens.
    pub fn update(&mut self, desktop_db: f32, headset_db: f32, dt_ms: f32) -> Option<Mic> {
        self.smooth(desktop_db, headset_db, dt_ms);
        self.since_switch_ms = (self.since_switch_ms + dt_ms).min(f32::MAX / 2.0);

        let lv = self.levels();
        if !lv.speaking {
            // Silence carries no information; hold state, don't accumulate.
            return None;
        }

        let evidence = if lv.diff_db >= self.cfg.near_margin_db {
            Some(Mic::Desktop)
        } else if lv.diff_db < self.cfg.far_margin_db {
            Some(Mic::Headset)
        } else {
            None
        };

        match evidence {
            Some(m) if m == self.current => {
                self.candidate = None;
                self.candidate_ms = 0.0;
            }
            Some(m) => {
                if self.candidate == Some(m) {
                    self.candidate_ms += dt_ms;
                } else {
                    self.candidate = Some(m);
                    self.candidate_ms = dt_ms;
                }
            }
            None => {}
        }

        if let Some(m) = self.candidate {
            if self.candidate_ms >= self.cfg.switch_hold_ms
                && self.since_switch_ms >= self.cfg.min_dwell_ms
            {
                self.current = m;
                self.candidate = None;
                self.candidate_ms = 0.0;
                self.since_switch_ms = 0.0;
                return Some(m);
            }
        }
        None
    }

    fn smooth(&mut self, desktop_db: f32, headset_db: f32, dt_ms: f32) {
        if !self.initialized {
            self.desk_env = desktop_db;
            self.head_env = headset_db;
            self.initialized = true;
            return;
        }
        let coef = |tau_ms: f32| -> f32 {
            if tau_ms <= 0.0 {
                1.0
            } else {
                1.0 - (-dt_ms / tau_ms).exp()
            }
        };
        let attack = coef(self.cfg.attack_ms);
        let release = coef(self.cfg.release_ms);
        let follow = |env: &mut f32, x: f32| {
            let k = if x > *env { attack } else { release };
            *env += (x - *env) * k;
        };
        follow(&mut self.desk_env, desktop_db);
        follow(&mut self.head_env, headset_db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Switching {
        Switching {
            start_with: crate::config::StartWith::Desktop,
            desktop_offset_db: 0.0,
            voice_gate_db: -45.0,
            near_margin_db: -6.0,
            far_margin_db: -16.0,
            switch_hold_ms: 500.0,
            min_dwell_ms: 2000.0,
            attack_ms: 0.0,
            release_ms: 0.0,
        }
    }

    fn run(sel: &mut Selector, desk: f32, head: f32, ms: f32) -> Vec<Mic> {
        let mut switches = vec![];
        let mut t = 0.0;
        while t < ms {
            if let Some(m) = sel.update(desk, head, 10.0) {
                switches.push(m);
            }
            t += 10.0;
        }
        switches
    }

    #[test]
    fn silence_never_switches() {
        let mut s = Selector::new(cfg(), Mic::Desktop);
        assert!(run(&mut s, -90.0, -90.0, 10_000.0).is_empty());
        assert_eq!(s.current(), Mic::Desktop);
    }

    #[test]
    fn walking_away_switches_after_hold() {
        let mut s = Selector::new(cfg(), Mic::Desktop);
        // Speaking far from desk: desktop 25 dB below headset.
        let sw = run(&mut s, -50.0, -25.0, 400.0);
        assert!(sw.is_empty(), "must not switch before hold time");
        let sw = run(&mut s, -50.0, -25.0, 200.0);
        assert_eq!(sw, vec![Mic::Headset]);
    }

    #[test]
    fn coming_back_switches_to_desktop() {
        let mut s = Selector::new(cfg(), Mic::Headset);
        let sw = run(&mut s, -27.0, -25.0, 600.0);
        assert_eq!(sw, vec![Mic::Desktop]);
    }

    #[test]
    fn dead_zone_holds_current() {
        let mut s = Selector::new(cfg(), Mic::Desktop);
        // diff = -10, between far (-16) and near (-6)
        assert!(run(&mut s, -35.0, -25.0, 5000.0).is_empty());
        let mut s = Selector::new(cfg(), Mic::Headset);
        assert!(run(&mut s, -35.0, -25.0, 5000.0).is_empty());
    }

    #[test]
    fn min_dwell_blocks_rapid_flapping() {
        let mut s = Selector::new(cfg(), Mic::Desktop);
        assert_eq!(run(&mut s, -50.0, -25.0, 600.0), vec![Mic::Headset]);
        // Immediately strong "near" evidence: hold is satisfied at 500 ms but
        // dwell (2000 ms since last switch) is not.
        assert!(run(&mut s, -25.0, -25.0, 1000.0).is_empty());
        assert_eq!(run(&mut s, -25.0, -25.0, 1000.0), vec![Mic::Desktop]);
    }

    #[test]
    fn offset_shifts_comparison() {
        let mut c = cfg();
        c.desktop_offset_db = 20.0;
        let mut s = Selector::new(c, Mic::Headset);
        // Raw desktop 20 dB quieter, but offset brings diff to 0 -> near.
        assert_eq!(run(&mut s, -45.0, -25.0, 600.0), vec![Mic::Desktop]);
    }

    #[test]
    fn muted_headset_freezes_switching() {
        // Headset muted (digital silence): no decisions in either direction,
        // even with loud sound at the desk. This is deliberate: room noise must
        // not be able to hop you onto the desktop mic.
        let mut s = Selector::new(cfg(), Mic::Headset);
        assert!(run(&mut s, -30.0, -100.0, 5000.0).is_empty());
        let mut s = Selector::new(cfg(), Mic::Desktop);
        assert!(run(&mut s, -30.0, -100.0, 5000.0).is_empty());
    }

    #[test]
    fn force_resets_candidate() {
        let mut s = Selector::new(cfg(), Mic::Desktop);
        run(&mut s, -50.0, -25.0, 400.0); // accumulate 400 ms toward headset
        s.force(Mic::Headset);
        assert_eq!(s.current(), Mic::Headset);
        // Now evidence matches current; nothing should flip back on its own.
        assert!(run(&mut s, -50.0, -25.0, 3000.0).is_empty());
    }
}
