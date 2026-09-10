//! Deterministic `h24` selection and bounded transition state.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use super::heic::RgbaFrame;
use super::{AppearancePreference, ClockSnapshot, ImageReference, Metadata, Selection, TimePoint};

const DAY_SECONDS: u32 = 86_400;
const RESYNCHRONIZE_AFTER: Duration = Duration::from_secs(60);
const DISSOLVE_DURATION: Duration = Duration::from_secs(2);
const CLOCK_TOLERANCE_NANOSECONDS: i128 = 1_000_000_000;
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeRequest {
    pub generation: u64,
    pub image: ImageReference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeOutcome {
    Ignored,
    Failed,
    Rejected,
    Presented,
    Transitioning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SynchronizeOutcome {
    pub selection_changed: bool,
    pub discontinuity: bool,
    pub presentation_changed: bool,
}

#[derive(Debug)]
enum Mode {
    Static,
    Time(Vec<TimePoint>),
}

#[derive(Clone, Copy, Debug)]
struct PendingDecode {
    request: DecodeRequest,
    transition_started_at: Option<Duration>,
    dispatched: bool,
}

#[derive(Debug)]
struct Transition {
    from: RgbaFrame,
    to: RgbaFrame,
    blend: RgbaFrame,
    to_image: ImageReference,
    started_at: Duration,
}

/// Renderer-neutral playback state for an `h24` schedule or static fallback.
///
/// At most three decoded frames are retained: the two dissolve endpoints and
/// one current blended frame. A presentation consumer may additionally retain
/// its renderer-owned copy. Decode working memory is bounded separately by the
/// HEIC decoder.
#[derive(Debug)]
pub struct Playback {
    mode: Mode,
    primary: ImageReference,
    reduced_motion: bool,
    selected: ImageReference,
    generation: u64,
    pending: Option<PendingDecode>,
    presented: Option<RgbaFrame>,
    presented_image: Option<ImageReference>,
    transition: Option<Transition>,
    retry_not_before: Option<Duration>,
    last_clock: ClockSnapshot,
    last_monotonic: Duration,
}

impl Playback {
    pub fn new(
        metadata: &Metadata,
        primary: ImageReference,
        appearance: AppearancePreference,
        clock: ClockSnapshot,
        monotonic: Duration,
        reduced_motion: bool,
    ) -> Self {
        let (mode, selected) = match metadata.select(appearance, false, false) {
            Selection::Time(schedule) => {
                let mut points = schedule.points().to_vec();
                points.sort_by(|left, right| {
                    left.time
                        .value()
                        .partial_cmp(&right.time.value())
                        .unwrap_or(Ordering::Equal)
                });
                let selected = selected_point(&points, clock).image;
                (Mode::Time(points), selected)
            }
            Selection::Static(image) => (Mode::Static, image),
            Selection::Primary => (Mode::Static, primary),
            Selection::Solar(_) => unreachable!("solar selection was not enabled"),
        };
        let request = DecodeRequest {
            generation: next_generation(),
            image: selected,
        };
        Self {
            mode,
            primary,
            reduced_motion,
            selected,
            generation: request.generation,
            pending: Some(PendingDecode {
                request,
                transition_started_at: None,
                dispatched: false,
            }),
            presented: None,
            presented_image: None,
            transition: None,
            retry_not_before: None,
            last_clock: clock,
            last_monotonic: monotonic,
        }
    }

    pub const fn selected(&self) -> ImageReference {
        self.selected
    }

    pub fn frame(&self) -> Option<&RgbaFrame> {
        self.transition
            .as_ref()
            .map(|transition| &transition.blend)
            .or(self.presented.as_ref())
    }

    pub const fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }

    pub fn take_decode_request(&mut self) -> Option<DecodeRequest> {
        let pending = self.pending.as_mut()?;
        if pending.dispatched {
            return None;
        }
        pending.dispatched = true;
        Some(pending.request)
    }

    /// Re-selects directly from current civil fields. Elapsed boundaries are
    /// never replayed.
    pub fn synchronize(&mut self, clock: ClockSnapshot, monotonic: Duration) -> SynchronizeOutcome {
        self.synchronize_inner(clock, monotonic, false)
    }

    /// Forces discontinuity behavior for an explicit resume or external
    /// time-zone/clock-change notification.
    pub fn resynchronize_after_discontinuity(
        &mut self,
        clock: ClockSnapshot,
        monotonic: Duration,
    ) -> SynchronizeOutcome {
        self.synchronize_inner(clock, monotonic, true)
    }

    fn synchronize_inner(
        &mut self,
        clock: ClockSnapshot,
        monotonic: Duration,
        force_discontinuity: bool,
    ) -> SynchronizeOutcome {
        let discontinuity = force_discontinuity
            || clock_discontinuity(self.last_clock, self.last_monotonic, clock, monotonic);
        let selection = match &self.mode {
            Mode::Static => self.selected,
            Mode::Time(points) => selected_point(points, clock).image,
        };
        let selection_changed = selection != self.selected;
        let mut presentation_changed = false;
        if selection_changed {
            if let Some(transition) = self.transition.take() {
                self.presented = Some(transition.blend);
                self.presented_image = None;
            }
            self.generation = next_generation();
            self.selected = selection;
            self.retry_not_before = None;
            let transition_started_at = if discontinuity || self.reduced_motion {
                None
            } else {
                match &self.mode {
                    Mode::Time(points) => boundary_start(points, clock, monotonic),
                    Mode::Static => None,
                }
            };
            self.pending = Some(PendingDecode {
                request: DecodeRequest {
                    generation: self.generation,
                    image: selection,
                },
                transition_started_at,
                dispatched: false,
            });
        } else if discontinuity {
            if let Some(transition) = self.transition.take() {
                presentation_changed = transition.blend.pixels != transition.to.pixels;
                self.presented = Some(transition.to);
                self.presented_image = Some(transition.to_image);
            }
            let selected_decode_in_flight = self
                .pending
                .is_some_and(|pending| pending.request.image == selection);
            if selected_decode_in_flight {
                self.generation = next_generation();
                self.pending = Some(PendingDecode {
                    request: DecodeRequest {
                        generation: self.generation,
                        image: selection,
                    },
                    transition_started_at: None,
                    dispatched: false,
                });
            } else if self.pending.is_none()
                && self.presented_image != Some(selection)
                && self.retry_due(monotonic)
            {
                self.generation = next_generation();
                self.pending = Some(PendingDecode {
                    request: DecodeRequest {
                        generation: self.generation,
                        image: selection,
                    },
                    transition_started_at: None,
                    dispatched: false,
                });
                self.retry_not_before = None;
            }
        } else if self.pending.is_none()
            && self.transition.is_none()
            && self.presented_image != Some(selection)
            && self.retry_due(monotonic)
        {
            self.generation = next_generation();
            self.pending = Some(PendingDecode {
                request: DecodeRequest {
                    generation: self.generation,
                    image: selection,
                },
                transition_started_at: match &self.mode {
                    Mode::Time(points) if !self.reduced_motion => {
                        boundary_start(points, clock, monotonic)
                    }
                    Mode::Static | Mode::Time(_) => None,
                },
                dispatched: false,
            });
            self.retry_not_before = None;
        }
        self.last_clock = clock;
        self.last_monotonic = monotonic;
        SynchronizeOutcome {
            selection_changed,
            discontinuity,
            presentation_changed,
        }
    }

    /// Returns the next scheduler wake delay. Dissolve frames are intentionally
    /// absent here: a presentation consumer advances them from compositor frame
    /// callbacks.
    pub fn next_wake(&self, clock: ClockSnapshot, monotonic: Duration) -> Option<Duration> {
        let now = time_of_day(clock);
        let boundary = match &self.mode {
            Mode::Time(points) => points
                .iter()
                .map(|point| effective_boundary_second(point.time.value()))
                .map(|boundary| {
                    let boundary = Duration::from_secs(u64::from(boundary));
                    if boundary > now {
                        boundary - now
                    } else {
                        Duration::from_secs(u64::from(DAY_SECONDS)) - now + boundary
                    }
                })
                .min(),
            Mode::Static => None,
        };
        let retry = (self.pending.is_none()
            && self.transition.is_none()
            && self.presented_image != Some(self.selected))
        .then(|| {
            self.retry_not_before
                .map(|deadline| deadline.saturating_sub(monotonic))
        })
        .flatten();
        if boundary.is_none() && retry.is_none() {
            None
        } else {
            Some(
                boundary
                    .into_iter()
                    .chain(retry)
                    .fold(RESYNCHRONIZE_AFTER, Duration::min),
            )
        }
    }

    pub fn complete_decode(
        &mut self,
        request: DecodeRequest,
        result: Result<RgbaFrame, ()>,
        monotonic: Duration,
    ) -> DecodeOutcome {
        self.complete_decode_with_limit(request, result, monotonic, MAX_FRAME_BYTES)
    }

    fn complete_decode_with_limit(
        &mut self,
        request: DecodeRequest,
        result: Result<RgbaFrame, ()>,
        monotonic: Duration,
        max_blend_bytes: usize,
    ) -> DecodeOutcome {
        let Some(pending) = self.pending else {
            return DecodeOutcome::Ignored;
        };
        if pending.request != request || !pending.dispatched {
            return DecodeOutcome::Ignored;
        }
        self.pending = None;
        let Ok(frame) = result else {
            self.retry_not_before = monotonic.checked_add(RESYNCHRONIZE_AFTER);
            self.queue_primary_fallback(request.image);
            return DecodeOutcome::Failed;
        };
        if !valid_frame(&frame) {
            self.retry_not_before = monotonic.checked_add(RESYNCHRONIZE_AFTER);
            self.queue_primary_fallback(request.image);
            return DecodeOutcome::Rejected;
        }
        if request.image == self.selected {
            self.retry_not_before = None;
        }

        let Some(previous) = self.presented.take() else {
            self.transition = None;
            self.presented = Some(frame);
            self.presented_image = Some(request.image);
            return DecodeOutcome::Presented;
        };
        let Some(started_at) = pending.transition_started_at else {
            self.transition = None;
            self.presented = Some(frame);
            self.presented_image = Some(request.image);
            return DecodeOutcome::Presented;
        };
        if monotonic.saturating_sub(started_at) >= DISSOLVE_DURATION
            || !same_dimensions(&previous, &frame)
        {
            self.transition = None;
            self.presented = Some(frame);
            self.presented_image = Some(request.image);
            return DecodeOutcome::Presented;
        }

        let Some(blend) = blend(&previous, &frame, 0, max_blend_bytes) else {
            self.transition = None;
            self.presented = Some(frame);
            self.presented_image = Some(request.image);
            return DecodeOutcome::Presented;
        };
        self.presented_image = None;
        self.transition = Some(Transition {
            from: previous,
            to: frame,
            blend,
            to_image: request.image,
            started_at,
        });
        self.advance_transition(monotonic);
        if self.transition.is_some() {
            DecodeOutcome::Transitioning
        } else {
            DecodeOutcome::Presented
        }
    }

    /// Samples an active dissolve. Returns true only when the presented pixels
    /// changed, allowing static schedules to avoid a redraw loop.
    pub fn advance_transition(&mut self, monotonic: Duration) -> bool {
        let Some(transition) = self.transition.as_mut() else {
            return false;
        };
        let elapsed = monotonic.saturating_sub(transition.started_at);
        if elapsed >= DISSOLVE_DURATION {
            let transition = self.transition.take().unwrap();
            let changed = transition.blend.pixels != transition.to.pixels;
            self.presented = Some(transition.to);
            self.presented_image = Some(transition.to_image);
            return changed;
        }
        let weight = ((elapsed.as_nanos().saturating_mul(256) / DISSOLVE_DURATION.as_nanos())
            .min(256)) as u16;
        blend_into(
            &transition.from,
            &transition.to,
            &mut transition.blend,
            weight,
        )
    }

    fn queue_primary_fallback(&mut self, failed_image: ImageReference) {
        if self.presented.is_some() || failed_image == self.primary {
            return;
        }
        self.generation = next_generation();
        self.pending = Some(PendingDecode {
            request: DecodeRequest {
                generation: self.generation,
                image: self.primary,
            },
            transition_started_at: None,
            dispatched: false,
        });
    }

    fn retry_due(&self, monotonic: Duration) -> bool {
        self.retry_not_before
            .is_none_or(|retry_not_before| monotonic >= retry_not_before)
    }
}

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, AtomicOrdering::Relaxed)
}

