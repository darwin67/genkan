//! Deterministic solar trajectory sampling and authored-point mapping.
//!
//! This module is renderer- and service-neutral: it converts an authored
//! `solar` schedule into the same local wall-clock `h24` schedule consumed by
//! [`crate::dynamic_wallpaper::playback::Playback`]. Location acquisition is a
//! separate concern handled by the desktop runtime.
//!
//! Azimuth is measured clockwise from true north and altitude is the angle
//! above the horizon. The trajectory is sampled once per wall-clock minute and
//! each authored point is then refined to one-second resolution around its
//! nearest sample.

use std::fmt;

use super::{
    ClockSnapshot, Location, NormalizedTime, Schedule, SolarPoint, SolarPosition, TimePoint,
};

/// Wall-clock seconds in one civil day.
pub const SECONDS_PER_DAY: u32 = 86_400;
const MINUTES_PER_DAY: u32 = 1_440;
const NANOSECONDS_PER_SECOND: i128 = 1_000_000_000;
const REFINEMENT_RADIUS_SECONDS: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolarError {
    /// The authored schedule was unexpectedly empty.
    EmptySchedule,
    /// The local date or trajectory could not be represented.
    InvalidTrajectory,
}

impl fmt::Display for SolarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid solar schedule: {self:?}")
    }
}

impl std::error::Error for SolarError {}

/// A single sampled solar position expressed as a unit vector.
///
/// The axes are `(east, north, up)` so spherical angular distance is a dot
/// product that is independent of the chosen azimuth representation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    second: u32,
    vector: [f64; 3],
}

/// Maps every authored solar point to the nearest point on the day's actual
/// trajectory for `clock`'s local date and `location`.
///
/// Authored order is preserved. Points that map to the same wall-clock second
/// remain in the returned schedule in metadata order; the downstream `h24`
/// scheduler keeps the last duplicate at that boundary.
pub fn map_schedule(
    schedule: &Schedule<SolarPoint>,
    location: Location,
    clock: ClockSnapshot,
) -> Result<Schedule<TimePoint>, SolarError> {
    let base_utc = clock
        .utc_nanoseconds()
        .checked_sub(i128::from(seconds_of_day(clock)) * NANOSECONDS_PER_SECOND)
        .ok_or(SolarError::InvalidTrajectory)?;
    let mut mapped = Vec::with_capacity(schedule.points().len());
    for point in schedule.points() {
        let target = unit_vector(point.position);
        let second = nearest_second(location, base_utc, target)?;
        let time = NormalizedTime::new(f64::from(second) / f64::from(SECONDS_PER_DAY))
            .map_err(|_| SolarError::InvalidTrajectory)?;
        mapped.push(TimePoint {
            image: point.image,
            time,
        });
    }
    Schedule::new(mapped, schedule.appearance).map_err(|_| SolarError::EmptySchedule)
}

/// The full one-minute trajectory for the civil day containing `clock`.
///
/// The returned samples are ordered by ascending wall-clock second.
pub fn trajectory(
    location: Location,
    clock: ClockSnapshot,
) -> Result<Vec<(u32, SolarPosition)>, SolarError> {
    let base_utc = clock
        .utc_nanoseconds()
        .checked_sub(i128::from(seconds_of_day(clock)) * NANOSECONDS_PER_SECOND)
        .ok_or(SolarError::InvalidTrajectory)?;
    let mut samples = Vec::with_capacity(MINUTES_PER_DAY as usize);
    for minute in 0..MINUTES_PER_DAY {
        let second = minute * 60;
        samples.push((second, solar_position(location, base_utc, second)));
    }
    Ok(samples)
}

