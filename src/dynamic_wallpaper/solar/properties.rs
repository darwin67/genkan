use super::*;
use crate::dynamic_wallpaper::{CivilDate, CivilTime, ImageReference};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn solar_positions_and_mappings_are_bounded(
        latitude in -90.0f64..=90.0, longitude in -180.0f64..=180.0,
        year in any::<i32>(), month in 1u8..=12, day in 1u8..=28,
        second in 0u32..86_400, offset in -86_399i32..=86_399,
        points in prop::collection::vec((-90.0f64..=90.0, 0.0f64..360.0), 1..12),
    ) {
        let location = Location::new(latitude, longitude).unwrap();
        let clock = ClockSnapshot::new(CivilDate::new(year, month, day).unwrap(),
            CivilTime::new((second / 3600) as u8, (second / 60 % 60) as u8, (second % 60) as u8).unwrap(), offset).unwrap();
        let position = solar_position(location, clock.utc_nanoseconds(), 0).unwrap();
        prop_assert!((-90.0..=90.0).contains(&position.altitude_degrees()));
        prop_assert!((0.0..360.0).contains(&position.azimuth_degrees()));
        let schedule = Schedule::new(points.iter().enumerate().map(|(id, &(alt, az))| SolarPoint {
            image: ImageReference::from_position(id), position: SolarPosition::new(alt, az).unwrap(),
        }).collect(), None).unwrap();
        let mapped = map_schedule(&schedule, location, clock).unwrap();
        prop_assert_eq!(mapped.len(), points.len());
        for (id, point) in mapped.iter().enumerate() {
            prop_assert_eq!(point.image.position(), id);
            prop_assert!((0.0..1.0).contains(&point.time.value()));
        }
    }

    #[test]
    fn invalid_floats_are_rejected_without_panicking(a in any::<f64>(), b in any::<f64>()) {
        prop_assert_eq!(Location::new(a,b).is_ok(), a.is_finite() && b.is_finite() && (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&b));
        prop_assert_eq!(NormalizedTime::new(a).is_ok(), a.is_finite() && (0.0..1.0).contains(&a));
        let _ = SolarPosition::new(a, b);
    }
}

#[test]
fn polar_trajectories_include_midnight_sun_and_polar_night() {
    for latitude in [-90.0, 90.0] {
        for month in [6, 12] {
            let clock = ClockSnapshot::new(
                CivilDate::new(2026, month, 21).unwrap(),
                CivilTime::new(0, 0, 0).unwrap(),
                0,
            )
            .unwrap();
            let samples = trajectory(Location::new(latitude, 0.0).unwrap(), clock).unwrap();
            assert_eq!(samples.len(), 1440);
            let summer = (latitude > 0.0) == (month == 6);
            assert!(samples
                .iter()
                .all(|(_, p)| (p.altitude_degrees() > 0.0) == summer));
        }
    }
}
