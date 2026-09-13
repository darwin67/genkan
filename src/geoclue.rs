//! Minimal GeoClue2 client for the logged-in desktop wallpaper process.
//!
//! GeoClue2 and its authorization agent are system-bus services; the client
//! requests city-level accuracy through the agent registered by the running
//! user session. It never exposes a provider selector, service URL, coordinate
//! field, or application-authorization knob. Raw service errors and
//! coordinates are deliberately absent from the public error surface so
//! callers cannot leak them into ordinary diagnostics.

use std::fmt;
use std::time::Duration;

use futures_util::StreamExt;
use thiserror::Error;

/// Application identity that NixOS authorizes in the user-session agent.
pub const DESKTOP_ID: &str = "genkan-wallpaper";
/// `GCLUE_ACCURACY_LEVEL_CITY`, the coarsest useful level for sunrise/sunset.
pub const CITY_ACCURACY_LEVEL: u32 = 2;
/// A fix coarser than this is treated as less precise than city accuracy.
/// GeoClue's city level is on the order of a metro area; country-level
/// GeoIP fixes are an order of magnitude larger and can shift sunrise and
/// sunset by several minutes.
pub const MAX_CITY_ACCURACY_METERS: f64 = 25_000.0;
const ACCESS_DENIED_NAME: &str = "org.freedesktop.DBus.Error.AccessDenied";

const SERVICE: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_INTERFACE: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT_INTERFACE: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION_INTERFACE: &str = "org.freedesktop.GeoClue2.Location";
const EMPTY_PATH: &str = "/";

/// A validated, city-level location. The value is retained in memory only.
#[derive(Clone, Copy, PartialEq)]
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

impl fmt::Debug for GeoLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeoLocation")
            .field("latitude_degrees", &"<redacted>")
            .field("longitude_degrees", &"<redacted>")
            .field("accuracy_meters", &self.accuracy_meters)
            .finish()
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

    /// Whether a later request could succeed. Transient bus, timeout, and
    /// unknown-accuracy failures retry on a bounded backoff; denial and
    /// coarser-than-city results stop solar for the process lifetime.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::Timeout | Self::Invalid)
    }
}

fn classify_bus_error(error: &zbus::Error) -> GeoClueError {
    if is_access_denied(error) {
        GeoClueError::Denied
    } else {
        GeoClueError::Unavailable
    }
}

fn classify_fdo_error(error: &zbus::fdo::Error) -> GeoClueError {
    if matches!(error, zbus::fdo::Error::AccessDenied(_)) {
        GeoClueError::Denied
    } else {
        GeoClueError::Unavailable
    }
}

fn is_access_denied(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, _, _) => is_access_denied_name(name.as_str()),
        zbus::Error::FDO(error) => matches!(**error, zbus::fdo::Error::AccessDenied(_)),
        _ => false,
    }
}

fn is_access_denied_name(name: &str) -> bool {
    name == ACCESS_DENIED_NAME
}

/// Requests one city-level fix, bounded by `timeout`.
pub async fn request_city_location(timeout: Duration) -> Result<GeoLocation, GeoClueError> {
    match tokio::time::timeout(timeout, resolve()).await {
        Ok(result) => result,
        Err(_) => Err(GeoClueError::Timeout),
    }
}

async fn resolve() -> Result<GeoLocation, GeoClueError> {
    let connection = zbus::Connection::system()
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
        .map_err(|error| classify_fdo_error(&error))?;
    client
        .set_property("RequestedAccuracyLevel", CITY_ACCURACY_LEVEL)
        .await
        .map_err(|error| classify_fdo_error(&error))?;
    // `Start` must expect a reply: a `NoReplyExpected` call cannot observe
    // the daemon's ACCESS_DENIED error and would loop until the outer timeout.
    client
        .call::<_, _, ()>("Start", &())
        .await
        .map_err(|error| classify_bus_error(&error))?;

    let result = await_location(&connection, &client).await;
    let _ = client.call_noreply("Stop", &()).await;
    result
}

