use std::time::{Duration, Instant};

use joyshockrust_core::{Controls, validate_trigger_thresholds};

use crate::gyro_gate::{TriggerGate, TriggerSide};

#[derive(Clone, Copy)]
pub struct Settings {
    side: TriggerSide,
    gates: [Option<TriggerGate>; 2],
}

impl Settings {
    pub fn parse(payload: &[u8]) -> Option<Option<Self>> {
        if payload.is_empty() {
            return Some(None);
        }
        let &[
            side @ 0..=1,
            partial,
            partial_release,
            full,
            full_release,
            ref rest @ ..,
        ] = payload
        else {
            return None;
        };
        let (mask, limit) = match rest {
            [] => (3, 100),
            &[mask @ 1..=3] => (mask, 100),
            &[mask @ 1..=3, limit @ 1..=100] => (mask, limit),
            _ => return None,
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
                (mask & 1 != 0).then(|| {
                    TriggerGate::from_percent(side, partial, partial_release).within_travel(limit)
                }),
                (mask & 2 != 0).then(|| {
                    TriggerGate::from_percent(side, full, full_release).within_travel(limit)
                }),
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
                .filter(|old| {
                    old.expires > now
                        && old.settings.side == settings.side
                        && old.settings.gates.map(|gate| gate.is_some())
                            == settings.gates.map(|gate| gate.is_some())
                })
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
        let next = std::array::from_fn(|i| {
            self.settings.gates[i].is_some_and(|gate| gate.pressed(controls, previous[i]))
        });
        self.stages.replace(next).is_some_and(|old| old != next) && Instant::now() < self.expires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unassigned_stage_does_not_generate_a_preview_cue() {
        let mut preview = None;
        Preview::refresh(
            &mut preview,
            Settings::parse(&[0, 45, 35, 95, 85, 1]).unwrap(),
        );
        let preview = preview.as_mut().unwrap();
        let mut controls = Controls::default();
        assert!(!preview.crossed(&controls));
        controls.left_trigger = 0.5;
        assert!(preview.crossed(&controls));
        controls.left_trigger = 1.0;
        assert!(!preview.crossed(&controls));
        controls.left_trigger = 0.85;
        assert!(!preview.crossed(&controls));
        controls.left_trigger = 0.35;
        assert!(preview.crossed(&controls));
    }

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
