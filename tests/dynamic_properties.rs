#![cfg(feature = "gui")]

use genkan::dynamic_wallpaper::{playback::Playback, *};
use proptest::prelude::*;
use std::time::Duration;

fn clock(second: u32) -> ClockSnapshot {
    ClockSnapshot::new(
        CivilDate::new(2026, 11, 1).unwrap(),
        CivilTime::new(
            (second / 3600) as u8,
            (second / 60 % 60) as u8,
            (second % 60) as u8,
        )
        .unwrap(),
        0,
    )
    .unwrap()
}

fn metadata(boundaries: &[u32]) -> Metadata {
    let schedule = Schedule::new(
        boundaries
            .iter()
            .enumerate()
            .map(|(id, second)| TimePoint {
                image: ImageReference::from_position(id),
                time: NormalizedTime::new(f64::from(*second) / 86_400.0).unwrap(),
            })
            .collect(),
        None,
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata
        .insert(AppleProperty::Time, PropertyValue::Time(schedule))
        .unwrap();
    metadata
}

fn expected(boundaries: &[u32], second: u32) -> usize {
    // Independent circular-interval oracle. Index breaks duplicate ties in
    // authored order (the last authored image owns that boundary).
    boundaries
        .iter()
        .enumerate()
        .filter(|(_, time)| **time <= second)
        .max_by_key(|(id, time)| (**time, *id))
        .or_else(|| {
            boundaries
                .iter()
                .enumerate()
                .max_by_key(|(id, time)| (**time, *id))
        })
        .unwrap()
        .0
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn h24_selects_one_image_and_changes_only_at_boundaries(
        boundaries in prop::collection::vec(0u32..86_400, 1..32), second in 0u32..86_400,
    ) {
        let metadata = metadata(&boundaries);
        let mut playback = Playback::new(&metadata, ImageReference::from_position(999), AppearancePreference::Automatic, clock(second), Duration::ZERO, true);
        prop_assert_eq!(playback.selected().position(), expected(&boundaries, second));
        let next = (second + 1) % 86_400;
        let previous = playback.selected();
        playback.synchronize(clock(next), Duration::from_secs(1));
        prop_assert_eq!(playback.selected().position(), expected(&boundaries, next));
        if playback.selected() != previous { prop_assert!(boundaries.contains(&next)); }
        for boundary in &boundaries {
            playback.synchronize(clock(*boundary), Duration::from_secs(2));
            prop_assert_eq!(playback.selected().position(), expected(&boundaries, *boundary));
        }
    }
}

#[test]
fn every_second_has_an_image_in_wrapping_duplicate_schedule() {
    let boundaries = [43_200, 120, 43_200, 86_399];
    let mut playback = Playback::new(
        &metadata(&boundaries),
        ImageReference::from_position(999),
        AppearancePreference::Automatic,
        clock(0),
        Duration::ZERO,
        true,
    );
    for second in 0..86_400 {
        playback.synchronize(clock(second), Duration::from_secs(u64::from(second)));
        assert_eq!(
            playback.selected().position(),
            expected(&boundaries, second)
        );
    }
    assert!(Schedule::<TimePoint>::new(vec![], None).is_err());
}
