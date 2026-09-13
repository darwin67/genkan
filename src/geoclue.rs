//! Minimal GeoClue2 client for the logged-in desktop wallpaper process.
//!
//! The client requests city-level accuracy through the user session's GeoClue
//! agent and never exposes a provider selector, service URL, coordinate field,
//! or application-authorization knob. Raw service errors and coordinates are
//! deliberately absent from the public error surface so callers cannot leak
//! them into ordinary diagnostics.

use std::time::Duration;

use thiserror::Error;

/// Application identity that NixOS authorizes in the user-session agent.
pub const DESKTOP_ID: &str = "genkan-wallpaper";
/// `GCLUE_ACCURACY_LEVEL_CITY`, the coarsest useful level for sunrise/sunset.
pub const CITY_ACCURACY_LEVEL: u32 = 2;
/// A fix coarser than this is treated as less precise than city accuracy.
pub const MAX_CITY_ACCURACY_METERS: f64 = 100_000.0;

const SERVICE: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_INTERFACE: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT_INTERFACE: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION_INTERFACE: &str = "org.freedesktop.GeoClue2.Location";
const EMPTY_PATH: &str = "/";
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A validated, city-level location. The value is retained in memory only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoLocation {
    latitude_degrees: f64,
    longitude_degrees: f64,
    accuracy_meters: f64,
}

impl GeoLocation {
    pub fn new(
        latitude_degrees: f64,
        longitude_degrees: f64,
        accuracy_meters: f64,
    ) -> Result<Self, GeoClueError> {
        if !latitude_degrees.is_finite()
            || !(-90.0..=90.0).contains(&latitude_degrees)
            || !longitude_degrees.is_finite()
            || !(-180.0..=180.0).contains(&longitude_degrees)
            || !accuracy_meters.is_finite()
            || accuracy_meters < 0.0
        {
            return Err(GeoClueError::Invalid);
        }
        if accuracy_meters > MAX_CITY_ACCURACY_METERS {
            return Err(GeoClueError::Coarse);
        }
        Ok(Self {
            latitude_degrees,
            longitude_degrees,
            accuracy_meters,
        })
    }

    pub const fn latitude_degrees(self) -> f64 {
        self.latitude_degrees
    }

    pub const fn longitude_degrees(self) -> f64 {
        self.longitude_degrees
    }
}

/// Bounded, non-identifying failure categories.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum GeoClueError {
    #[error("GeoClue is unavailable")]
    Unavailable,
    #[error("GeoClue did not return a location in time")]
    Timeout,
    #[error("GeoClue authorization was denied")]
    Denied,
    #[error("GeoClue returned an invalid location")]
    Invalid,
    #[error("GeoClue location was coarser than city accuracy")]
    Coarse,
}

impl GeoClueError {
    /// A single lowercase category suitable for bounded diagnostics. It never
    /// contains coordinates, network observations, metadata, or raw D-Bus text.
    pub const fn category(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Denied => "denied",
            Self::Invalid => "invalid",
            Self::Coarse => "coarse",
        }
    }
}

/// Requests one city-level fix, bounded by `timeout`.
pub async fn request_city_location(timeout: Duration) -> Result<GeoLocation, GeoClueError> {
    match tokio::time::timeout(timeout, resolve()).await {
        Ok(result) => result,
        Err(_) => Err(GeoClueError::Timeout),
    }
}

async fn resolve() -> Result<GeoLocation, GeoClueError> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    let manager = zbus::Proxy::new(&connection, SERVICE, MANAGER_PATH, MANAGER_INTERFACE)
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    let client_path: zbus::zvariant::OwnedObjectPath = manager
        .call("GetClient", &())
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    let client = zbus::Proxy::new(&connection, SERVICE, client_path.as_str(), CLIENT_INTERFACE)
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    client
        .set_property("DesktopId", DESKTOP_ID.to_owned())
        .await
        .map_err(|_| GeoClueError::Denied)?;
    client
        .set_property("RequestedAccuracyLevel", CITY_ACCURACY_LEVEL)
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    client
        .call_noreply("Start", &())
        .await
        .map_err(|_| GeoClueError::Denied)?;

    let result = poll_location(&connection, &client).await;
    let _ = client.call_noreply("Stop", &()).await;
    result
}