/// Waits for the client's `LocationUpdated` signal.
///
/// The current `Location` property is read after subscribing so a fix that
/// arrived before the match rule was installed is still observed.
async fn await_location(
    connection: &zbus::Connection,
    client: &zbus::Proxy<'_>,
) -> Result<GeoLocation, GeoClueError> {
    let mut updates = client
        .receive_signal("LocationUpdated")
        .await
        .map_err(|_| GeoClueError::Unavailable)?;
    if let Some(location) = read_location(connection, client).await? {
        return Ok(location);
    }
    loop {
        let signal = updates.next().await.ok_or(GeoClueError::Unavailable)?;
        let (_, updated) = signal
            .body()
            .deserialize::<(
                zbus::zvariant::OwnedObjectPath,
                zbus::zvariant::OwnedObjectPath,
            )>()
            .map_err(|_| GeoClueError::Unavailable)?;
        if updated.as_str() != EMPTY_PATH {
            return read_location_at(connection, updated.as_str()).await;
        }
    }
}

async fn read_location(
    connection: &zbus::Connection,
    client: &zbus::Proxy<'_>,
) -> Result<Option<GeoLocation>, GeoClueError> {
    match client
        .get_property::<zbus::zvariant::OwnedObjectPath>("Location")
        .await
    {
        Ok(path) if path.as_str() != EMPTY_PATH => {
            read_location_at(connection, path.as_str()).await.map(Some)
        }
        _ => Ok(None),
    }
}

async fn read_location_at(
    connection: &zbus::Connection,
    path: &str,
) -> Result<GeoLocation, GeoClueError> {
    let location = zbus::Proxy::new(connection, SERVICE, path, LOCATION_INTERFACE)
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
    GeoLocation::new(latitude, longitude, accuracy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_request_uses_the_authorized_desktop_identity() {
        assert_eq!(DESKTOP_ID, "genkan-wallpaper");
        assert!(!DESKTOP_ID.contains(char::is_whitespace));
        assert_eq!(CITY_ACCURACY_LEVEL, 2, "GeoClue city accuracy is level 2");
        let module = include_str!("../nix/module.nix");
        assert!(module.contains(&format!("wallpaperDesktopId = \"{DESKTOP_ID}\"")));
        assert!(
            module.contains("appConfig.${wallpaperDesktopId}"),
            "the module must authorize the desktop id through appConfig"
        );
    }

    #[test]
    fn retryable_failures_are_bounded_and_terminal_failures_stop_solar() {
        assert!(GeoClueError::Unavailable.is_retryable());
        assert!(GeoClueError::Timeout.is_retryable());
        assert!(GeoClueError::Invalid.is_retryable());
        assert!(!GeoClueError::Denied.is_retryable());
        assert!(!GeoClueError::Coarse.is_retryable());
    }

    #[test]
    fn bus_errors_classify_denial_separately_from_availability() {
        let denied = zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied("no".into())));
        assert_eq!(classify_bus_error(&denied), GeoClueError::Denied);
        assert_eq!(
            classify_bus_error(&zbus::Error::Failure("boom".into())),
            GeoClueError::Unavailable
        );
        assert!(is_access_denied_name(ACCESS_DENIED_NAME));
        assert!(!is_access_denied_name(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        ));
        assert_eq!(
            classify_fdo_error(&zbus::fdo::Error::AccessDenied("no".into())),
            GeoClueError::Denied
        );
        assert_eq!(
            classify_fdo_error(&zbus::fdo::Error::Failed("x".into())),
            GeoClueError::Unavailable
        );
    }

    #[test]
    fn location_debug_redacts_coordinates() {
        let fix = GeoLocation::new(37.7749, -122.4194, 5_000.0).unwrap();
        let debug = format!("{fix:?}");
        assert!(!debug.contains("37.7749"), "{debug}");
        assert!(!debug.contains("122.4194"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
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
        assert_eq!(
            GeoLocation::new(0.0, 0.0, 50_000.0),
            Err(GeoClueError::Coarse),
            "country-level fixes must not be accepted as city-level"
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