fn selected_point(points: &[TimePoint], clock: ClockSnapshot) -> TimePoint {
    let now = seconds_of_day(clock);
    points
        .iter()
        .rev()
        .find(|point| effective_boundary_second(point.time.value()) <= now)
        .copied()
        .unwrap_or_else(|| *points.last().expect("validated schedule is nonempty"))
}

fn boundary_start(
    points: &[TimePoint],
    clock: ClockSnapshot,
    monotonic: Duration,
) -> Option<Duration> {
    let selected = selected_point(points, clock);
    let boundary = effective_boundary_second(selected.time.value());
    let now = time_of_day(clock);
    let boundary = Duration::from_secs(u64::from(boundary));
    let elapsed = if boundary <= now {
        now - boundary
    } else if boundary == Duration::from_secs(u64::from(DAY_SECONDS)) {
        now
    } else {
        Duration::from_secs(u64::from(DAY_SECONDS)) - boundary + now
    };
    monotonic.checked_sub(elapsed)
}

fn effective_boundary_second(time: f64) -> u32 {
    let mut low = 0;
    let mut high = DAY_SECONDS;
    while low < high {
        let middle = low + (high - low) / 2;
        if time <= f64::from(middle) / f64::from(DAY_SECONDS) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

fn seconds_of_day(clock: ClockSnapshot) -> u32 {
    let time = clock.time();
    u32::from(time.hour()) * 3_600 + u32::from(time.minute()) * 60 + u32::from(time.second())
}

fn time_of_day(clock: ClockSnapshot) -> Duration {
    Duration::new(u64::from(seconds_of_day(clock)), clock.nanosecond())
}

fn clock_discontinuity(
    previous_clock: ClockSnapshot,
    previous_monotonic: Duration,
    clock: ClockSnapshot,
    monotonic: Duration,
) -> bool {
    if clock.utc_offset_seconds() != previous_clock.utc_offset_seconds()
        || monotonic < previous_monotonic
    {
        return true;
    }
    let wall_delta = utc_nanoseconds(clock) - utc_nanoseconds(previous_clock);
    let monotonic_delta = i128::try_from(monotonic.saturating_sub(previous_monotonic).as_nanos())
        .unwrap_or(i128::MAX);
    (wall_delta - monotonic_delta).abs() > CLOCK_TOLERANCE_NANOSECONDS
}

fn utc_nanoseconds(clock: ClockSnapshot) -> i128 {
    let date = clock.date();
    let month = i128::from(date.month());
    let adjustment = (14 - month).div_euclid(12);
    let year = i128::from(date.year()) + 4_800 - adjustment;
    let month = month + 12 * adjustment - 3;
    let day_number =
        i128::from(date.day()) + (153 * month + 2).div_euclid(5) + 365 * year + year.div_euclid(4)
            - year.div_euclid(100)
            + year.div_euclid(400)
            - 32_045;
    (day_number * i128::from(DAY_SECONDS) + i128::from(seconds_of_day(clock))
        - i128::from(clock.utc_offset_seconds()))
        * 1_000_000_000
        + i128::from(clock.nanosecond())
}

fn valid_frame(frame: &RgbaFrame) -> bool {
    valid_frame_with_limit(frame, MAX_FRAME_BYTES)
}

fn valid_frame_with_limit(frame: &RgbaFrame, max_frame_bytes: usize) -> bool {
    let Some(bytes) = usize::try_from(frame.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .and_then(|row| {
            usize::try_from(frame.height)
                .ok()
                .and_then(|height| row.checked_mul(height))
        })
    else {
        return false;
    };
    frame.width > 0
        && frame.height > 0
        && bytes <= max_frame_bytes
        && frame.pixels.len() == bytes
        && frame.pixels.capacity() <= max_frame_bytes
        && frame.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255)
}

fn same_dimensions(left: &RgbaFrame, right: &RgbaFrame) -> bool {
    left.width == right.width && left.height == right.height
}

fn blend(
    from: &RgbaFrame,
    to: &RgbaFrame,
    to_weight: u16,
    max_output_bytes: usize,
) -> Option<RgbaFrame> {
    if to.pixels.len() > max_output_bytes {
        return None;
    }
    let from_weight = 256 - to_weight;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(to.pixels.len()).ok()?;
    pixels.extend(from.pixels.iter().zip(&to.pixels).map(|(from, to)| {
        ((u16::from(*from) * from_weight + u16::from(*to) * to_weight + 128) / 256) as u8
    }));
    Some(RgbaFrame {
        width: to.width,
        height: to.height,
        pixels,
    })
}

fn blend_into(from: &RgbaFrame, to: &RgbaFrame, blend: &mut RgbaFrame, to_weight: u16) -> bool {
    let from_weight = 256 - to_weight;
    let mut changed = false;
    for ((output, from), to) in blend.pixels.iter_mut().zip(&from.pixels).zip(&to.pixels) {
        let value =
            ((u16::from(*from) * from_weight + u16::from(*to) * to_weight + 128) / 256) as u8;
        changed |= *output != value;
        *output = value;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_wallpaper::heic::Document;
    use crate::dynamic_wallpaper::{
        Appearance, CivilDate, CivilTime, NormalizedTime, PropertyValue, Schedule,
    };

    fn image(position: usize) -> ImageReference {
        ImageReference::from_position(position)
    }

    fn point(position: usize, time: f64) -> TimePoint {
        TimePoint {
            image: image(position),
            time: NormalizedTime::new(time).unwrap(),
        }
    }

    fn metadata(points: Vec<TimePoint>) -> Metadata {
        let mut metadata = Metadata::default();
        metadata
            .insert(
                crate::dynamic_wallpaper::AppleProperty::Time,
                PropertyValue::Time(Schedule::new(points, None).unwrap()),
            )
            .unwrap();
        metadata
    }

    fn clock(hour: u8, minute: u8, second: u8, offset: i32) -> ClockSnapshot {
        clock_on(1, hour, minute, second, offset)
    }

    fn clock_on(day: u8, hour: u8, minute: u8, second: u8, offset: i32) -> ClockSnapshot {
        ClockSnapshot::new(
            CivilDate::new(2026, 11, day).unwrap(),
            CivilTime::new(hour, minute, second).unwrap(),
            offset,
        )
        .unwrap()
    }

    fn precise_clock(hour: u8, minute: u8, second: u8, nanosecond: u32) -> ClockSnapshot {
        ClockSnapshot::new_with_nanosecond(
            CivilDate::new(2026, 11, 1).unwrap(),
            CivilTime::new(hour, minute, second).unwrap(),
            nanosecond,
            0,
        )
        .unwrap()
    }

    fn frame(value: u8, width: u32, height: u32) -> RgbaFrame {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
        RgbaFrame {
            width,
            height,
            pixels,
        }
    }

    fn complete_initial(playback: &mut Playback, value: u8) {
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Ok(frame(value, 2, 1)), Duration::ZERO),
            DecodeOutcome::Presented
        );
    }

    #[test]
    fn selection_sorts_wraps_and_uses_the_last_duplicate_at_a_boundary() {
        let metadata = metadata(vec![point(1, 0.5), point(2, 0.25), point(3, 0.25)]);
        let before = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(1, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(before.selected(), image(1));

        let boundary = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(6, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(boundary.selected(), image(3));

        let after = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(12, 0, 1, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(after.selected(), image(1));
    }

    #[test]
    fn scheduler_wakes_at_boundaries_or_sixty_second_resynchronization() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let scheduled = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 30, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(
            scheduled.next_wake(clock(0, 0, 30, 0), Duration::ZERO),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            scheduled.next_wake(clock(0, 1, 0, 0), Duration::from_secs(30)),
            Some(Duration::from_secs(60))
        );

        let mut static_metadata = Metadata::default();
        static_metadata
            .insert(
                crate::dynamic_wallpaper::AppleProperty::Appearance,
                PropertyValue::Appearance(Appearance {
                    light: image(4),
                    dark: image(5),
                }),
            )
            .unwrap();
        let mut static_playback = Playback::new(
            &static_metadata,
            image(0),
            AppearancePreference::Dark,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(static_playback.selected(), image(5));
        assert_eq!(
            static_playback.next_wake(clock(0, 0, 0, 0), Duration::ZERO),
            None
        );
        let unchanged =
            static_playback.synchronize(clock(12, 0, 0, 0), Duration::from_secs(43_200));
        assert!(!unchanged.selection_changed);
        assert!(!unchanged.presentation_changed);
        let appearance_request = static_playback.take_decode_request().unwrap();
        assert_eq!(appearance_request.image, image(5));
        assert_eq!(
            static_playback.complete_decode(
                appearance_request,
                Err(()),
                Duration::from_secs(43_200),
            ),
            DecodeOutcome::Failed
        );
        assert_eq!(
            static_playback.take_decode_request().unwrap().image,
            image(0)
        );
    }

    #[test]
    fn clock_offset_jump_reselects_without_replaying_or_dissolving() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 0.05), point(2, 0.1)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(1, 50, 0, -4 * 3_600),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 10);

        let outcome =
            playback.synchronize(clock(1, 10, 0, -5 * 3_600), Duration::from_secs(20 * 60));
        assert_eq!(
            outcome,
            SynchronizeOutcome {
                selection_changed: true,
                discontinuity: true,
                presentation_changed: false,
            }
        );
        assert_eq!(playback.selected(), image(0));
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Ok(frame(20, 2, 1)), Duration::from_secs(1_200)),
            DecodeOutcome::Presented
        );
        assert!(!playback.is_transitioning());
    }

    #[test]
    fn spring_clock_suspend_and_midnight_reselect_current_civil_time() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 0.1), point(2, 0.5)]);
        for (before, after, monotonic, expected, discontinuity) in [
            (
                clock(1, 59, 59, -5 * 3_600),
                clock(3, 0, 0, -4 * 3_600),
                Duration::from_secs(1),
                image(1),
                true,
            ),
            (
                clock(1, 0, 0, 0),
                clock(13, 0, 0, 0),
                Duration::from_secs(1),
                image(2),
                true,
            ),
            (
                clock(1, 0, 0, 0),
                clock(13, 0, 0, 0),
                Duration::ZERO,
                image(2),
                true,
            ),
            (
                clock_on(1, 23, 59, 59, 0),
                clock_on(2, 0, 0, 0, 0),
                Duration::from_secs(1),
                image(0),
                false,
            ),
        ] {
            let mut playback = Playback::new(
                &metadata,
                image(9),
                AppearancePreference::Automatic,
                before,
                Duration::ZERO,
                false,
            );
            let outcome = playback.synchronize(after, monotonic);
            assert_eq!(playback.selected(), expected);
            assert_eq!(outcome.discontinuity, discontinuity);
        }
    }

    #[test]
    fn regular_boundary_dissolves_for_two_seconds_from_the_boundary() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 0);
        let outcome = playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        assert!(outcome.selection_changed);
        assert!(!outcome.discontinuity);
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Ok(frame(200, 2, 1)), Duration::from_millis(60_500),),
            DecodeOutcome::Transitioning
        );
        assert_eq!(playback.frame().unwrap().pixels[0], 50);
        assert!(playback.advance_transition(Duration::from_secs(61)));
        assert_eq!(playback.frame().unwrap().pixels[0], 100);
        assert!(playback.advance_transition(Duration::from_secs(62)));
        assert_eq!(playback.frame().unwrap().pixels[0], 200);
        assert!(!playback.is_transitioning());
    }

    #[test]
    fn explicit_resume_snaps_active_or_pending_transition_to_current_selection() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut active = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut active, 0);
        active.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let request = active.take_decode_request().unwrap();
        active.complete_decode(request, Ok(frame(200, 2, 1)), Duration::from_millis(60_500));
        let outcome =
            active.resynchronize_after_discontinuity(clock(0, 1, 1, 0), Duration::from_secs(61));
        assert!(outcome.discontinuity);
        assert!(outcome.presentation_changed);
        assert!(!active.is_transitioning());
        assert_eq!(active.frame().unwrap().pixels[0], 200);

        let mut pending = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut pending, 0);
        pending.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let stale = pending.take_decode_request().unwrap();
        pending.resynchronize_after_discontinuity(clock(0, 1, 1, 0), Duration::from_secs(61));
        assert_eq!(
            pending.complete_decode(stale, Ok(frame(99, 2, 1)), Duration::from_secs(61)),
            DecodeOutcome::Ignored
        );
        let current = pending.take_decode_request().unwrap();
        assert_ne!(current.generation, stale.generation);
        assert_eq!(
            pending.complete_decode(current, Ok(frame(200, 2, 1)), Duration::from_secs(61)),
            DecodeOutcome::Presented
        );
    }

    #[test]
    fn late_unequal_and_reduced_motion_decodes_switch_immediately() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        for (reduced_motion, completion, dimensions) in [
            (false, Duration::from_secs(63), (2, 1)),
            (false, Duration::from_secs(61), (1, 1)),
            (true, Duration::from_secs(61), (2, 1)),
        ] {
            let mut playback = Playback::new(
                &metadata,
                image(0),
                AppearancePreference::Automatic,
                clock(0, 0, 0, 0),
                Duration::ZERO,
                reduced_motion,
            );
            complete_initial(&mut playback, 0);
            playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
            let request = playback.take_decode_request().unwrap();
            assert_eq!(
                playback.complete_decode(
                    request,
                    Ok(frame(200, dimensions.0, dimensions.1)),
                    completion,
                ),
                DecodeOutcome::Presented
            );
            assert!(!playback.is_transitioning());
        }
    }

    #[test]
    fn stale_and_failed_decode_work_cannot_replace_the_retained_frame() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let stale = playback.take_decode_request().unwrap();
        playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        assert_eq!(
            playback.complete_decode(stale, Ok(frame(99, 2, 1)), Duration::from_secs(60)),
            DecodeOutcome::Ignored
        );
        let current = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(current, Err(()), Duration::from_secs(60)),
            DecodeOutcome::Failed
        );
        assert!(playback.frame().is_none());
        let fallback = playback.take_decode_request().unwrap();
        assert_eq!(fallback.image, image(0));
        assert_eq!(
            playback.complete_decode(fallback, Ok(frame(10, 2, 1)), Duration::from_secs(60)),
            DecodeOutcome::Presented
        );
        assert_eq!(playback.frame().unwrap().pixels[0], 10);
    }

    #[test]
    fn failed_boundary_decode_retries_on_bounded_resynchronization() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 10);
        playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let failed = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(failed, Err(()), Duration::from_secs(60)),
            DecodeOutcome::Failed
        );
        assert_eq!(playback.frame().unwrap().pixels[0], 10);
        assert_eq!(playback.take_decode_request(), None);

        let outcome = playback.synchronize(clock(0, 2, 0, 0), Duration::from_secs(120));
        assert!(!outcome.selection_changed);
        assert!(!outcome.presentation_changed);
        let retry = playback.take_decode_request().unwrap();
        assert_eq!(retry.image, image(1));
    }

    #[test]
    fn decode_requests_are_dispatched_once_and_unique_across_playback_instances() {
        let metadata = metadata(vec![point(0, 0.0)]);
        let mut first = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let first_request = first.take_decode_request().unwrap();
        assert_eq!(first.take_decode_request(), None);

        let mut replacement = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let replacement_request = replacement.take_decode_request().unwrap();
        assert_ne!(first_request.generation, replacement_request.generation);
        assert_eq!(
            replacement.complete_decode(first_request, Ok(frame(99, 2, 1)), Duration::ZERO),
            DecodeOutcome::Ignored
        );
    }

    #[test]
    fn fixture_schedule_drives_lazy_document_decode_and_transition() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dynamic-heic/synthetic-all-properties.heic");
        let document = Document::open(&path).unwrap();
        let mut playback = Playback::new(
            document.metadata(),
            document.primary_image(),
            AppearancePreference::Automatic,
            clock(6, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let initial = playback.take_decode_request().unwrap();
        assert_eq!(initial.image, image(1));
        assert_eq!(
            playback.complete_decode(
                initial,
                document.decode(initial.image).map_err(|_| ()),
                Duration::ZERO,
            ),
            DecodeOutcome::Presented
        );
        assert!(playback.frame().unwrap().pixels[1] >= 253);

        let boundary = playback.synchronize(clock(12, 0, 0, 0), Duration::from_secs(6 * 3_600));
        assert!(boundary.selection_changed);
        let next = playback.take_decode_request().unwrap();
        assert_eq!(next.image, image(2));
        assert_eq!(
            playback.complete_decode(
                next,
                document.decode(next.image).map_err(|_| ()),
                Duration::from_secs(6 * 3_600) + Duration::from_millis(500),
            ),
            DecodeOutcome::Transitioning
        );
        let blended = &playback.frame().unwrap().pixels[..4];
        assert!(blended[1] > 180);
        assert!(blended[2] > 60);
        assert_eq!(blended[3], 255);
    }

    #[test]
    fn malformed_completion_is_rejected_before_becoming_a_frame_role() {
        let metadata = metadata(vec![point(0, 0.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(
                request,
                Ok(RgbaFrame {
                    width: 2,
                    height: 2,
                    pixels: vec![0; 15],
                }),
                Duration::ZERO,
            ),
            DecodeOutcome::Rejected
        );
        assert!(playback.frame().is_none());

        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(
                request,
                Ok(RgbaFrame {
                    width: 1,
                    height: 1,
                    pixels: vec![1, 2, 3, 0],
                }),
                Duration::ZERO,
            ),
            DecodeOutcome::Rejected
        );
    }

    #[test]
    fn floating_boundaries_and_midnight_use_the_selection_predicate() {
        let thirteen_seconds = 13.0 / 86_400.0;
        assert_eq!(effective_boundary_second(thirteen_seconds), 13);
        assert_eq!(
            effective_boundary_second(f64::from_bits((1.0 / 86_400.0_f64).to_bits() + 1)),
            2
        );

        let metadata = metadata(vec![point(0, 0.25), point(1, 0.999_999)]);
        let mut playback = Playback::new(
            &metadata,
            image(9),
            AppearancePreference::Automatic,
            clock_on(1, 23, 59, 59, 0),
            Duration::ZERO,
            false,
        );
        assert_eq!(playback.selected(), image(0));
        assert_eq!(
            playback.next_wake(clock_on(1, 23, 59, 59, 0), Duration::ZERO),
            Some(Duration::from_secs(1))
        );
        playback.synchronize(clock_on(2, 0, 0, 0, 0), Duration::from_secs(1));
        assert_eq!(playback.selected(), image(1));
    }

    #[test]
    fn real_boundary_deadline_rejects_late_and_pre_epoch_dissolves() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut late = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            precise_clock(0, 0, 59, 900_000_000),
            Duration::from_millis(59_900),
            false,
        );
        complete_initial(&mut late, 0);
        late.synchronize(
            precise_clock(0, 1, 0, 900_000_000),
            Duration::from_millis(60_900),
        );
        let request = late.take_decode_request().unwrap();
        assert_eq!(
            late.complete_decode(request, Ok(frame(200, 2, 1)), Duration::from_millis(62_400),),
            DecodeOutcome::Presented
        );

        let mut young_epoch = Playback::new(
            &metadata,
            image(9),
            AppearancePreference::Automatic,
            clock(12, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let selected = young_epoch.take_decode_request().unwrap();
        assert_eq!(
            young_epoch.complete_decode(selected, Err(()), Duration::ZERO),
            DecodeOutcome::Failed
        );
        let fallback = young_epoch.take_decode_request().unwrap();
        young_epoch.complete_decode(fallback, Ok(frame(0, 2, 1)), Duration::ZERO);
        young_epoch.synchronize(clock(12, 1, 0, 0), Duration::from_secs(60));
        let retry = young_epoch.take_decode_request().unwrap();
        assert_eq!(
            young_epoch.complete_decode(retry, Ok(frame(200, 2, 1)), Duration::from_secs(60),),
            DecodeOutcome::Presented
        );
    }

    #[test]
    fn retry_deadline_and_blended_identity_prevent_loops_and_stale_labels() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 0);
        playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();
        playback.complete_decode(request, Ok(frame(200, 2, 1)), Duration::from_millis(60_500));
        playback.synchronize(clock(0, 0, 30, 0), Duration::from_secs(90));
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Err(()), Duration::from_secs(90)),
            DecodeOutcome::Failed
        );
        playback.synchronize(clock(0, 0, 31, 0), Duration::from_secs(91));
        assert_eq!(playback.take_decode_request(), None);
        playback.resynchronize_after_discontinuity(clock(0, 0, 31, 0), Duration::from_secs(91));
        assert_eq!(playback.take_decode_request(), None);
        assert_eq!(
            playback.next_wake(clock(0, 0, 31, 0), Duration::from_secs(91)),
            Some(Duration::from_secs(29))
        );
        playback.synchronize(clock(0, 0, 32, 0), Duration::from_secs(150));
        assert_eq!(playback.take_decode_request().unwrap().image, image(0));
        assert_ne!(
            playback.next_wake(clock(0, 0, 32, 0), Duration::from_secs(150)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn transition_reuses_blend_storage_and_reports_actual_pixel_changes() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 0);
        playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();
        playback.complete_decode(request, Ok(frame(200, 2, 1)), Duration::from_millis(60_500));
        let allocation = playback.frame().unwrap().pixels.as_ptr();
        assert!(!playback.advance_transition(Duration::from_millis(60_500)));
        assert!(playback.advance_transition(Duration::from_secs(61)));
        assert_eq!(playback.frame().unwrap().pixels.as_ptr(), allocation);

        let mut no_allocation = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut no_allocation, 0);
        no_allocation.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let request = no_allocation.take_decode_request().unwrap();
        assert_eq!(
            no_allocation.complete_decode_with_limit(
                request,
                Ok(frame(200, 2, 1)),
                Duration::from_secs(60),
                0,
            ),
            DecodeOutcome::Presented
        );
        assert!(!no_allocation.is_transitioning());
    }

    #[test]
    fn frame_limit_includes_retained_vector_capacity() {
        let mut pixels = Vec::with_capacity(8);
        pixels.extend_from_slice(&[1, 2, 3, 255]);
        assert!(!valid_frame_with_limit(
            &RgbaFrame {
                width: 1,
                height: 1,
                pixels,
            },
            4,
        ));
    }

    #[test]
    fn ordinary_and_static_failures_wake_once_when_retry_becomes_due() {
        let timed_metadata = metadata(vec![point(0, 0.0), point(1, 0.5)]);
        let mut timed = Playback::new(
            &timed_metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(11, 59, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut timed, 0);
        timed.synchronize(clock(12, 0, 0, 0), Duration::from_secs(60));
        let failed = timed.take_decode_request().unwrap();
        timed.complete_decode(failed, Err(()), Duration::from_secs(60));
        timed.synchronize(clock(12, 0, 59, 0), Duration::from_secs(119));
        assert_eq!(timed.take_decode_request(), None);
        timed.synchronize(clock(12, 1, 0, 0), Duration::from_secs(120));
        assert_eq!(timed.take_decode_request().unwrap().image, image(1));
        assert_ne!(
            timed.next_wake(clock(12, 1, 0, 0), Duration::from_secs(120)),
            Some(Duration::ZERO)
        );

        let mut static_metadata = Metadata::default();
        static_metadata
            .insert(
                crate::dynamic_wallpaper::AppleProperty::Appearance,
                PropertyValue::Appearance(Appearance {
                    light: image(1),
                    dark: image(2),
                }),
            )
            .unwrap();
        let mut static_playback = Playback::new(
            &static_metadata,
            image(0),
            AppearancePreference::Dark,
            clock(1, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        let appearance = static_playback.take_decode_request().unwrap();
        static_playback.complete_decode(appearance, Err(()), Duration::ZERO);
        let primary = static_playback.take_decode_request().unwrap();
        static_playback.complete_decode(primary, Ok(frame(0, 2, 1)), Duration::ZERO);
        assert_eq!(
            static_playback.next_wake(clock(1, 0, 0, 0), Duration::ZERO),
            Some(Duration::from_secs(60))
        );
        static_playback.synchronize(clock(1, 1, 0, 0), Duration::from_secs(60));
        assert_eq!(
            static_playback.take_decode_request().unwrap().image,
            image(2)
        );
        assert_eq!(
            static_playback.next_wake(clock(1, 1, 0, 0), Duration::from_secs(60)),
            None
        );
    }

    #[test]
    fn completing_identical_transition_updates_identity_without_dirty_pixels() {
        let metadata = metadata(vec![point(0, 0.0), point(1, 60.0 / 86_400.0)]);
        let mut playback = Playback::new(
            &metadata,
            image(0),
            AppearancePreference::Automatic,
            clock(0, 0, 0, 0),
            Duration::ZERO,
            false,
        );
        complete_initial(&mut playback, 1);
        playback.synchronize(clock(0, 1, 0, 0), Duration::from_secs(60));
        let request = playback.take_decode_request().unwrap();
        assert_eq!(
            playback.complete_decode(request, Ok(frame(1, 2, 1)), Duration::from_secs(60),),
            DecodeOutcome::Transitioning
        );
        assert!(!playback.advance_transition(Duration::from_secs(62)));
        assert!(!playback.is_transitioning());
    }
}