async fn poll_location(
    connection: &zbus::Connection,
    client: &zbus::Proxy<'_>,
) -> Result<GeoLocation, GeoClueError> {
    loop {
        if let Ok(path) = client
            .get_property::<zbus::zvariant::OwnedObjectPath>("Location")
            .await
        {
            if path.as_str() != EMPTY_PATH {
                let location =
                    zbus::Proxy::new(connection, SERVICE, path.as_str(), LOCATION_INTERFACE)
                        .await
                        .map_err(|_| GeoClueError::Unavailable)?;
                let latitude: f64 = location
                    .get_property("Latitude")
                    .await
                    .map_err(|_| GeoClueError::Unavailable)?;
                let longitude: f64 = location
                    .get_property("Longitude")
                    .await
                    .map_err(|_| GeoClueError::Unavailable)?;
                let accuracy: f64 = location
                    .get_property("Accuracy")
                    .await
                    .map_err(|_| GeoClueError::Unavailable)?;
                return GeoLocation::new(latitude, longitude, accuracy);
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_request_uses_the_authorized_desktop_identity() {
        assert_eq!(DESKTOP_ID, "genkan-wallpaper");
        assert!(!DESKTOP_ID.contains(char::is_whitespace));
        assert_eq!(CITY_ACCURACY_LEVEL, 2, "GeoClue city accuracy is level 2");
    }

    #[test]
    fn location_accepts_city_fixes_and_rejects_invalid_or_coarse_values() {
        let fix = GeoLocation::new(37.7749, -122.4194, 5_000.0).unwrap();
        assert_eq!(fix.latitude_degrees(), 37.7749);
        assert_eq!(fix.longitude_degrees(), -122.4194);
        assert_eq!(fix.accuracy_meters, 5_000.0);

        assert_eq!(GeoLocation::new(91.0, 0.0, 1.0), Err(GeoClueError::Invalid));
        assert_eq!(
            GeoLocation::new(-91.0, 0.0, 1.0),
            Err(GeoClueError::Invalid)
        );
        assert_eq!(
            GeoLocation::new(0.0, 181.0, 1.0),
            Err(GeoClueError::Invalid)
        );
        assert_eq!(
            GeoLocation::new(f64::NAN, 0.0, 1.0),
            Err(GeoClueError::Invalid)
        );
        assert_eq!(GeoLocation::new(0.0, 0.0, -1.0), Err(GeoClueError::Invalid));
        assert_eq!(
            GeoLocation::new(0.0, 0.0, f64::INFINITY),
            Err(GeoClueError::Invalid)
        );
        assert_eq!(
            GeoLocation::new(0.0, 0.0, MAX_CITY_ACCURACY_METERS + 1.0),
            Err(GeoClueError::Coarse)
        );
        assert!(GeoLocation::new(0.0, 0.0, MAX_CITY_ACCURACY_METERS).is_ok());
    }

    #[test]
    fn error_categories_are_bounded_and_do_not_echo_service_text() {
        assert_eq!(GeoClueError::Unavailable.category(), "unavailable");
        assert_eq!(GeoClueError::Timeout.category(), "timeout");
        assert_eq!(GeoClueError::Denied.category(), "denied");
        assert_eq!(GeoClueError::Invalid.category(), "invalid");
        assert_eq!(GeoClueError::Coarse.category(), "coarse");
        for error in [
            GeoClueError::Unavailable,
            GeoClueError::Timeout,
            GeoClueError::Denied,
            GeoClueError::Invalid,
            GeoClueError::Coarse,
        ] {
            assert!(error.to_string().len() < 64);
            assert!(!error.to_string().contains("org.freedesktop"));
        }
    }
}