fn nearest_second(location: Location, base_utc: i128, target: [f64; 3]) -> Result<u32, SolarError> {
    let mut best: Option<Sample> = None;
    for minute in 0..MINUTES_PER_DAY {
        let second = minute * 60;
        best = nearer(
            best,
            Sample {
                second,
                vector: unit_vector(solar_position(location, base_utc, second)),
            },
            target,
        );
    }
    let coarse = best.ok_or(SolarError::InvalidTrajectory)?.second;
    let start = coarse.saturating_sub(REFINEMENT_RADIUS_SECONDS);
    let end = (coarse + REFINEMENT_RADIUS_SECONDS).min(SECONDS_PER_DAY - 1);
    let mut refined: Option<Sample> = None;
    for second in start..=end {
        refined = nearer(
            refined,
            Sample {
                second,
                vector: unit_vector(solar_position(location, base_utc, second)),
            },
            target,
        );
    }
    Ok(refined.ok_or(SolarError::InvalidTrajectory)?.second)
}

/// Keeps the earlier candidate when two samples are equidistant. The strict
/// comparison is what makes an exact angular-distance tie deterministic.
fn nearer(best: Option<Sample>, candidate: Sample, target: [f64; 3]) -> Option<Sample> {
    match best {
        Some(current) => {
            let current_distance = angular_distance(current.vector, target);
            let candidate_distance = angular_distance(candidate.vector, target);
            if candidate_distance < current_distance {
                Some(candidate)
            } else {
                Some(current)
            }
        }
        None => Some(candidate),
    }
}

fn seconds_of_day(clock: ClockSnapshot) -> u32 {
    let time = clock.time();
    u32::from(time.hour()) * 3_600 + u32::from(time.minute()) * 60 + u32::from(time.second())
}

/// Converts an altitude/azimuth into the `(east, north, up)` unit vector.
pub fn unit_vector(position: SolarPosition) -> [f64; 3] {
    let altitude = position.altitude_degrees().to_radians();
    let azimuth = position.azimuth_degrees().to_radians();
    [
        altitude.cos() * azimuth.sin(),
        altitude.cos() * azimuth.cos(),
        altitude.sin(),
    ]
}

fn angular_distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    let dot = left[0] * right[0] + left[1] * right[1] + left[2] * right[2];
    dot.clamp(-1.0, 1.0).acos()
}

fn solar_position(location: Location, base_utc: i128, second: u32) -> SolarPosition {
    let utc_seconds = (base_utc + i128::from(second) * NANOSECONDS_PER_SECOND) as f64
        / NANOSECONDS_PER_SECOND as f64;
    let (altitude, azimuth) = solar_altitude_azimuth(location, utc_seconds);
    let altitude = altitude.clamp(-90.0, 90.0);
    let azimuth = normalize_degrees(azimuth);
    SolarPosition::new(altitude, azimuth).expect("solar geometry stays within its domain")
}

fn normalize_degrees(value: f64) -> f64 {
    let value = value.rem_euclid(360.0);
    if value >= 360.0 {
        0.0
    } else {
        value
    }
}

