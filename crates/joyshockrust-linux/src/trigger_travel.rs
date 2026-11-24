use std::time::{Duration, Instant};

use joyshockrust_core::{Controls, validate_trigger_thresholds};

use crate::gyro_gate::{TriggerGate, TriggerSide};

#[derive(Clone, Copy)]
pub struct Settings {
    side: TriggerSide,
    gates: [Option<TriggerGate>; 2],
    travel_limit: Option<TriggerGate>,
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
        let (mask, scale, travel_limit) = match rest {
            [] => (3, 100, None),
            &[mask @ 1..=3] => (mask, 100, None),
            &[mask @ 1..=3, scale @ 1..=100] => (mask, scale, None),
            &[mask @ 0..=3, scale @ 1..=100, limit @ 1..=100] => (mask, scale, Some(limit)),
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
                    TriggerGate::from_percent(side, partial, partial_release).within_travel(scale)
                }),
                (mask & 2 != 0).then(|| {
                    TriggerGate::from_percent(side, full, full_release).within_travel(scale)
                }),
            ],
            travel_limit: travel_limit
                .map(|limit| TriggerGate::from_percent(side, limit, limit - 1)),
        }))
    }
}

pub struct Preview {
    settings: Settings,
    stages: Option<[bool; 2]>,
    limit_reached: Option<bool>,
    expires: Instant,
}

impl Preview {
    pub fn refresh(current: &mut Option<Self>, settings: Option<Settings>) {
        let now = Instant::now();
        *current = settings.map(|settings| {
            let prior = current
                .as_ref()
                .filter(|old| old.expires > now && old.settings.side == settings.side);
            let stages = prior
                .filter(|old| {
                    old.settings.gates.map(|gate| gate.is_some())
                        == settings.gates.map(|gate| gate.is_some())
                })
                .and_then(|old| old.stages);
            Self {
                settings,
                stages,
                limit_reached: prior
                    .filter(|old| old.settings.travel_limit == settings.travel_limit)
                    .and_then(|old| old.limit_reached),
                expires: now + Duration::from_secs(2),
            }
        });
    }

    pub fn crossed(&mut self, controls: &Controls) -> bool {
        let previous = self.stages.unwrap_or_default();
        let next = std::array::from_fn(|i| {
            self.settings.gates[i].is_some_and(|gate| gate.pressed(controls, previous[i]))
        });
        let stage_changed = self.stages.replace(next).is_some_and(|old| old != next);
        let reached = self
            .settings
            .travel_limit
            .is_some_and(|limit| limit.pressed(controls, self.limit_reached.unwrap_or(false)));
        let hit_limit = self
            .limit_reached
            .replace(reached)
            .is_some_and(|old| !old && reached);
        (stage_changed || hit_limit) && Instant::now() < self.expires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travel_limit_ticks_on_engagement_and_rearms_on_release_with_no_stage_bindings() {
        let settings = Settings::parse(&[1, 45, 35, 95, 85, 0, 100, 60]).unwrap();
        let mut preview = None;
        Preview::refresh(&mut preview, settings);
        let mut controls = Controls {
            left_trigger: 1.0,
            ..Controls::default()
        };
        for (travel, tick) in [
            (0.55, false),
            (0.60, true),
            (0.70, false),
            (0.596, false),
            (0.60, false),
            (0.58, false),
            (0.61, true),
        ] {
            controls.right_trigger = travel;
            assert_eq!(
                preview.as_mut().unwrap().crossed(&controls),
                tick,
                "{travel}"
            );
            Preview::refresh(&mut preview, settings);
        }
        Preview::refresh(
            &mut preview,
            Settings::parse(&[1, 45, 35, 95, 85, 0, 100, 50]).unwrap(),
        );
        assert!(
            !preview.as_mut().unwrap().crossed(&controls),
            "editing the limit is silent"
        );
        assert!(Settings::parse(&[1, 45, 35, 95, 85, 0, 100, 0]).is_none());
    }

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
