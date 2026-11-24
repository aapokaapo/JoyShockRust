use std::time::{Duration, Instant};

use joyshockrust_core::{Controls, validate_trigger_thresholds};

use crate::gyro_gate::{TriggerGate, TriggerSide};

#[derive(Clone, Copy)]
pub struct Settings {
    side: TriggerSide,
    gates: [TriggerGate; 2],
}

impl Settings {
    pub fn parse(payload: &[u8]) -> Option<Option<Self>> {
        if payload.is_empty() {
            return Some(None);
        }
        let &[side @ 0..=1, partial, partial_release, full, full_release] = payload else {
            return None;
        };
        validate_trigger_thresholds([partial, partial_release, full, full_release]).ok()?;
        let side = if side == 0 {
            TriggerSide::Left
        } else {
            TriggerSide::Right
        };
        Some(Some(Self {
            side,
            gates: [
                TriggerGate::from_percent(side, partial, partial_release),
                TriggerGate::from_percent(side, full, full_release),
            ],
        }))
    }
}

pub struct Preview {
    settings: Settings,
    stages: Option<[bool; 2]>,
    expires: Instant,
}

impl Preview {
    pub fn refresh(current: &mut Option<Self>, settings: Option<Settings>) {
        let now = Instant::now();
        *current = settings.map(|settings| {
            let stages = current
                .as_ref()
                .filter(|old| old.expires > now && old.settings.side == settings.side)
                .and_then(|old| old.stages);
            Self {
                settings,
                stages,
                expires: now + Duration::from_secs(2),
            }
        });
    }

    pub fn crossed(&mut self, controls: &Controls) -> bool {
        let previous = self.stages.unwrap_or_default();
        let next = std::array::from_fn(|i| self.settings.gates[i].pressed(controls, previous[i]));
        self.stages.replace(next).is_some_and(|old| old != next) && Instant::now() < self.expires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_cues_each_stage_with_hysteresis_and_tracks_live_edits() {
        let mut preview = None;
        Preview::refresh(&mut preview, Settings::parse(&[0, 45, 35, 95, 85]).unwrap());
        let mut controls = Controls {
            right_trigger: 1.0,
            ..Controls::default()
        };
        for (position, crossed) in [
            (0.10, false),
            (0.44, false),
            (0.45, true),
            (0.40, false),
            (0.95, true),
            (0.90, false),
            (0.85, true),
            (0.80, false),
            (0.35, true),
            (0.35, false),
        ] {
            controls.left_trigger = position;
            assert_eq!(
                preview.as_mut().unwrap().crossed(&controls),
                crossed,
                "{position}"
            );
        }
        Preview::refresh(&mut preview, Settings::parse(&[0, 30, 20, 95, 85]).unwrap());
        assert!(preview.as_mut().unwrap().crossed(&controls));
        Preview::refresh(&mut preview, Settings::parse(&[1, 30, 20, 95, 85]).unwrap());
        assert!(!preview.as_mut().unwrap().crossed(&controls));
        preview.as_mut().unwrap().expires = Instant::now();
        controls.right_trigger = 0.0;
        assert!(!preview.as_mut().unwrap().crossed(&controls));
        assert!(Settings::parse(&[0, 20, 30, 95, 85]).is_none());
        Preview::refresh(&mut preview, Settings::parse(&[]).unwrap());
        assert!(preview.is_none());
    }
}