/// NOAA solar position equations expressed directly in degrees.
fn solar_altitude_azimuth(location: Location, utc_seconds: f64) -> (f64, f64) {
    let julian_day = utc_seconds / 86_400.0 + 2_440_587.5;
    let century = (julian_day - 2_451_545.0) / 36_525.0;

    let mean_longitude =
        (280.46646 + century * (36_000.769_83 + century * 0.0003032)).rem_euclid(360.0);
    let mean_anomaly = 357.52911 + century * (35_999.050_29 - 0.0001537 * century);
    let eccentricity = 0.016708634 - century * (0.000042037 + 0.0000001267 * century);

    let anomaly = mean_anomaly.to_radians();
    let equation_of_center = anomaly.sin() * (1.914602 - century * (0.004817 + 0.000014 * century))
        + (2.0 * anomaly).sin() * (0.019993 - 0.000101 * century)
        + (3.0 * anomaly).sin() * 0.000289;
    let true_longitude = mean_longitude + equation_of_center;

    let omega = (125.04 - 1934.136 * century).to_radians();
    let apparent_longitude = true_longitude - 0.00569 - 0.00478 * omega.sin();

    let mean_obliquity = 23.0
        + (26.0 + (21.448 - century * (46.815 + century * (0.00059 - century * 0.001813))) / 60.0)
            / 60.0;
    let obliquity = mean_obliquity + 0.00256 * omega.cos();
    let declination = (obliquity.to_radians().sin() * apparent_longitude.to_radians().sin())
        .asin()
        .to_degrees();

    let y = (obliquity.to_radians() / 2.0).tan();
    let mean_longitude_rad = mean_longitude.to_radians();
    let equation_of_time = 4.0
        * (y * y * (2.0 * mean_longitude_rad).sin() - 2.0 * eccentricity * anomaly.sin()
            + 4.0 * eccentricity * y * anomaly.sin() * (2.0 * mean_longitude_rad).cos()
            - 0.5 * y * y * (4.0 * mean_longitude_rad).sin()
            - 1.25 * eccentricity * eccentricity * (2.0 * anomaly).sin())
        .to_degrees();

    let minutes = utc_seconds.rem_euclid(86_400.0) / 60.0;
    let true_solar_minutes = minutes + equation_of_time + 4.0 * location.longitude_degrees();
    let hour_angle = signed_hour_angle(true_solar_minutes);

    let latitude = location.latitude_degrees().to_radians();
    let declination_rad = declination.to_radians();
    let hour_angle_rad = hour_angle.to_radians();
    let sin_altitude = latitude.sin() * declination_rad.sin()
        + latitude.cos() * declination_rad.cos() * hour_angle_rad.cos();
    let altitude = sin_altitude.clamp(-1.0, 1.0).asin().to_degrees();
    let azimuth = hour_angle_rad
        .sin()
        .atan2(hour_angle_rad.cos() * latitude.sin() - declination_rad.tan() * latitude.cos())
        .to_degrees()
        + 180.0;
    (altitude, azimuth)
}

