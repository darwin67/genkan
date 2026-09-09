//! Renderer- and service-neutral model for macOS-style dynamic wallpapers.

use std::fmt;

pub const APPLE_DESKTOP_NAMESPACE: &str = "http://ns.apple.com/namespace/1.0/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleProperty {
    Time,
    Solar,
    Appearance,
}

impl AppleProperty {
    pub fn from_expanded_name(namespace: &str, local_name: &str) -> Option<Self> {
        if namespace != APPLE_DESKTOP_NAMESPACE {
            return None;
        }

        match local_name {
            "h24" => Some(Self::Time),
            "solar" => Some(Self::Solar),
            "apr" => Some(Self::Appearance),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageReference(usize);

impl ImageReference {
    pub const fn from_position(position: usize) -> Self {
        Self(position)
    }

    pub const fn position(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedTime(f64);

impl NormalizedTime {
    pub fn new(value: f64) -> Result<Self, ModelError> {
        if value.is_finite() && (0.0..1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ModelError::InvalidNormalizedTime)
        }
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolarPosition {
    altitude_degrees: f64,
    azimuth_degrees: f64,
}

impl SolarPosition {
    pub fn new(altitude_degrees: f64, azimuth_degrees: f64) -> Result<Self, ModelError> {
        if !altitude_degrees.is_finite() || !(-90.0..=90.0).contains(&altitude_degrees) {
            return Err(ModelError::InvalidSolarAltitude);
        }
        if !azimuth_degrees.is_finite() || !(0.0..360.0).contains(&azimuth_degrees) {
            return Err(ModelError::InvalidSolarAzimuth);
        }

        Ok(Self {
            altitude_degrees,
            azimuth_degrees,
        })
    }

    pub const fn altitude_degrees(self) -> f64 {
        self.altitude_degrees
    }

    pub const fn azimuth_degrees(self) -> f64 {
        self.azimuth_degrees
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    pub light: ImageReference,
    pub dark: ImageReference,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimePoint {
    pub image: ImageReference,
    pub time: NormalizedTime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolarPoint {
    pub image: ImageReference,
    pub position: SolarPosition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule<T> {
    points: Vec<T>,
    pub appearance: Option<Appearance>,
}

impl<T> Schedule<T> {
    pub fn new(points: Vec<T>, appearance: Option<Appearance>) -> Result<Self, ModelError> {
        if points.is_empty() {
            Err(ModelError::EmptySchedule)
        } else {
            Ok(Self { points, appearance })
        }
    }

    pub fn points(&self) -> &[T] {
        &self.points
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Time(Schedule<TimePoint>),
    Solar(Schedule<SolarPoint>),
    Appearance(Appearance),
}

impl PropertyValue {
    const fn property(&self) -> AppleProperty {
        match self {
            Self::Time(_) => AppleProperty::Time,
            Self::Solar(_) => AppleProperty::Solar,
            Self::Appearance(_) => AppleProperty::Appearance,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    time: Option<Schedule<TimePoint>>,
    solar: Option<Schedule<SolarPoint>>,
    appearance: Option<Appearance>,
}

impl Metadata {
    pub fn insert_expanded(
        &mut self,
        namespace: &str,
        local_name: &str,
        value: PropertyValue,
    ) -> Result<bool, ModelError> {
        let Some(property) = AppleProperty::from_expanded_name(namespace, local_name) else {
            return Ok(false);
        };
        self.insert(property, value)?;
        Ok(true)
    }

    pub fn insert(
        &mut self,
        property: AppleProperty,
        value: PropertyValue,
    ) -> Result<(), ModelError> {
        if value.property() != property {
            return Err(ModelError::PropertyTypeMismatch);
        }

        let occupied = match property {
            AppleProperty::Time => self.time.is_some(),
            AppleProperty::Solar => self.solar.is_some(),
            AppleProperty::Appearance => self.appearance.is_some(),
        };
        if occupied {
            return Err(ModelError::DuplicateProperty(property));
        }

        match value {
            PropertyValue::Time(schedule) => self.time = Some(schedule),
            PropertyValue::Solar(schedule) => self.solar = Some(schedule),
            PropertyValue::Appearance(appearance) => self.appearance = Some(appearance),
        }
        Ok(())
    }

    pub const fn time(&self) -> Option<&Schedule<TimePoint>> {
        self.time.as_ref()
    }

    pub const fn solar(&self) -> Option<&Schedule<SolarPoint>> {
        self.solar.as_ref()
    }

    pub const fn appearance(&self) -> Option<Appearance> {
        self.appearance
    }

    pub fn select(
        &self,
        appearance: AppearancePreference,
        solar_enabled: bool,
        location_available: bool,
    ) -> Selection<'_> {
        if appearance != AppearancePreference::Automatic {
            return self.fallback_appearance().map_or(
                Selection::Primary,
                |pair| match appearance {
                    AppearancePreference::Light => Selection::Static(pair.light),
                    AppearancePreference::Dark => Selection::Static(pair.dark),
                    AppearancePreference::Automatic => unreachable!(),
                },
            );
        }

        if solar_enabled && location_available {
            if let Some(schedule) = self.solar.as_ref() {
                return Selection::Solar(schedule);
            }
        }
        if let Some(schedule) = self.time.as_ref() {
            return Selection::Time(schedule);
        }

        self.fallback_appearance()
            .map_or(Selection::Primary, |pair| Selection::Static(pair.light))
    }

    fn fallback_appearance(&self) -> Option<Appearance> {
        self.appearance
            .or_else(|| self.time.as_ref().and_then(|value| value.appearance))
            .or_else(|| self.solar.as_ref().and_then(|value| value.appearance))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AppearancePreference {
    #[default]
    Automatic,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Selection<'a> {
    Solar(&'a Schedule<SolarPoint>),
    Time(&'a Schedule<TimePoint>),
    Static(ImageReference),
    Primary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl CivilDate {
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, ModelError> {
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap_year(year) => 29,
            2 => 28,
            _ => return Err(ModelError::InvalidCivilDate),
        };
        if day == 0 || day > days {
            return Err(ModelError::InvalidCivilDate);
        }

        Ok(Self { year, month, day })
    }
}

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl CivilTime {
    pub fn new(hour: u8, minute: u8, second: u8) -> Result<Self, ModelError> {
        if hour < 24 && minute < 60 && second < 60 {
            Ok(Self {
                hour,
                minute,
                second,
            })
        } else {
            Err(ModelError::InvalidCivilTime)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSnapshot {
    pub date: CivilDate,
    pub time: CivilTime,
    pub utc_offset_seconds: i32,
}

impl ClockSnapshot {
    pub fn new(
        date: CivilDate,
        time: CivilTime,
        utc_offset_seconds: i32,
    ) -> Result<Self, ModelError> {
        if !(-86_400..86_400).contains(&utc_offset_seconds) {
            return Err(ModelError::InvalidUtcOffset);
        }
        Ok(Self {
            date,
            time,
            utc_offset_seconds,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    latitude_degrees: f64,
    longitude_degrees: f64,
}

impl Location {
    pub fn new(latitude_degrees: f64, longitude_degrees: f64) -> Result<Self, ModelError> {
        if !latitude_degrees.is_finite() || !(-90.0..=90.0).contains(&latitude_degrees) {
            return Err(ModelError::InvalidLatitude);
        }
        if !longitude_degrees.is_finite() || !(-180.0..=180.0).contains(&longitude_degrees) {
            return Err(ModelError::InvalidLongitude);
        }
        Ok(Self {
            latitude_degrees,
            longitude_degrees,
        })
    }

    pub const fn latitude_degrees(self) -> f64 {
        self.latitude_degrees
    }

    pub const fn longitude_degrees(self) -> f64 {
        self.longitude_degrees
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelError {
    DuplicateProperty(AppleProperty),
    EmptySchedule,
    InvalidCivilDate,
    InvalidCivilTime,
    InvalidLatitude,
    InvalidLongitude,
    InvalidNormalizedTime,
    InvalidSolarAltitude,
    InvalidSolarAzimuth,
    InvalidUtcOffset,
    PropertyTypeMismatch,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid dynamic wallpaper model: {self:?}")
    }
}

impl std::error::Error for ModelError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(position: usize) -> ImageReference {
        ImageReference::from_position(position)
    }

    fn appearance() -> Appearance {
        Appearance {
            light: image(0),
            dark: image(3),
        }
    }

    fn time_schedule() -> Schedule<TimePoint> {
        Schedule::new(
            vec![TimePoint {
                image: image(1),
                time: NormalizedTime::new(0.25).unwrap(),
            }],
            Some(appearance()),
        )
        .unwrap()
    }

    fn solar_schedule() -> Schedule<SolarPoint> {
        Schedule::new(
            vec![SolarPoint {
                image: image(2),
                position: SolarPosition::new(35.0, 110.0).unwrap(),
            }],
            Some(appearance()),
        )
        .unwrap()
    }

    fn different_time_schedule() -> Schedule<TimePoint> {
        Schedule::new(
            vec![TimePoint {
                image: image(3),
                time: NormalizedTime::new(0.75).unwrap(),
            }],
            Some(Appearance {
                light: image(1),
                dark: image(2),
            }),
        )
        .unwrap()
    }

    #[test]
    fn apple_properties_use_expanded_names_not_prefixes() {
        assert_eq!(
            AppleProperty::from_expanded_name(APPLE_DESKTOP_NAMESPACE, "h24"),
            Some(AppleProperty::Time)
        );
        assert_eq!(
            AppleProperty::from_expanded_name(APPLE_DESKTOP_NAMESPACE, "solar"),
            Some(AppleProperty::Solar)
        );
        assert_eq!(
            AppleProperty::from_expanded_name(APPLE_DESKTOP_NAMESPACE, "apr"),
            Some(AppleProperty::Appearance)
        );
        assert_eq!(
            AppleProperty::from_expanded_name("https://example.com/apple_desktop", "h24"),
            None
        );
        assert_eq!(
            AppleProperty::from_expanded_name(APPLE_DESKTOP_NAMESPACE, "future"),
            None
        );
    }

    #[test]
    fn equivalent_attribute_and_element_forms_enter_the_same_model_path() {
        enum XmpForm {
            Attribute,
            Element,
        }

        for form in [XmpForm::Attribute, XmpForm::Element] {
            let mut metadata = Metadata::default();
            assert_eq!(
                metadata.insert_expanded(
                    APPLE_DESKTOP_NAMESPACE,
                    "h24",
                    PropertyValue::Time(time_schedule()),
                ),
                Ok(true),
                "XMP {form_name} form must use the expanded property name",
                form_name = match form {
                    XmpForm::Attribute => "attribute",
                    XmpForm::Element => "element",
                }
            );
            assert!(metadata.time().is_some());
        }
    }

    #[test]
    fn positions_are_zero_based_collection_positions() {
        assert_eq!(image(0).position(), 0);
        assert_eq!(image(3).position(), 3);
    }

    #[test]
    fn standalone_apr_and_embedded_ap_remain_distinct() {
        let standalone = Appearance {
            light: image(0),
            dark: image(1),
        };
        let embedded = Appearance {
            light: image(2),
            dark: image(3),
        };
        let metadata = Metadata {
            time: Some(Schedule::new(time_schedule().points, Some(embedded)).unwrap()),
            appearance: Some(standalone),
            ..Metadata::default()
        };

        assert_eq!(metadata.appearance(), Some(standalone));
        assert_eq!(metadata.time().unwrap().appearance, Some(embedded));
    }

    #[test]
    fn identical_duplicate_properties_are_idempotent() {
        let mut metadata = Metadata::default();
        let original = time_schedule();
        metadata
            .insert(AppleProperty::Time, PropertyValue::Time(original.clone()))
            .unwrap();

        assert_eq!(
            metadata.insert(AppleProperty::Time, PropertyValue::Time(original.clone())),
            Ok(())
        );
        assert_eq!(metadata.time(), Some(&original));
    }

    #[test]
    fn conflicting_duplicate_permanently_invalidates_only_that_property() {
        let mut metadata = Metadata::default();
        let original = time_schedule();
        metadata
            .insert(AppleProperty::Time, PropertyValue::Time(original.clone()))
            .unwrap();
        metadata
            .insert(
                AppleProperty::Appearance,
                PropertyValue::Appearance(appearance()),
            )
            .unwrap();

        assert_eq!(
            metadata.insert(
                AppleProperty::Time,
                PropertyValue::Time(different_time_schedule()),
            ),
            Err(ModelError::DuplicateProperty(AppleProperty::Time))
        );
        assert_eq!(metadata.time(), None);
        assert_eq!(
            metadata.select(AppearancePreference::Automatic, false, false),
            Selection::Static(image(0))
        );

        assert_eq!(
            metadata.insert(AppleProperty::Time, PropertyValue::Time(original)),
            Err(ModelError::DuplicateProperty(AppleProperty::Time))
        );
        assert_eq!(metadata.time(), None);
    }

    #[test]
    fn explicit_appearance_suppresses_dynamic_schedules() {
        let metadata = Metadata {
            time: Some(time_schedule()),
            solar: Some(solar_schedule()),
            appearance: Some(appearance()),
        };

        assert_eq!(
            metadata.select(AppearancePreference::Dark, true, true),
            Selection::Static(image(3))
        );
    }

    #[test]
    fn solar_requires_opt_in_and_location_then_precedes_time() {
        let metadata = Metadata {
            time: Some(time_schedule()),
            solar: Some(solar_schedule()),
            appearance: Some(appearance()),
        };

        assert!(matches!(
            metadata.select(AppearancePreference::Automatic, true, true),
            Selection::Solar(_)
        ));
        assert!(matches!(
            metadata.select(AppearancePreference::Automatic, false, true),
            Selection::Time(_)
        ));
        assert!(matches!(
            metadata.select(AppearancePreference::Automatic, true, false),
            Selection::Time(_)
        ));
    }

    #[test]
    fn appearance_then_primary_are_static_fallbacks() {
        let metadata = Metadata {
            appearance: Some(appearance()),
            ..Metadata::default()
        };
        assert_eq!(
            metadata.select(AppearancePreference::Automatic, true, false),
            Selection::Static(image(0))
        );
        assert_eq!(
            Metadata::default().select(AppearancePreference::Automatic, false, false),
            Selection::Primary
        );
    }

    #[test]
    fn appearance_fallback_precedence_uses_distinguishable_pairs() {
        let standalone = Appearance {
            light: image(0),
            dark: image(1),
        };
        let embedded_time = Appearance {
            light: image(2),
            dark: image(3),
        };
        let embedded_solar = Appearance {
            light: image(4),
            dark: image(5),
        };
        let metadata = Metadata {
            time: Some(Schedule::new(time_schedule().points, Some(embedded_time)).unwrap()),
            solar: Some(Schedule::new(solar_schedule().points, Some(embedded_solar)).unwrap()),
            appearance: Some(standalone),
        };

        assert_eq!(
            metadata.select(AppearancePreference::Dark, true, true),
            Selection::Static(image(1))
        );

        let metadata = Metadata {
            appearance: None,
            ..metadata
        };
        assert_eq!(
            metadata.select(AppearancePreference::Light, false, false),
            Selection::Static(image(2))
        );

        let metadata = Metadata {
            time: None,
            ..metadata
        };
        assert_eq!(
            metadata.select(AppearancePreference::Dark, false, false),
            Selection::Static(image(5))
        );
    }

    #[test]
    fn model_rejects_out_of_domain_values() {
        assert_eq!(
            NormalizedTime::new(1.0),
            Err(ModelError::InvalidNormalizedTime)
        );
        assert_eq!(
            SolarPosition::new(91.0, 0.0),
            Err(ModelError::InvalidSolarAltitude)
        );
        assert_eq!(
            SolarPosition::new(0.0, 360.0),
            Err(ModelError::InvalidSolarAzimuth)
        );
        assert_eq!(
            Location::new(f64::NAN, 0.0),
            Err(ModelError::InvalidLatitude)
        );
        assert_eq!(
            Schedule::<TimePoint>::new(Vec::new(), None),
            Err(ModelError::EmptySchedule)
        );
    }

    #[test]
    fn deterministic_clock_and_location_inputs_are_plain_values() {
        let snapshot = ClockSnapshot::new(
            CivilDate::new(2024, 2, 29).unwrap(),
            CivilTime::new(6, 30, 15).unwrap(),
            -8 * 60 * 60,
        )
        .unwrap();
        let location = Location::new(37.7749, -122.4194).unwrap();

        assert_eq!(snapshot.date.day, 29);
        assert_eq!(snapshot.time.hour, 6);
        assert_eq!(snapshot.utc_offset_seconds, -28_800);
        assert_eq!(location.latitude_degrees(), 37.7749);
        assert_eq!(location.longitude_degrees(), -122.4194);
        assert_eq!(
            CivilDate::new(2023, 2, 29),
            Err(ModelError::InvalidCivilDate)
        );
    }

    #[test]
    fn civil_values_accept_and_reject_asymmetric_boundaries() {
        assert!(CivilDate::new(2000, 2, 29).is_ok());
        assert_eq!(
            CivilDate::new(1900, 2, 29),
            Err(ModelError::InvalidCivilDate)
        );
        assert!(CivilTime::new(23, 59, 59).is_ok());
        assert_eq!(
            CivilTime::new(24, 0, 0),
            Err(ModelError::InvalidCivilTime)
        );
        assert!(ClockSnapshot::new(
            CivilDate::new(2026, 9, 9).unwrap(),
            CivilTime::new(12, 0, 0).unwrap(),
            86_399,
        )
        .is_ok());
        assert_eq!(
            ClockSnapshot::new(
                CivilDate::new(2026, 9, 9).unwrap(),
                CivilTime::new(12, 0, 0).unwrap(),
                86_400,
            ),
            Err(ModelError::InvalidUtcOffset)
        );
    }
}