fn signed_hour_angle(true_solar_minutes: f64) -> f64 {
    let degrees = (true_solar_minutes / 4.0 - 180.0).rem_euclid(360.0);
    if degrees > 180.0 {
        degrees - 360.0
    } else {
        degrees
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_wallpaper::{
        Appearance, CivilDate, CivilTime, ImageReference, Schedule, SolarPoint,
    };

    fn clock_on(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> ClockSnapshot {
        ClockSnapshot::new(
            CivilDate::new(year, month, day).unwrap(),
            CivilTime::new(hour, minute, 0).unwrap(),
            0,
        )
        .unwrap()
    }

    fn location(latitude: f64, longitude: f64) -> Location {
        Location::new(latitude, longitude).unwrap()
    }

    fn midday(year: i32, month: u8, day: u8) -> ClockSnapshot {
        clock_on(year, month, day, 12, 0)
    }

    #[test]
    fn azimuth_is_clockwise_from_north_in_equatorial_equinox() {
        let equator = location(0.0, 0.0);
        let samples = trajectory(equator, midday(2026, 3, 20)).unwrap();
        let by_second = |second: u32| {
            samples
                .iter()
                .find(|(sample, _)| *sample == second)
                .map(|(_, position)| *position)
                .unwrap()
        };

        // At local solar noon the equinox sun is near the zenith. Its morning
        // rising and evening setting points are due east and west respectively.
        let noon = (0..MINUTES_PER_DAY)
            .map(|minute| by_second(minute * 60))
            .max_by(|left, right| {
                left.altitude_degrees()
                    .partial_cmp(&right.altitude_degrees())
                    .unwrap()
            })
            .unwrap();
        assert!(noon.altitude_degrees() > 89.0, "{noon:?}");

        let sunrise_sample = samples
            .iter()
            .min_by(|(_, left), (_, right)| {
                left.altitude_degrees()
                    .abs()
                    .partial_cmp(&right.altitude_degrees().abs())
                    .unwrap()
            })
            .map(|(_, position)| *position)
            .unwrap();
        assert!(sunrise_sample.altitude_degrees().abs() < 1.0);
        let sunrise_azimuth = sunrise_sample.azimuth_degrees();
        assert!(
            (sunrise_azimuth - 90.0).abs() < 2.0 || (sunrise_azimuth - 270.0).abs() < 2.0,
            "unexpected rising/setting azimuth {sunrise_azimuth}"
        );
    }

    #[test]
    fn northern_hemisphere_noon_is_due_south_and_afternoon_is_westerly() {
        let north = location(40.0, -75.0);
        let samples = trajectory(north, midday(2026, 6, 21)).unwrap();
        let noon_second = samples
            .iter()
            .max_by(|(_, left), (_, right)| {
                left.altitude_degrees()
                    .partial_cmp(&right.altitude_degrees())
                    .unwrap()
            })
            .map(|(second, _)| *second)
            .unwrap();
        let noon = samples
            .iter()
            .find(|(second, _)| *second == noon_second)
            .unwrap()
            .1;
        assert!((noon.azimuth_degrees() - 180.0).abs() < 2.0, "{noon:?}");
        // Solstice noon altitude is close to 90 - |latitude - declination|.
        assert!((noon.altitude_degrees() - 73.0).abs() < 2.0, "{noon:?}");

        // Longitude -75 is UTC-5, so 12:00 and 21:00 UTC are 07:00 and 16:00 local.
        let morning = samples
            .iter()
            .find(|(second, _)| *second == 12 * 3_600)
            .unwrap()
            .1;
        let afternoon = samples
            .iter()
            .find(|(second, _)| *second == 21 * 3_600)
            .unwrap()
            .1;
        assert!(morning.azimuth_degrees() < 180.0, "{morning:?}");
        assert!(afternoon.azimuth_degrees() > 180.0, "{afternoon:?}");
        assert!(
            morning.altitude_degrees() > 0.0 && afternoon.altitude_degrees() > 0.0,
            "{morning:?} {afternoon:?}"
        );
    }

    #[test]
    fn southern_hemisphere_noon_is_due_north() {
        let south = location(-33.87, 151.21);
        let samples = trajectory(south, midday(2026, 12, 21)).unwrap();
        let noon = samples
            .iter()
            .max_by(|(_, left), (_, right)| {
                left.altitude_degrees()
                    .partial_cmp(&right.altitude_degrees())
                    .unwrap()
            })
            .unwrap()
            .1;
        let azimuth = noon.azimuth_degrees();
        assert!(!(2.0..=358.0).contains(&azimuth), "{noon:?}");
    }

    #[test]
    fn polar_day_and_night_keep_their_computed_trajectory() {
        let north_pole_summer = trajectory(location(89.0, 0.0), midday(2026, 6, 21)).unwrap();
        assert!(north_pole_summer
            .iter()
            .all(|(_, position)| position.altitude_degrees() > 0.0));
        let north_pole_winter = trajectory(location(89.0, 0.0), midday(2026, 12, 21)).unwrap();
        assert!(north_pole_winter
            .iter()
            .all(|(_, position)| position.altitude_degrees() < 0.0));
    }

    #[test]
    fn polar_authored_points_map_to_the_computed_trajectory() {
        let north = location(89.0, 0.0);
        let day = midday(2026, 6, 21);
        let samples = trajectory(north, day).unwrap();
        let schedule = Schedule::new(
            vec![SolarPoint {
                image: ImageReference::from_position(0),
                position: samples[12 * 60].1,
            }],
            None,
        )
        .unwrap();
        let mapped = map_schedule(&schedule, north, day).unwrap();
        let expected = f64::from(12 * 3_600) / f64::from(SECONDS_PER_DAY);
        assert!(
            (mapped.points()[0].time.value() - expected).abs() < 1.0 / f64::from(SECONDS_PER_DAY)
        );
    }

    #[test]
    fn authored_points_map_to_their_actual_trajectory_time() {
        let north = location(40.0, -75.0);
        let day = midday(2026, 6, 21);
        let samples = trajectory(north, day).unwrap();
        let authored = [samples[5 * 60].1, samples[12 * 60].1, samples[19 * 60].1];
        let schedule = Schedule::new(
            authored
                .iter()
                .enumerate()
                .map(|(index, position)| SolarPoint {
                    image: ImageReference::from_position(index),
                    position: *position,
                })
                .collect(),
            Some(Appearance {
                light: ImageReference::from_position(0),
                dark: ImageReference::from_position(2),
            }),
        )
        .unwrap();

        let mapped = map_schedule(&schedule, north, day).unwrap();
        assert_eq!(mapped.points().len(), authored.len());
        for (index, point) in mapped.points().iter().enumerate() {
            let expected = f64::from([5_u32, 12, 19][index] * 3_600) / f64::from(SECONDS_PER_DAY);
            assert!(
                (point.time.value() - expected).abs() < 1.0 / f64::from(SECONDS_PER_DAY),
                "{index}: {} vs {expected}",
                point.time.value()
            );
            assert_eq!(point.image, ImageReference::from_position(index));
        }
        assert_eq!(
            mapped.appearance,
            Some(Appearance {
                light: ImageReference::from_position(0),
                dark: ImageReference::from_position(2),
            })
        );
    }

    #[test]
    fn duplicate_authored_times_preserve_metadata_order() {
        let north = location(40.0, -75.0);
        let day = midday(2026, 6, 21);
        let samples = trajectory(north, day).unwrap();
        let noon = samples[12 * 60].1;
        let schedule = Schedule::new(
            vec![
                SolarPoint {
                    image: ImageReference::from_position(7),
                    position: noon,
                },
                SolarPoint {
                    image: ImageReference::from_position(9),
                    position: noon,
                },
            ],
            None,
        )
        .unwrap();

        let mapped = map_schedule(&schedule, north, day).unwrap();
        assert_eq!(mapped.points()[0].time, mapped.points()[1].time);
        assert_eq!(mapped.points()[0].image, ImageReference::from_position(7));
        assert_eq!(mapped.points()[1].image, ImageReference::from_position(9));
    }

    #[test]
    fn nearest_selection_prefers_the_earlier_of_equal_samples() {
        let target = unit_vector(SolarPosition::new(0.0, 0.0).unwrap());
        let equal = |second: u32| Sample {
            second,
            vector: target,
        };
        let mut best: Option<Sample> = None;
        for sample in [equal(30), equal(90), equal(150)] {
            best = nearer(best, sample, target);
        }
        assert_eq!(best.unwrap().second, 30);

        let slightly_off = Sample {
            second: 60,
            vector: unit_vector(SolarPosition::new(0.5, 0.0).unwrap()),
        };
        let selected = nearer(Some(equal(30)), slightly_off, target).unwrap();
        assert_eq!(selected.second, 30);
    }

    #[test]
    fn unit_vectors_encode_the_north_clockwise_azimuth_convention() {
        let east = unit_vector(SolarPosition::new(0.0, 90.0).unwrap());
        assert!((east[0] - 1.0).abs() < 1e-12);
        assert!(east[1].abs() < 1e-12);
        let north = unit_vector(SolarPosition::new(0.0, 0.0).unwrap());
        assert!((north[1] - 1.0).abs() < 1e-12);
        let up = unit_vector(SolarPosition::new(90.0, 0.0).unwrap());
        assert!((up[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn angular_distance_is_symmetric_and_zero_for_identical_vectors() {
        let left = unit_vector(SolarPosition::new(12.0, 34.0).unwrap());
        let right = unit_vector(SolarPosition::new(-40.0, 200.0).unwrap());
        assert!(angular_distance(left, left).abs() < 1e-12);
        assert!((angular_distance(left, right) - angular_distance(right, left)).abs() < 1e-12);
    }

    #[test]
    fn clock_date_midnight_is_the_trajectory_base() {
        let north = location(48.85, 2.35);
        let evening = clock_on(2026, 9, 10, 22, 30);
        let samples = trajectory(north, evening).unwrap();
        assert_eq!(samples.len(), MINUTES_PER_DAY as usize);
        assert_eq!(samples[0].0, 0);
        assert_eq!(
            samples[MINUTES_PER_DAY as usize - 1].0,
            (MINUTES_PER_DAY - 1) * 60
        );
    }
}
