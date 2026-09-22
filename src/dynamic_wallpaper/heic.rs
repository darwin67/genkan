//! Bounded parser and decoder for macOS dynamic HEIC wallpapers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::FromRawFd;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use libheif_rs::{
    ColorProfileNCLX, ColorSpace, DecodingOptions, HeifContext, LibHeif, RgbChroma, StreamReader,
};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformExecutor, TransformOptions};
use plist::{Dictionary, Value};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};
use thiserror::Error;

use super::{
    container, Appearance, AppleProperty, HeifItemId, ImageReference, Metadata, ModelError,
    NormalizedTime, PropertyValue, Schedule, SolarPoint, SolarPosition, TimePoint, TopLevelImages,
};

const APPLE_DESKTOP_NAMESPACE: &str = "http://ns.apple.com/namespace/1.0/";

/// Resource ceilings applied before metadata or pixel allocations.
#[derive(Clone, Copy, Debug)]
struct Limits {
    max_source_bytes: u64,
    /// Container-item records: `ipma` entries, `iloc` records, `iref`
    /// destinations, and track references, summed over every box of each kind.
    /// libheif applies this one value to each of those counts, but it applies
    /// it per box, so the container preflight enforces the sum as well. Tile
    /// based images contribute an entry per tile, so a real multi-image
    /// wallpaper needs far more than its top-level image count.
    max_container_items: u32,
    /// Property associations summed over every `ipma` box. libheif merges the
    /// boxes into the first without an aggregate check, so this is the ceiling
    /// on the merged association storage.
    max_container_associations: u32,
    /// Boxes the container preflight may visit. Each one is a native box object
    /// libheif would allocate, so this bounds the walk as well as the parser.
    max_container_boxes: u32,
    /// Nesting depth of container boxes.
    max_container_depth: u32,
    /// Extents one `iloc` record may declare, matching libheif's own
    /// `max_iloc_extents_per_item`.
    max_item_extents: u32,
    /// Non-image items. libheif copies each one's extent into its own
    /// allocation while parsing, and its per-item ceiling bounds that copy but
    /// not how many copies exist.
    max_metadata_items: usize,
    /// Top-level still images the schedule may address.
    max_top_level_images: usize,
    /// Metadata blocks attached to the primary image.
    max_metadata_blocks: usize,
    max_tiles: u32,
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
    max_metadata_block_bytes: usize,
    max_metadata_bytes: usize,
    /// Bytes of one embedded ICC profile. libheif applies the same value while
    /// reading the profile, and the decoder applies it again before parsing one
    /// so a profile that reaches Rust unconverted is still bounded.
    max_color_profile_bytes: usize,
    /// Tags in one embedded ICC profile. Every tag is parsed separately, and
    /// the profile parser validates each allocation against that tag's own
    /// extent rather than against the profile, so the tag count is what bounds
    /// the aggregate. Real display profiles carry well under twenty.
    max_color_profile_tags: usize,
    /// Decoded text one embedded ICC profile may materialize, summed over every
    /// tag. The profile parser allocates a separate string per localization
    /// record and bounds each record against the tag but not their sum, so a
    /// profile at the byte ceiling can otherwise expand to tens of gigabytes.
    /// Real descriptive text is a few dozen bytes.
    max_color_profile_text_bytes: usize,
    /// Localization records one embedded ICC profile may materialize, summed
    /// over every tag. The parser keeps three strings per record regardless of
    /// the record's declared length, so a flood of zero-length records costs
    /// record storage without costing any text bytes. A profile that localizes
    /// its description and copyright into dozens of languages reaches this
    /// legitimately, so the ceiling sits far above any real profile while the
    /// text ceiling still bounds what the records can carry.
    max_color_profile_text_records: usize,
    max_xml_depth: usize,
    max_xml_events: usize,
    max_plist_bytes: usize,
    max_plist_objects: usize,
    max_expanded_plist_objects: usize,
    max_expanded_plist_bytes: usize,
    max_plist_depth: usize,
    max_schedule_points: usize,
    max_output_bytes: usize,
    max_libheif_memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024 * 1024,
            // Tile-based images put one `ipma` entry and one `iloc` record on
            // every tile, so a 4K multi-image wallpaper needs hundreds of these
            // records even though it exposes a handful of top-level images.
            max_container_items: 4_096,
            // Every `ipma` entry can carry up to 255 associations, and libheif
            // copies them when it merges boxes, so the sum is what bounds the
            // merged storage rather than the per-box entry count.
            max_container_associations: 32_768,
            // Real files carry tens of boxes. This sits above the item ceiling
            // so a flood of item records is reported as an item-count failure,
            // while a flood of other boxes is still bounded.
            max_container_boxes: 16_384,
            max_container_depth: 16,
            max_item_extents: 32,
            // Real wallpapers carry one or two metadata items; the ceiling
            // exists so a container cannot multiply the per-item copy.
            max_metadata_items: 64,
            // Apple dynamic wallpapers carry one still per schedule point, and a
            // schedule may address the same image from several points.
            max_top_level_images: 64,
            max_metadata_blocks: 64,
            max_tiles: 4_096,
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 32 * 1024 * 1024,
            max_metadata_block_bytes: 1024 * 1024,
            max_metadata_bytes: 4 * 1024 * 1024,
            // Real display profiles are a few kilobytes; the ceiling bounds what
            // the profile parser may walk before the transform is built.
            max_color_profile_bytes: 1024 * 1024,
            // A real 6K wallpaper's profile carries 10 to 17 tags. The ceiling
            // is generous against that and still bounds the aggregate of the
            // parser's per-tag allocations.
            max_color_profile_tags: 64,
            // Real descriptive text is a few dozen bytes. The ceiling is
            // generous against that and still bounds the parser's retained text
            // well below the frame budgets.
            max_color_profile_text_bytes: 4 * 1024 * 1024,
            // A heavily localized profile carries dozens of records per
            // descriptive tag. The ceiling is far above that, and the text
            // ceiling is what bounds the amplification a flood of records
            // would otherwise reach.
            max_color_profile_text_records: 1_024,
            max_xml_depth: 32,
            max_xml_events: 4_096,
            max_plist_bytes: 512 * 1024,
            max_plist_objects: 1_024,
            max_expanded_plist_objects: 4_096,
            max_expanded_plist_bytes: 16 * 1024 * 1024,
            max_plist_depth: 32,
            max_schedule_points: 256,
            max_output_bytes: 128 * 1024 * 1024,
            max_libheif_memory_bytes: 384 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not open dynamic wallpaper: {0}")]
    Open(&'static str),
    #[error("dynamic wallpaper exceeds the source size limit")]
    SourceTooLarge,
    #[error("invalid or unsupported HEIC container: {0}")]
    Container(String),
    #[error("dynamic wallpaper exceeds a resource limit: {0}")]
    Limit(&'static str),
    #[error("invalid dynamic wallpaper metadata: {0}")]
    Metadata(String),
    #[error("dynamic wallpaper image reference is not present")]
    MissingImage,
    #[error("unsupported dynamic wallpaper color profile: {0}")]
    UnsupportedColor(&'static str),
    #[error("could not decode dynamic wallpaper image: {0}")]
    Decode(String),
}

/// One decoded, tightly packed, opaque RGBA8 image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// An opened HEIC whose decoded source is fixed when it is opened.
///
/// The container is validated and copied before it reaches libheif, so neither
/// replacing the pathname nor rewriting the bound inode afterwards changes what
/// this document decodes.
pub struct Document {
    context: HeifContext<'static>,
    images: TopLevelImages,
    primary: ImageReference,
    metadata: Metadata,
    limits: Limits,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_with_limits(path, Limits::default())
    }

    fn open_with_limits(path: &Path, limits: Limits) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::Open("path is not absolute"));
        }
        let mut file = crate::stable_file::open_regular(path).map_err(|error| {
            use crate::stable_file::OpenError;
            Error::Open(match error {
                OpenError::Unavailable => "file is unavailable",
                OpenError::Metadata => "file metadata is unavailable",
                OpenError::NotRegular => "path is not a regular file",
                OpenError::Reopen => "stable file could not be reopened",
            })
        })?;
        let source_bytes = file
            .metadata()
            .map_err(|_| Error::Open("file metadata is unavailable"))?
            .len();
        if source_bytes > limits.max_source_bytes {
            return Err(Error::SourceTooLarge);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| Error::Open("file could not be rewound"))?;

        // libheif applies its item limits to each box on its own and copies
        // every non-image item into its own allocation while parsing, so the
        // aggregate ceilings are enforced here, before the container reaches
        // it. The read is bounded by the source-size check above.
        let mut source = Vec::new();
        source
            .try_reserve_exact(usize::try_from(source_bytes).unwrap_or(usize::MAX))
            .map_err(|_| Error::Limit("container preflight buffer"))?;
        (&mut file)
            .take(source_bytes)
            .read_to_end(&mut source)
            .map_err(|_| Error::Open("file could not be read"))?;
        container::preflight(
            &source,
            &container::Budget {
                max_container_items: limits.max_container_items,
                max_container_associations: limits.max_container_associations,
                max_container_boxes: limits.max_container_boxes,
                max_container_depth: limits.max_container_depth,
                max_item_extents: limits.max_item_extents,
                max_metadata_items: u32::try_from(limits.max_metadata_items).unwrap_or(u32::MAX),
                max_metadata_bytes: u64::try_from(limits.max_metadata_bytes).unwrap_or(u64::MAX),
            },
        )?;

        // libheif parses a descriptor rather than the validated buffer, so the
        // two could otherwise disagree: an owner with write access can replace
        // the contents of the bound inode between the walk and the parse. Hand
        // the parser an anonymous copy of exactly the bytes that were checked,
        // and give it that copy's length rather than the length the file had
        // when it was first measured, which can be larger if the file shrank.
        let snapshot = anonymous_source(&source)?;
        let snapshot_bytes = u64::try_from(source.len()).unwrap_or(u64::MAX);
        drop(source);

        let _ = libheif();
        let mut context = HeifContext::new().map_err(container_error)?;
        let mut security_limits = libheif_rs::SecurityLimits::default();
        security_limits.set_max_image_size_pixels(limits.max_pixels);
        security_limits.set_max_number_of_tiles(u64::from(limits.max_tiles));
        security_limits
            .set_max_bayer_pattern_pixels(u32::try_from(limits.max_pixels).unwrap_or(u32::MAX));
        security_limits.set_max_items(limits.max_container_items);
        security_limits.set_max_color_profile_size(
            u32::try_from(limits.max_color_profile_bytes).unwrap_or(u32::MAX),
        );
        // libheif eagerly loads metadata while parsing. The pinned Nix build has no
        // compressed-metadata codecs, so this per-item ceiling and
        // max_container_items bound retained native metadata before Rust can
        // inspect aggregate sizes.
        security_limits.set_max_memory_block_size(
            u64::try_from(limits.max_metadata_block_bytes).unwrap_or(u64::MAX),
        );
        security_limits.set_max_total_memory(
            u64::try_from(limits.max_libheif_memory_bytes).unwrap_or(u64::MAX),
        );
        context
            .set_security_limits(&security_limits)
            .map_err(container_error)?;
        context
            .read_reader(Box::new(StreamReader::new(snapshot, snapshot_bytes)))
            .map_err(container_error)?;

        let item_ids = context.image_ids();
        if item_ids.is_empty() {
            return Err(Error::Container("no top-level images".into()));
        }
        if item_ids.len() > limits.max_top_level_images {
            return Err(Error::Limit("top-level item count"));
        }
        let images = TopLevelImages::new(item_ids.iter().copied().map(HeifItemId::new).collect());

        let primary_handle = context.primary_image_handle().map_err(container_error)?;
        validate_dimensions(&primary_handle, &limits)?;
        let primary_id = primary_handle.item_id();
        let primary = item_ids
            .iter()
            .position(|item_id| *item_id == primary_id)
            .map(ImageReference::from_position)
            .ok_or_else(|| Error::Container("primary image is not top-level".into()))?;
        let metadata = read_metadata(&primary_handle, &images, &limits)?;
        security_limits.set_max_memory_block_size(
            u64::try_from(limits.max_libheif_memory_bytes).unwrap_or(u64::MAX),
        );
        context
            .set_security_limits(&security_limits)
            .map_err(container_error)?;

        Ok(Self {
            context,
            images,
            primary,
            metadata,
            limits,
        })
    }

    pub fn images(&self) -> &TopLevelImages {
        &self.images
    }

    pub fn item_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.images.iter().map(HeifItemId::value)
    }

    pub fn primary_image(&self) -> ImageReference {
        self.primary
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn decode(&self, image: ImageReference) -> Result<RgbaFrame, Error> {
        let item_id = self.images.resolve(image).ok_or(Error::MissingImage)?;
        let handle = self
            .context
            .image_handle(item_id.value())
            .map_err(container_error)?;
        validate_dimensions(&handle, &self.limits)?;
        let color = validate_color(&handle, &self.limits)?;

        let mut options = DecodingOptions::new()
            .ok_or_else(|| Error::Decode("could not allocate decoding options".into()))?;
        options.set_strict_decoding(true);
        options.set_convert_hdr_to_8bit(false);
        options.set_output_image_nclx_profile_passthrough(true);
        let image = libheif()
            .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), Some(options))
            .map_err(decode_error)?;
        // libheif hands the item's ICC profile through to the decoded image
        // instead of converting it, so the transform validated on the handle
        // describes exactly these pixels. A profile that appears only on the
        // decoded image would tag pixels that were never validated.
        if color.is_none() && image.color_profile_raw().is_some() {
            return Err(Error::UnsupportedColor("decoded ICC profile"));
        }
        if let Some(profile) = image.color_profile_nclx() {
            validate_nclx(&profile)?;
        }
        let premultiplied_alpha = image.is_premultiplied_alpha();
        let plane = image
            .planes()
            .interleaved
            .ok_or_else(|| Error::Decode("decoder returned no interleaved plane".into()))?;
        if plane.width != handle.width() || plane.height != handle.height() {
            return Err(Error::Decode(
                "decoded dimensions do not match the item".into(),
            ));
        }
        if plane.bits_per_pixel != 8 || plane.storage_bits_per_pixel != 32 {
            return Err(Error::Decode(
                "decoder returned an unexpected RGBA representation".into(),
            ));
        }
        pack_rgba(
            plane.data,
            plane.stride,
            plane.width,
            plane.height,
            &self.limits,
            premultiplied_alpha,
            color.as_ref().map(|color| color.transform.as_ref()),
        )
    }
}

fn libheif() -> &'static LibHeif {
    static LIBHEIF: OnceLock<LibHeif> = OnceLock::new();
    LIBHEIF.get_or_init(LibHeif::new)
}

/// Copies `bytes` into an anonymous file that is the only reference to them.
///
/// The container preflight approves a buffer, but libheif parses a descriptor.
/// Reading the original path again would let an owner with write access replace
/// the contents between the two, so the parser is given a private copy of
/// exactly the bytes that were checked. The descriptor is closed when the
/// context is dropped. It is not sealed, so a process able to reach this
/// process's descriptors could still reopen it; the copy closes the window
/// against rewriting the original file, which is the threat this addresses.
fn anonymous_source(bytes: &[u8]) -> Result<File, Error> {
    // SAFETY: the name is a valid NUL-terminated string, the flags are valid,
    // and the returned descriptor is owned here.
    let fd = unsafe { libc::memfd_create(c"genkan-heic".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(Error::Open("anonymous source could not be created"));
    }
    // SAFETY: `fd` is a fresh descriptor uniquely owned by this call.
    let mut snapshot = unsafe { File::from_raw_fd(fd) };
    snapshot
        .write_all(bytes)
        .map_err(|_| Error::Open("anonymous source could not be written"))?;
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::Open("anonymous source could not be rewound"))?;
    Ok(snapshot)
}

fn container_error(error: libheif_rs::HeifError) -> Error {
    Error::Container(error.to_string())
}

fn decode_error(error: libheif_rs::HeifError) -> Error {
    Error::Decode(error.to_string())
}

fn validate_dimensions(handle: &libheif_rs::ImageHandle, limits: &Limits) -> Result<(), Error> {
    let width = handle.width();
    let height = handle.height();
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(Error::Limit("image dimensions"))?;
    if width == 0
        || height == 0
        || width > limits.max_width
        || height > limits.max_height
        || pixels > limits.max_pixels
    {
        return Err(Error::Limit("image dimensions"));
    }
    Ok(())
}

/// The four-character type signatures whose payload the profile parser reads as
/// text, one allocation per localization or script record.
const ICC_TEXT_TYPES: [&[u8; 4]; 3] = [b"mluc", b"desc", b"text"];

/// Structural preflight for an embedded ICC profile, run before the profile
/// parser sees it.
///
/// The profile parser bounds each tag against its own extent but not the sum
/// across tags or across the records inside one tag, so a profile that fits the
/// byte ceiling can still make it materialize far more memory than the profile
/// occupies. The parser also reads the declared profile extent without
/// requiring it to match the bytes it was handed, so a profile can describe one
/// size and occupy another.
///
/// This walk establishes the extent, the tag count, the bounds of every tag,
/// the decoded text budget, and the localization-record budget, and rejects
/// anything it cannot account for. It reads only offsets and lengths, never
/// allocates, and therefore runs in time proportional to the tag count.
fn preflight_icc_profile(bytes: &[u8], limits: &Limits) -> Result<(), Error> {
    const HEADER_BYTES: usize = 128;
    const TAG_ENTRY_BYTES: usize = 12;

    if bytes.len() < HEADER_BYTES + 4 {
        return Err(Error::UnsupportedColor("malformed embedded ICC profile"));
    }
    // The header's first field is the profile's own length. The parser reads it
    // but never compares it with the bytes it was given, so a profile can
    // declare any extent at all; require agreement.
    let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if usize::try_from(declared).ok() != Some(bytes.len()) {
        return Err(Error::UnsupportedColor("embedded ICC profile extent"));
    }
    let tag_count = u32::from_be_bytes([
        bytes[HEADER_BYTES],
        bytes[HEADER_BYTES + 1],
        bytes[HEADER_BYTES + 2],
        bytes[HEADER_BYTES + 3],
    ]);
    let tag_count = usize::try_from(tag_count).unwrap_or(usize::MAX);
    if tag_count > limits.max_color_profile_tags {
        return Err(Error::UnsupportedColor("embedded ICC profile tag count"));
    }
    let table_end = HEADER_BYTES
        .checked_add(4)
        .and_then(|start| {
            tag_count
                .checked_mul(TAG_ENTRY_BYTES)
                .and_then(|bytes| start.checked_add(bytes))
        })
        .ok_or(Error::UnsupportedColor("malformed embedded ICC profile"))?;
    if table_end > bytes.len() {
        return Err(Error::UnsupportedColor("malformed embedded ICC profile"));
    }

    let mut text_bytes = 0usize;
    let mut text_records = 0usize;
    for index in 0..tag_count {
        let entry = HEADER_BYTES + 4 + index * TAG_ENTRY_BYTES;
        let offset = u32::from_be_bytes([
            bytes[entry + 4],
            bytes[entry + 5],
            bytes[entry + 6],
            bytes[entry + 7],
        ]);
        let size = u32::from_be_bytes([
            bytes[entry + 8],
            bytes[entry + 9],
            bytes[entry + 10],
            bytes[entry + 11],
        ]);
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        // The parser treats a zero size as "to the end of the profile" for the
        // tone-curve reader, so mirror that rather than reading it as empty.
        let end = if size == 0 {
            bytes.len()
        } else {
            offset
                .checked_add(size)
                .ok_or(Error::UnsupportedColor("malformed embedded ICC profile"))?
        };
        if offset > bytes.len() || end > bytes.len() {
            return Err(Error::UnsupportedColor("malformed embedded ICC profile"));
        }
        let tag = &bytes[offset..end];
        if !ICC_TEXT_TYPES.iter().any(|kind| tag.starts_with(*kind)) {
            continue;
        }
        charge_icc_text(tag, &mut text_bytes, &mut text_records, limits)?;
    }
    Ok(())
}

/// Charges the text one tag will make the profile parser allocate.
///
/// The accounting mirrors the parser's control flow and charges *decoded* sizes
/// rather than encoded ones: the parser converts UTF-16 to UTF-8 and converts
/// lossy UTF-8, both of which can produce more bytes than they consume. It
/// stops where the parser stops, counts each record once even when several
/// records reference the same bytes, and counts records separately from bytes
/// because the parser keeps three strings per record whatever length the record
/// declares.
///
/// It is deliberately conservative where the two disagree, so a text-typed tag
/// the parser would ignore is still charged and a truncated record still
/// consumes its record budget. Over-charging only refuses a profile the parser
/// would have read more cheaply; under-charging is what this exists to
/// prevent.
fn charge_icc_text(
    tag: &[u8],
    text: &mut usize,
    records: &mut usize,
    limits: &Limits,
) -> Result<(), Error> {
    match &tag[..4] {
        b"mluc" => {
            if tag.len() < 28 {
                return Ok(());
            }
            let count = u32::from_be_bytes([tag[8], tag[9], tag[10], tag[11]]);
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            // The first record's length and offset sit at fixed positions.
            if !charge_icc_record(
                tag,
                u32::from_be_bytes([tag[24], tag[25], tag[26], tag[27]]),
                u32::from_be_bytes([tag[20], tag[21], tag[22], tag[23]]),
                text,
                records,
                limits,
            )? {
                return Ok(());
            }
            for record in 1..count {
                let Some(header) = 28usize
                    .checked_add(record.saturating_sub(1).saturating_mul(12))
                    .and_then(|start| tag.get(start..start + 12))
                else {
                    return Ok(());
                };
                if !charge_icc_record(
                    tag,
                    u32::from_be_bytes([header[8], header[9], header[10], header[11]]),
                    u32::from_be_bytes([header[4], header[5], header[6], header[7]]),
                    text,
                    records,
                    limits,
                )? {
                    return Ok(());
                }
            }
        }
        b"desc" => {
            // A v2 `textDescriptionType`: an ASCII section at offset 12, then a
            // Unicode language code and count, then the Unicode string. The
            // parser reads both sections, not just the ASCII one.
            if tag.len() < 12 {
                return Ok(());
            }
            let ascii = u32::from_be_bytes([tag[8], tag[9], tag[10], tag[11]]);
            let ascii = usize::try_from(ascii).unwrap_or(usize::MAX);
            let Some(ascii_end) = 12usize.checked_add(ascii) else {
                return Ok(());
            };
            // The parser rejects a description whose ASCII section runs past
            // the tag, so nothing is allocated and the profile fails anyway.
            if ascii_end > tag.len() {
                return Ok(());
            }
            charge_icc_text_bytes(lossy_text_bound(ascii), text, limits)?;
            let Some(header) = tag.get(ascii_end..ascii_end + 8) else {
                return Ok(());
            };
            *records = records.saturating_add(1);
            if *records > limits.max_color_profile_text_records {
                return Err(Error::UnsupportedColor("embedded ICC profile text records"));
            }
            let units = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
            let units = usize::try_from(units).unwrap_or(usize::MAX);
            let Some(encoded) = units.checked_mul(2) else {
                return Ok(());
            };
            if ascii_end
                .checked_add(8)
                .and_then(|start| start.checked_add(encoded))
                .is_none_or(|end| end > tag.len())
            {
                return Ok(());
            }
            charge_icc_text_bytes(utf16_text_bound(encoded), text, limits)?;
        }
        // A `text` tag reads everything after its type and reserved bytes and
        // converts it lossily.
        _ => {
            let bytes = tag.len().saturating_sub(8);
            charge_icc_text_bytes(lossy_text_bound(bytes), text, limits)?;
        }
    }
    Ok(())
}

/// Charges one localization record and reports whether the parser would read it.
///
/// The parser stops reading a tag when a record's range runs past it, so a
/// record that does not fit is not charged and ends the walk.
fn charge_icc_record(
    tag: &[u8],
    offset: u32,
    length: u32,
    text: &mut usize,
    records: &mut usize,
    limits: &Limits,
) -> Result<bool, Error> {
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    if offset.checked_add(length).is_none_or(|end| end > tag.len()) {
        return Ok(false);
    }
    *records = records.saturating_add(1);
    if *records > limits.max_color_profile_text_records {
        return Err(Error::UnsupportedColor("embedded ICC profile text records"));
    }
    charge_icc_text_bytes(utf16_text_bound(length), text, limits)?;
    Ok(true)
}

/// Upper bound on the UTF-8 bytes a UTF-16 payload of `bytes` decodes to.
///
/// Three UTF-8 bytes per two-byte code unit is the worst case for a code unit
/// that is not part of a surrogate pair.
fn utf16_text_bound(bytes: usize) -> usize {
    bytes.saturating_mul(3) / 2
}

/// Upper bound on the UTF-8 bytes a lossy UTF-8 conversion of `bytes` produces.
///
/// Every invalid byte becomes a three-byte replacement character.
fn lossy_text_bound(bytes: usize) -> usize {
    bytes.saturating_mul(3)
}

/// Adds one decoded size to the text total.
fn charge_icc_text_bytes(decoded: usize, total: &mut usize, limits: &Limits) -> Result<(), Error> {
    *total = total
        .checked_add(decoded)
        .ok_or(Error::UnsupportedColor("embedded ICC profile text size"))?;
    if *total > limits.max_color_profile_text_bytes {
        return Err(Error::UnsupportedColor("embedded ICC profile text size"));
    }
    Ok(())
}

/// A validated ICC-to-sRGB transform for one embedded profile.
///
/// An embedded profile is converted rather than ignored: dropping it would
/// display the wallpaper with the wrong colors, which RFD 4 rejects. Profiles
/// that cannot be converted are refused instead of displayed with unspecified
/// color.
struct IccTransform {
    transform: Arc<dyn TransformExecutor<u8> + Send + Sync>,
}

impl IccTransform {
    fn new(profile: &libheif_rs::ColorProfileRaw, limits: &Limits) -> Result<Self, Error> {
        let bytes = profile.data.as_slice();
        if bytes.is_empty() || bytes.len() > limits.max_color_profile_bytes {
            return Err(Error::UnsupportedColor("embedded ICC profile size"));
        }
        preflight_icc_profile(bytes, limits)?;
        let source = ColorProfile::new_from_slice(bytes)
            .map_err(|_| Error::UnsupportedColor("malformed embedded ICC profile"))?;
        if source.color_space != DataColorSpace::Rgb {
            return Err(Error::UnsupportedColor("non-RGB embedded ICC profile"));
        }
        let destination = ColorProfile::new_srgb();
        let transform = source
            .create_transform_8bit(
                Layout::Rgba,
                &destination,
                Layout::Rgba,
                TransformOptions::default(),
            )
            .map_err(|_| Error::UnsupportedColor("unconvertible embedded ICC profile"))?;
        Ok(Self { transform })
    }
}

/// Validates the color metadata and returns the sRGB transform an embedded ICC
/// profile needs, if the item carries one.
///
/// The check runs before the decode so an unsupported profile costs nothing
/// beyond reading its bytes, and the returned transform is applied to the
/// decoded pixels.
fn validate_color(
    handle: &libheif_rs::ImageHandle,
    limits: &Limits,
) -> Result<Option<IccTransform>, Error> {
    if handle.luma_bits_per_pixel() > 8 || handle.chroma_bits_per_pixel() > 8 {
        return Err(Error::UnsupportedColor("HDR bit depth"));
    }
    let color = handle
        .color_profile_raw()
        .map(|profile| IccTransform::new(&profile, limits))
        .transpose()?;
    if let Some(profile) = handle.color_profile_nclx() {
        validate_nclx(&profile)?;
    }
    Ok(color)
}

fn validate_nclx(profile: &ColorProfileNCLX) -> Result<(), Error> {
    validate_nclx_encoding(
        profile.color_primaries(),
        profile.transfer_characteristics(),
    )
}

fn validate_nclx_encoding(
    primaries: libheif_rs::ColorPrimaries,
    transfer: libheif_rs::TransferCharacteristics,
) -> Result<(), Error> {
    use libheif_rs::{ColorPrimaries, TransferCharacteristics};

    if !matches!(
        primaries,
        ColorPrimaries::ITU_R_BT_709_5 | ColorPrimaries::Unspecified
    ) {
        return Err(Error::UnsupportedColor("non-sRGB color primaries"));
    }
    if !matches!(
        transfer,
        TransferCharacteristics::IEC_61966_2_1 | TransferCharacteristics::Unspecified
    ) {
        return Err(Error::UnsupportedColor("non-sRGB transfer function"));
    }
    Ok(())
}

/// Tightly packed RGBA bytes the ICC conversion transform consumes at a time.
///
/// The conversion works on a fixed scratch chunk rather than a second
/// full-frame buffer, so a converted frame costs one frame plus this constant
/// instead of two frames.
const ICC_CONVERSION_CHUNK_BYTES: usize = 1024 * 1024;

/// Packs one decoded plane into tightly packed, opaque RGBA8.
///
/// Alpha is composited over opaque black, which is the "alpha is composited
/// over opaque black" half of RFD 4's rendering contract. An embedded ICC
/// profile is converted to sRGB before that compositing step, while the color
/// channels are still straight rather than alpha-multiplied, because a
/// nonlinear transform does not commute with premultiplication.
fn pack_rgba(
    source: &[u8],
    stride: usize,
    width: u32,
    height: u32,
    limits: &Limits,
    premultiplied_alpha: bool,
    color: Option<&(dyn TransformExecutor<u8> + Send + Sync)>,
) -> Result<RgbaFrame, Error> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(Error::Limit("decoded image memory"))?;
    let output_bytes = row_bytes
        .checked_mul(usize::try_from(height).map_err(|_| Error::Limit("decoded image memory"))?)
        .ok_or(Error::Limit("decoded image memory"))?;
    if output_bytes > limits.max_output_bytes {
        return Err(Error::Limit("decoded image memory"));
    }
    if stride < row_bytes {
        return Err(Error::Decode("decoded row stride is too short".into()));
    }
    let required = stride
        .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
        .ok_or_else(|| Error::Decode("decoded plane size overflow".into()))?;
    if source.len() < required {
        return Err(Error::Decode("decoded plane is truncated".into()));
    }

    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(output_bytes)
        .map_err(|_| Error::Limit("decoded image memory"))?;
    for row in source.chunks(stride).take(height as usize) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    if let Some(transform) = color {
        if premultiplied_alpha {
            unpremultiply(&mut pixels);
        }
        convert_to_srgb(&mut pixels, transform)?;
    }
    // A conversion leaves the color channels straight, so the compositing step
    // has to apply alpha itself even when the decoder premultiplied it.
    composite_over_black(&mut pixels, premultiplied_alpha && color.is_none());
    Ok(RgbaFrame {
        width,
        height,
        pixels,
    })
}

/// Converts tightly packed RGBA pixels to sRGB through `transform`.
///
/// The conversion runs over bounded chunks so the working set is a constant
/// rather than a second frame. The pixel budget was already checked against
/// `max_output_bytes` by the caller, so a frame that reaches here is one the
/// decoder was willing to allocate.
fn convert_to_srgb(
    pixels: &mut [u8],
    transform: &(dyn TransformExecutor<u8> + Send + Sync),
) -> Result<(), Error> {
    let chunk = ICC_CONVERSION_CHUNK_BYTES.min(pixels.len());
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(chunk)
        .map_err(|_| Error::Limit("ICC conversion buffer"))?;
    scratch.resize(chunk, 0);
    for chunk in pixels.chunks_mut(ICC_CONVERSION_CHUNK_BYTES) {
        let scratch = &mut scratch[..chunk.len()];
        transform
            .transform(chunk, scratch)
            .map_err(|_| Error::UnsupportedColor("embedded ICC profile conversion failed"))?;
        chunk.copy_from_slice(scratch);
    }
    Ok(())
}

/// Divides alpha back out of premultiplied color channels.
///
/// A fully transparent pixel carries no color to recover, so it becomes black,
/// which is what compositing it over opaque black would produce anyway.
fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

/// Composites straight or premultiplied alpha over opaque black and clears the
/// alpha channel, so every frame is opaque RGBA8.
fn composite_over_black(pixels: &mut [u8], premultiplied_alpha: bool) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if !premultiplied_alpha {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        pixel[3] = 255;
    }
}

fn read_metadata(
    handle: &libheif_rs::ImageHandle,
    images: &TopLevelImages,
    limits: &Limits,
) -> Result<Metadata, Error> {
    let count = handle.number_of_metadata_blocks(0).max(0) as usize;
    if count > limits.max_metadata_blocks {
        return Err(Error::Limit("metadata block count"));
    }
    let mut ids = vec![0; count];
    let written = handle.metadata_block_ids(&mut ids, 0);
    if written != count {
        return Err(Error::Container(
            "could not enumerate metadata blocks".into(),
        ));
    }
    let mut total = 0usize;
    let mut metadata = Metadata::default();
    for id in ids {
        let size = handle.metadata_size(id);
        if size > limits.max_metadata_block_bytes {
            return Err(Error::Limit("metadata block size"));
        }
        total = total
            .checked_add(size)
            .ok_or(Error::Limit("total metadata size"))?;
        if total > limits.max_metadata_bytes {
            return Err(Error::Limit("total metadata size"));
        }
        if handle.metadata_type(id) != Some("mime")
            || handle.metadata_content_type(id) != Some("application/rdf+xml")
        {
            continue;
        }
        let bytes = handle.metadata(id).map_err(container_error)?;
        for (property, plist_bytes) in parse_xmp(&bytes, limits)? {
            let property = parse_property(property, &plist_bytes, images, limits)?;
            let (name, property) = property;
            insert_metadata_property(&mut metadata, name, property)?;
        }
    }
    Ok(metadata)
}

fn insert_metadata_property(
    metadata: &mut Metadata,
    name: AppleProperty,
    property: PropertyValue,
) -> Result<(), Error> {
    match metadata.insert(name, property) {
        Ok(()) | Err(ModelError::DuplicateProperty(_)) => Ok(()),
        Err(error) => Err(Error::Metadata(error.to_string())),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PropertyName {
    H24,
    Solar,
    Apr,
}

fn property_name(namespace: ResolveResult<'_>, local: &str) -> Option<PropertyName> {
    if !matches!(namespace, ResolveResult::Bound(uri) if uri.as_ref() == APPLE_DESKTOP_NAMESPACE) {
        return None;
    }
    match local {
        "h24" => Some(PropertyName::H24),
        "solar" => Some(PropertyName::Solar),
        "apr" => Some(PropertyName::Apr),
        _ => None,
    }
}

/// Reserves metadata-string capacity fallibly so memory pressure surfaces as a
/// bounded error instead of aborting the process.
fn reserve_metadata_string(buffer: &mut String, len: usize) -> Result<(), Error> {
    buffer
        .try_reserve(len)
        .map_err(|_| Error::Limit("metadata text"))
}

fn append_metadata_text(buffer: &mut String, fragment: &str) -> Result<(), Error> {
    reserve_metadata_string(buffer, fragment.len())?;
    buffer.push_str(fragment);
    Ok(())
}

fn push_property(
    properties: &mut Vec<(PropertyName, Vec<u8>)>,
    property: (PropertyName, Vec<u8>),
) -> Result<(), Error> {
    properties
        .try_reserve(1)
        .map_err(|_| Error::Limit("property count"))?;
    properties.push(property);
    Ok(())
}

fn parse_xmp(bytes: &[u8], limits: &Limits) -> Result<Vec<(PropertyName, Vec<u8>)>, Error> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader
        .resolver_mut()
        .set_max_namespace_bindings(limits.max_xml_events);
    let mut depth = 0usize;
    let mut events = 0usize;
    let mut active: Option<(PropertyName, usize, String)> = None;
    let mut properties = Vec::new();
    loop {
        events += 1;
        if events > limits.max_xml_events {
            return Err(Error::Limit("XMP event count"));
        }
        match reader
            .read_event()
            .map_err(|error| Error::Metadata(error.to_string()))?
        {
            Event::Start(start) => {
                depth += 1;
                if depth > limits.max_xml_depth {
                    return Err(Error::Limit("XMP nesting depth"));
                }
                if active.is_some() {
                    return Err(Error::Metadata("nested markup in Apple property".into()));
                }
                for attribute in start.attributes().with_checks(true) {
                    let attribute =
                        attribute.map_err(|error| Error::Metadata(error.to_string()))?;
                    let (namespace, local) = reader.resolver().resolve_attribute(attribute.key);
                    if let Some(name) = property_name(namespace, local.as_ref()) {
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map_err(|error| Error::Metadata(error.to_string()))?;
                        push_property(
                            &mut properties,
                            (name, decode_plist_base64(&value, limits)?),
                        )?;
                    }
                }
                let (namespace, local) = reader.resolver().resolve_element(start.name());
                if let Some(name) = property_name(namespace, local.as_ref()) {
                    active = Some((name, depth, String::new()));
                }
            }
            Event::Empty(empty) => {
                if depth
                    .checked_add(1)
                    .is_none_or(|empty_depth| empty_depth > limits.max_xml_depth)
                {
                    return Err(Error::Limit("XMP nesting depth"));
                }
                if active.is_some() {
                    return Err(Error::Metadata("nested markup in Apple property".into()));
                }
                for attribute in empty.attributes().with_checks(true) {
                    let attribute =
                        attribute.map_err(|error| Error::Metadata(error.to_string()))?;
                    let (namespace, local) = reader.resolver().resolve_attribute(attribute.key);
                    if let Some(name) = property_name(namespace, local.as_ref()) {
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map_err(|error| Error::Metadata(error.to_string()))?;
                        push_property(
                            &mut properties,
                            (name, decode_plist_base64(&value, limits)?),
                        )?;
                    }
                }
                let (namespace, local) = reader.resolver().resolve_element(empty.name());
                if let Some(name) = property_name(namespace, local.as_ref()) {
                    push_property(&mut properties, (name, decode_plist_base64("", limits)?))?;
                }
            }
            Event::Text(text) => {
                if let Some((_, _, value)) = active.as_mut() {
                    append_metadata_text(value, &text.xml_content(XmlVersion::Implicit1_0))?;
                }
            }
            Event::CData(data) => {
                if let Some((_, _, value)) = active.as_mut() {
                    append_metadata_text(value, &data.xml_content(XmlVersion::Implicit1_0))?;
                }
            }
            Event::End(_) => {
                if let Some((_, property_depth, _)) = active.as_ref() {
                    if *property_depth == depth {
                        let (name, _, value) = active.take().unwrap();
                        push_property(
                            &mut properties,
                            (name, decode_plist_base64(&value, limits)?),
                        )?;
                    }
                }
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::Metadata("unbalanced XMP".into()))?;
            }
            Event::DocType(_) => return Err(Error::Metadata("XMP DTD is not allowed".into())),
            Event::GeneralRef(reference) if active.is_some() => {
                let character = reference
                    .resolve_char_ref()
                    .map_err(|error| Error::Metadata(error.to_string()))?
                    .ok_or_else(|| Error::Metadata("entity in Apple property".into()))?;
                let value = &mut active.as_mut().unwrap().2;
                reserve_metadata_string(value, character.len_utf8())?;
                value.push(character);
            }
            Event::Comment(_) | Event::PI(_) if active.is_some() => {
                return Err(Error::Metadata("markup in Apple property".into()));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 || active.is_some() {
        return Err(Error::Metadata("unbalanced XMP".into()));
    }
    Ok(properties)
}

fn decode_plist_base64(value: &str, limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut compact = String::new();
    reserve_metadata_string(&mut compact, value.len())?;
    for character in value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
    {
        compact.push(character);
    }
    let encoded_limit = limits
        .max_plist_bytes
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or(Error::Limit("property list size"))?;
    if compact.len() > encoded_limit {
        return Err(Error::Limit("property list size"));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|error| Error::Metadata(format!("invalid Base64 property: {error}")))?;
    if decoded.len() > limits.max_plist_bytes {
        return Err(Error::Limit("property list size"));
    }
    Ok(decoded)
}

fn parse_property(
    name: PropertyName,
    bytes: &[u8],
    images: &TopLevelImages,
    limits: &Limits,
) -> Result<(AppleProperty, PropertyValue), Error> {
    // Apple's own dynamic wallpapers encode these properties as XML property
    // lists; generators such as `wallpapper` and `Equinox` emit XML as well.
    // Only the binary encoding gets the structural preflight below, so the XML
    // branch keeps its own byte, depth, and object bounds while the `plist`
    // crate expands it. The binary magic is unambiguous, so anything else is
    // offered to the XML reader rather than gated on a declaration: a property
    // may legally begin with `<plist>`, whitespace, a comment, or a byte-order
    // mark, and the reader rejects whatever is not a property list.
    let value = if bytes.starts_with(b"bplist00") {
        preflight_binary_plist(bytes, limits)?;
        Value::from_reader(std::io::Cursor::new(bytes))
            .map_err(|error| Error::Metadata(format!("invalid binary property list: {error}")))?
    } else {
        preflight_xml_plist(bytes, limits)?;
        Value::from_reader_xml(std::io::Cursor::new(bytes))
            .map_err(|error| Error::Metadata(format!("invalid XML property list: {error}")))?
    };
    let dictionary = value
        .as_dictionary()
        .ok_or_else(|| Error::Metadata("property list root is not a dictionary".into()))?;
    match name {
        PropertyName::H24 => parse_h24(dictionary, images, limits),
        PropertyName::Solar => parse_solar(dictionary, images, limits),
        PropertyName::Apr => parse_apr(dictionary, images),
    }
}

fn parse_h24(
    dictionary: &Dictionary,
    images: &TopLevelImages,
    limits: &Limits,
) -> Result<(AppleProperty, PropertyValue), Error> {
    let points = required_array(dictionary, "ti")?;
    if points.is_empty() || points.len() > limits.max_schedule_points {
        return Err(Error::Metadata("invalid h24 point count".into()));
    }
    let mut parsed = Vec::with_capacity(points.len());
    for point in points {
        let point = required_dictionary(point, "h24 point")?;
        parsed.push(TimePoint {
            image: image_reference(point, "i", images)?,
            time: NormalizedTime::new(number(point, "t")?)
                .map_err(|error| Error::Metadata(error.to_string()))?,
        });
    }
    let appearance = optional_appearance(dictionary, images)?;
    let schedule =
        Schedule::new(parsed, appearance).map_err(|error| Error::Metadata(error.to_string()))?;
    Ok((AppleProperty::Time, PropertyValue::Time(schedule)))
}

fn parse_solar(
    dictionary: &Dictionary,
    images: &TopLevelImages,
    limits: &Limits,
) -> Result<(AppleProperty, PropertyValue), Error> {
    let points = required_array(dictionary, "si")?;
    if points.is_empty() || points.len() > limits.max_schedule_points {
        return Err(Error::Metadata("invalid solar point count".into()));
    }
    let mut parsed = Vec::with_capacity(points.len());
    for point in points {
        let point = required_dictionary(point, "solar point")?;
        parsed.push(SolarPoint {
            image: image_reference(point, "i", images)?,
            position: SolarPosition::new(number(point, "a")?, number(point, "z")?)
                .map_err(|error| Error::Metadata(error.to_string()))?,
        });
    }
    let appearance = optional_appearance(dictionary, images)?;
    let schedule =
        Schedule::new(parsed, appearance).map_err(|error| Error::Metadata(error.to_string()))?;
    Ok((AppleProperty::Solar, PropertyValue::Solar(schedule)))
}

fn parse_apr(
    dictionary: &Dictionary,
    images: &TopLevelImages,
) -> Result<(AppleProperty, PropertyValue), Error> {
    Ok((
        AppleProperty::Appearance,
        PropertyValue::Appearance(parse_appearance(dictionary, images)?),
    ))
}

fn optional_appearance(
    dictionary: &Dictionary,
    images: &TopLevelImages,
) -> Result<Option<Appearance>, Error> {
    dictionary
        .get("ap")
        .map(|value| {
            let dictionary = required_dictionary(value, "ap")?;
            parse_appearance(dictionary, images)
        })
        .transpose()
}

fn parse_appearance(dictionary: &Dictionary, images: &TopLevelImages) -> Result<Appearance, Error> {
    Ok(Appearance {
        dark: image_reference(dictionary, "d", images)?,
        light: image_reference(dictionary, "l", images)?,
    })
}

fn required_array<'a>(dictionary: &'a Dictionary, key: &str) -> Result<&'a Vec<Value>, Error> {
    dictionary
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Metadata(format!("{key} is not an array")))
}

fn required_dictionary<'a>(value: &'a Value, label: &str) -> Result<&'a Dictionary, Error> {
    value
        .as_dictionary()
        .ok_or_else(|| Error::Metadata(format!("{label} is not a dictionary")))
}

fn image_reference(
    dictionary: &Dictionary,
    key: &str,
    images: &TopLevelImages,
) -> Result<ImageReference, Error> {
    let index = dictionary
        .get(key)
        .and_then(exact_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map(ImageReference::from_position)
        .ok_or_else(|| Error::Metadata(format!("{key} is not a valid image index")))?;
    if images.resolve(index).is_none() {
        return Err(Error::Metadata(format!("{key} references a missing image")));
    }
    Ok(index)
}

fn number(dictionary: &Dictionary, key: &str) -> Result<f64, Error> {
    let value = dictionary
        .get(key)
        .ok_or_else(|| Error::Metadata(format!("missing {key}")))?;
    let number = match value {
        Value::Real(value) => *value,
        Value::Integer(value) => value
            .as_signed()
            .map(|value| value as f64)
            .or_else(|| value.as_unsigned().map(|value| value as f64))
            .ok_or_else(|| Error::Metadata(format!("{key} is outside its numeric domain")))?,
        _ => return Err(Error::Metadata(format!("{key} is not numeric"))),
    };
    if !number.is_finite() {
        return Err(Error::Metadata(format!("{key} is not finite")));
    }
    Ok(number)
}

fn exact_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(value) => value.as_unsigned(),
        _ => None,
    }
}

// Bounds an XML property list before the `plist` crate expands it.
//
// The binary format carries an explicit object count and offset table that
// `preflight_binary_plist` validates exactly. XML carries neither, so this
// counts the elements that allocate an object and tracks nesting through a
// conforming XML reader. Reading tags as raw text instead desynchronises on a
// `>` inside a quoted attribute or a closing tag inside a comment, which is
// exactly how crafted nesting escaped the bound.
//
// `plist` matches element names by local name and rejects any element outside
// the property-list set, so this resolves names the same way and counts the
// elements that become values.
fn preflight_xml_plist(bytes: &[u8], limits: &Limits) -> Result<(), Error> {
    if bytes.len() > limits.max_plist_bytes {
        return Err(Error::Limit("property list size"));
    }
    // A leading byte-order mark or whitespace is legal before the declaration.
    // The underlying reader accepts both, so the sniff must not depend on the
    // first byte of the property.
    let content = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let first = content
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .ok_or_else(|| Error::Metadata("property is not a property list".into()))?;
    if content[first] != b'<' {
        return Err(Error::Metadata("property is not a property list".into()));
    }
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader
        .resolver_mut()
        .set_max_namespace_bindings(limits.max_xml_events);
    let mut objects = 0usize;
    let mut expanded_bytes = 0usize;
    let mut depth = 0usize;
    let mut events = 0usize;
    let mut root = false;
    loop {
        events += 1;
        if events > limits.max_xml_events {
            return Err(Error::Limit("property list event count"));
        }
        let event = reader
            .read_event()
            .map_err(|error| Error::Metadata(format!("invalid XML property list: {error}")))?;
        match event {
            Event::Start(element) => {
                root = true;
                depth += 1;
                if depth > limits.max_plist_depth {
                    return Err(Error::Limit("property list nesting depth"));
                }
                let local = reader.resolver().resolve_element(element.name()).1;
                charge_plist_element(local.as_ref(), &mut objects, &mut expanded_bytes, limits)?;
            }
            Event::Empty(element) => {
                root = true;
                // An empty element occupies one level, so it is checked against
                // the same ceiling as a start tag even though it never raises
                // the tracked depth.
                if depth + 1 > limits.max_plist_depth {
                    return Err(Error::Limit("property list nesting depth"));
                }
                let local = reader.resolver().resolve_element(element.name()).1;
                charge_plist_element(local.as_ref(), &mut objects, &mut expanded_bytes, limits)?;
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::Metadata("unbalanced XML property list".into()))?;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 || !root {
        return Err(Error::Metadata("unbalanced XML property list".into()));
    }
    Ok(())
}

// Charges one element against the expansion budget. `plist` turns each of these
// into a `Value`, and a `key` becomes an allocated string as well. Anything
// outside the set is rejected by `plist` itself.
fn charge_plist_element(
    local: &str,
    objects: &mut usize,
    expanded_bytes: &mut usize,
    limits: &Limits,
) -> Result<(), Error> {
    if !matches!(
        local,
        "dict"
            | "array"
            | "data"
            | "date"
            | "integer"
            | "real"
            | "string"
            | "true"
            | "false"
            | "key"
    ) {
        return Ok(());
    }
    *objects = objects
        .checked_add(1)
        .ok_or(Error::Limit("expanded property list object count"))?;
    if *objects > limits.max_expanded_plist_objects {
        return Err(Error::Limit("expanded property list object count"));
    }
    // The expansion charges one `Value` per object. The binary path also
    // charges payload and collection bytes, but an XML property list is not
    // compressed, so its payload is already bounded by the encoded-size check
    // above and the object count is what remains to bound here.
    charge_expanded_bytes(expanded_bytes, 64, limits)
}

// Validates the object graph and its expansion before `plist` allocates it.
fn preflight_binary_plist(bytes: &[u8], limits: &Limits) -> Result<(), Error> {
    if bytes.len() < 40 || !bytes.starts_with(b"bplist00") {
        return Err(Error::Metadata(
            "property is not a binary property list".into(),
        ));
    }
    let trailer = bytes.len() - 32;
    let offset_size = usize::from(bytes[trailer + 6]);
    let reference_size = usize::from(bytes[trailer + 7]);
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&reference_size) {
        return Err(Error::Metadata(
            "invalid property list integer width".into(),
        ));
    }
    let object_count = read_be(bytes, trailer + 8, 8)?;
    let root = read_be(bytes, trailer + 16, 8)?;
    let table_offset = read_be(bytes, trailer + 24, 8)?;
    let object_count =
        usize::try_from(object_count).map_err(|_| Error::Limit("property list object count"))?;
    if object_count == 0 || object_count > limits.max_plist_objects {
        return Err(Error::Limit("property list object count"));
    }
    let root = usize::try_from(root).map_err(|_| Error::Metadata("invalid root object".into()))?;
    if root >= object_count {
        return Err(Error::Metadata("invalid root object".into()));
    }
    let table_offset = usize::try_from(table_offset)
        .map_err(|_| Error::Metadata("invalid offset table".into()))?;
    let table_bytes = object_count
        .checked_mul(offset_size)
        .ok_or_else(|| Error::Metadata("invalid offset table".into()))?;
    if table_offset < 8 || table_offset.checked_add(table_bytes) != Some(trailer) {
        return Err(Error::Metadata("invalid offset table".into()));
    }
    let mut offsets = Vec::with_capacity(object_count);
    for index in 0..object_count {
        let position = table_offset + index * offset_size;
        let offset = usize::try_from(read_be(bytes, position, offset_size)?)
            .map_err(|_| Error::Metadata("invalid object offset".into()))?;
        if !(8..table_offset).contains(&offset) {
            return Err(Error::Metadata("invalid object offset".into()));
        }
        offsets.push(offset);
    }

    let mut stack = vec![(root, 1usize)];
    let mut expanded = 0usize;
    let mut expanded_bytes = 0usize;
    while let Some((reference, depth)) = stack.pop() {
        expanded += 1;
        if expanded > limits.max_expanded_plist_objects {
            return Err(Error::Limit("expanded property list object count"));
        }
        charge_expanded_bytes(&mut expanded_bytes, 64, limits)?;
        if depth > limits.max_plist_depth {
            return Err(Error::Limit("property list nesting depth"));
        }
        let offset = offsets[reference];
        let marker = bytes[offset];
        let kind = marker >> 4;
        let info = marker & 0x0f;
        match kind {
            0x0 => {
                if !matches!(info, 0x0 | 0x8 | 0x9) {
                    return Err(Error::Metadata(
                        "unsupported property list primitive".into(),
                    ));
                }
            }
            0x1 | 0x2 => {
                let size = 1usize
                    .checked_shl(u32::from(info))
                    .ok_or_else(|| Error::Metadata("invalid numeric object".into()))?;
                checked_range(offset + 1, size, table_offset)?;
            }
            0x3 => {
                if info != 3 {
                    return Err(Error::Metadata("invalid date object".into()));
                }
                checked_range(offset + 1, 8, table_offset)?;
            }
            0x4..=0x6 => {
                let (length, start) = object_length(bytes, offset, info, table_offset)?;
                let unit = if kind == 0x6 { 2 } else { 1 };
                let size = length
                    .checked_mul(unit)
                    .ok_or_else(|| Error::Metadata("invalid data object".into()))?;
                checked_range(start, size, table_offset)?;
                let expanded_size = if kind == 0x6 {
                    length.checked_mul(3)
                } else {
                    Some(length)
                }
                .ok_or(Error::Limit("expanded property list memory"))?;
                charge_expanded_bytes(&mut expanded_bytes, expanded_size, limits)?;
            }
            0x8 => {
                let size = usize::from(info) + 1;
                checked_range(offset + 1, size, table_offset)?;
            }
            0xa | 0xc | 0xd => {
                let (length, start) = object_length(bytes, offset, info, table_offset)?;
                let reference_count = if kind == 0xd {
                    length
                        .checked_mul(2)
                        .ok_or_else(|| Error::Metadata("invalid dictionary".into()))?
                } else {
                    length
                };
                let size = reference_count
                    .checked_mul(reference_size)
                    .ok_or_else(|| Error::Metadata("invalid reference list".into()))?;
                checked_range(start, size, table_offset)?;
                let collection_bytes = reference_count
                    .checked_mul(16)
                    .ok_or(Error::Limit("expanded property list memory"))?;
                charge_expanded_bytes(&mut expanded_bytes, collection_bytes, limits)?;
                let planned = expanded
                    .checked_add(stack.len())
                    .and_then(|count| count.checked_add(reference_count))
                    .ok_or(Error::Limit("expanded property list object count"))?;
                if planned > limits.max_expanded_plist_objects {
                    return Err(Error::Limit("expanded property list object count"));
                }
                for index in 0..reference_count {
                    let child = usize::try_from(read_be(
                        bytes,
                        start + index * reference_size,
                        reference_size,
                    )?)
                    .map_err(|_| Error::Metadata("invalid object reference".into()))?;
                    if child >= object_count {
                        return Err(Error::Metadata("invalid object reference".into()));
                    }
                    stack.push((child, depth + 1));
                }
            }
            _ => return Err(Error::Metadata("unsupported property list object".into())),
        }
    }
    Ok(())
}

fn charge_expanded_bytes(total: &mut usize, amount: usize, limits: &Limits) -> Result<(), Error> {
    *total = total
        .checked_add(amount)
        .ok_or(Error::Limit("expanded property list memory"))?;
    if *total > limits.max_expanded_plist_bytes {
        return Err(Error::Limit("expanded property list memory"));
    }
    Ok(())
}

fn object_length(
    bytes: &[u8],
    object_offset: usize,
    info: u8,
    end: usize,
) -> Result<(usize, usize), Error> {
    if info < 0x0f {
        return Ok((usize::from(info), object_offset + 1));
    }
    checked_range(object_offset + 1, 1, end)?;
    let marker = bytes[object_offset + 1];
    if marker >> 4 != 0x1 || marker & 0x0f > 3 {
        return Err(Error::Metadata("invalid extended object length".into()));
    }
    let width = 1usize << (marker & 0x0f);
    let length = usize::try_from(read_be(bytes, object_offset + 2, width)?)
        .map_err(|_| Error::Metadata("object length is too large".into()))?;
    checked_range(object_offset + 2, width, end)?;
    Ok((length, object_offset + 2 + width))
}

fn checked_range(start: usize, length: usize, end: usize) -> Result<(), Error> {
    if start.checked_add(length).is_none_or(|value| value > end) {
        return Err(Error::Metadata("truncated property list object".into()));
    }
    Ok(())
}

fn read_be(bytes: &[u8], start: usize, length: usize) -> Result<u64, Error> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| Error::Metadata("integer offset overflow".into()))?;
    let slice = bytes
        .get(start..end)
        .ok_or_else(|| Error::Metadata("truncated property list integer".into()))?;
    let mut value = 0u64;
    for byte in slice {
        value = value
            .checked_shl(8)
            .and_then(|value| value.checked_add(u64::from(*byte)))
            .ok_or_else(|| Error::Metadata("property list integer overflow".into()))?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dynamic-heic")
            .join(name)
    }

    fn binary_plist(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        value.to_writer_binary(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn plist_preflight_counts_nesting_that_markup_hides() {
        let limits = Limits::default();
        let depth = limits.max_plist_depth * 40;
        // A `>` inside a quoted attribute used to end the tag early, so the
        // scanner saw a self-closing element with a one-character name and
        // charged neither depth nor an object. Both quote styles must be read
        // the same way.
        for opener in ["<p:array a='/>'>", "<p:array a=\"/>\" >"] {
            let mut qualified = String::from(
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>h24</key>",
            );
            for _ in 0..depth {
                qualified.push_str(opener);
            }
            for _ in 0..depth {
                qualified.push_str("</p:array>");
            }
            qualified.push_str("</dict></plist>");
            assert!(
                matches!(
                    preflight_xml_plist(qualified.as_bytes(), &limits),
                    Err(Error::Limit("property list nesting depth"))
                ),
                "{opener} was not counted as nesting"
            );
        }

        // A closing tag inside a comment used to erase tracked depth.
        let mut commented =
            String::from("<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>h24</key>");
        for _ in 0..depth {
            commented.push_str("<array><!-- > </array> -->");
        }
        commented.push_str("</dict></plist>");
        assert!(preflight_xml_plist(commented.as_bytes(), &limits).is_err());

        // The depth a crafted document reaches must also stay inside the event
        // budget, so it cannot trade one ceiling for the other.
        let mut dense = String::from("<plist><dict>");
        for _ in 0..limits.max_xml_events {
            dense.push_str("<string>x</string>");
        }
        dense.push_str("</dict></plist>");
        assert!(matches!(
            preflight_xml_plist(dense.as_bytes(), &limits),
            Err(Error::Limit("property list event count"))
        ));
    }

    #[test]
    fn plist_preflight_counts_every_allocating_element() {
        let limits = Limits::default();
        // A processing instruction and a CDATA section must not hide the
        // elements around them, and an empty element allocates like any other.
        let document = "<plist><?pi a='>'>?><dict><key>k</key><array><true/><false/>\
                        <string><![CDATA[ ]]>x</string></array></dict></plist>";
        assert!(preflight_xml_plist(document.as_bytes(), &limits).is_ok());
        assert!(Value::from_reader_xml(std::io::Cursor::new(document)).is_ok());

        // Keys allocate a string, so they count against the object ceiling.
        // The default ceilings make the event budget the tighter bound for a
        // key-heavy document, so lower the object ceiling to show the charge.
        let tight = Limits {
            max_expanded_plist_objects: 2,
            ..Limits::default()
        };
        let keys = "<plist><dict><key>a</key><key>b</key></dict></plist>";
        assert!(matches!(
            preflight_xml_plist(keys.as_bytes(), &tight),
            Err(Error::Limit("expanded property list object count"))
        ));

        // The same shape under the default ceilings stops at the event budget.
        let mut dense = String::from("<plist><dict>");
        for _ in 0..limits.max_xml_events {
            dense.push_str("<key>k</key>");
        }
        dense.push_str("</dict></plist>");
        assert!(matches!(
            preflight_xml_plist(dense.as_bytes(), &limits),
            Err(Error::Limit("property list event count"))
        ));
    }

    #[test]
    fn plist_accepts_legal_xml_preambles() {
        let limits = Limits::default();
        let body = "<plist version=\"1.0\"><dict><key>h24</key><string>x</string></dict></plist>";
        for property in [
            body.as_bytes().to_vec(),
            format!("  \n\t{body}").into_bytes(),
            format!("<!-- lead -->{body}").into_bytes(),
            format!("<?xml version=\"1.0\"?>\n{body}").into_bytes(),
            [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat(),
        ] {
            assert!(preflight_xml_plist(&property, &limits).is_ok());
            assert!(Value::from_reader_xml(std::io::Cursor::new(&property)).is_ok());
        }
        // Anything that is not markup is still refused.
        assert!(preflight_xml_plist(b"not a property list", &limits).is_err());
        assert!(preflight_xml_plist(&[0, 1, 2, 3], &limits).is_err());
    }

    #[test]
    fn property_parsing_accepts_a_declaration_free_xml_list() {
        // The sniff used to require an XML declaration or doctype, so a
        // property list that began with `<plist>` was rejected outright.
        let images = TopLevelImages::new(vec![HeifItemId::new(1), HeifItemId::new(2)]);
        let xml = b"<plist version=\"1.0\"><dict><key>l</key><integer>0</integer><key>d</key><integer>1</integer></dict></plist>";
        let (name, value) =
            parse_property(PropertyName::Apr, xml, &images, &Limits::default()).unwrap();
        assert_eq!(name, AppleProperty::Appearance);
        assert!(matches!(value, PropertyValue::Appearance(_)));
    }

    #[test]
    fn container_preflight_runs_before_libheif() {
        use super::container::fixture::{boxed, iinf, infe, ipma, meta};
        // The aggregate ceiling rejects the file before libheif parses it, so
        // the container only needs enough structure to reach the check.
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        let mut children = vec![iinf(&[infe(1, b"hvc1", None)])];
        for _ in 0..4 {
            children.push(boxed(b"iprp", &ipma(&entries)));
        }
        let path =
            std::env::temp_dir().join(format!("genkan-aggregate-heic-{}", std::process::id()));
        std::fs::write(&path, meta(&children)).unwrap();
        let result = Document::open(&path);
        std::fs::remove_file(path).unwrap();
        assert!(matches!(
            result,
            Err(Error::Limit("property association count"))
        ));
    }

    #[test]
    fn metadata_buffers_reserve_fallibly() {
        let mut buffer = String::new();
        reserve_metadata_string(&mut buffer, 8).unwrap();
        buffer.push_str("metadata");
        append_metadata_text(&mut buffer, " more").unwrap();
        assert_eq!(buffer, "metadata more");

        // A reservation that cannot be satisfied returns an error instead of
        // aborting the process under memory pressure.
        assert!(matches!(
            reserve_metadata_string(&mut buffer, usize::MAX),
            Err(Error::Limit("metadata text"))
        ));
        let mut properties = Vec::new();
        assert!(push_property(&mut properties, (PropertyName::H24, vec![1, 2, 3])).is_ok());
        assert_eq!(properties.len(), 1);
    }

    fn h24_value(index: Value, time: Value) -> Value {
        let mut point = Dictionary::new();
        point.insert("i".into(), index);
        point.insert("t".into(), time);
        let mut root = Dictionary::new();
        root.insert("ti".into(), Value::Array(vec![Value::Dictionary(point)]));
        Value::Dictionary(root)
    }

    fn repeated_reference_plist() -> Vec<u8> {
        let mut bytes = b"bplist00".to_vec();
        bytes.extend_from_slice(&[0x51, b'x']);
        bytes.extend_from_slice(&[0xa4, 0, 0, 0, 0]);
        bytes.extend_from_slice(&[8, 10]);
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&[1, 1]);
        bytes.extend_from_slice(&2u64.to_be_bytes());
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&15u64.to_be_bytes());
        bytes
    }

    fn repeated_data_plist(payload_bytes: usize, references: u8) -> Vec<u8> {
        assert!(payload_bytes <= u8::MAX as usize);
        assert!(references <= 14);
        let mut bytes = b"bplist00".to_vec();
        let data_offset = bytes.len();
        bytes.extend_from_slice(&[0x4f, 0x10, payload_bytes as u8]);
        bytes.resize(bytes.len() + payload_bytes, b'x');
        let array_offset = bytes.len();
        bytes.push(0xa0 | references);
        bytes.resize(bytes.len() + usize::from(references), 0);
        let table_offset = bytes.len();
        bytes.extend_from_slice(&[data_offset as u8, array_offset as u8]);
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&[1, 1]);
        bytes.extend_from_slice(&2u64.to_be_bytes());
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&(table_offset as u64).to_be_bytes());
        bytes
    }

    fn assert_color(frame: &RgbaFrame, expected: [u8; 4]) {
        assert_eq!((frame.width, frame.height), (8, 8));
        assert_eq!(frame.pixels.len(), 8 * 8 * 4);
        for pixel in frame.pixels.chunks_exact(4) {
            for channel in 0..3 {
                assert!(pixel[channel].abs_diff(expected[channel]) <= 2);
            }
            assert_eq!(pixel[3], 255);
        }
    }

    #[test]
    fn parses_and_decodes_all_supported_properties() {
        let document = Document::open(&fixture("synthetic-all-properties.heic")).unwrap();
        assert_eq!(document.item_ids().collect::<Vec<_>>(), &[1, 2, 3, 4]);
        assert_eq!(document.primary_image().position(), 0);
        let time = document.metadata().time().unwrap();
        assert_eq!(
            time.points()
                .iter()
                .map(|point| (point.image.position(), point.time.value()))
                .collect::<Vec<_>>(),
            [(0, 0.0), (1, 0.25), (2, 0.5), (3, 0.75)]
        );
        let solar = document.metadata().solar().unwrap();
        assert_eq!(
            solar
                .points()
                .iter()
                .map(|point| (
                    point.image.position(),
                    point.position.altitude_degrees(),
                    point.position.azimuth_degrees(),
                ))
                .collect::<Vec<_>>(),
            [
                (0, -20.0, 0.0),
                (1, 15.0, 90.0),
                (2, 55.0, 180.0),
                (3, 10.0, 270.0),
            ]
        );
        let appearance = document.metadata().appearance().unwrap();
        assert_eq!(
            (appearance.light.position(), appearance.dark.position()),
            (0, 3)
        );
        for (index, color) in [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ]
        .into_iter()
        .enumerate()
        {
            assert_color(
                &document
                    .decode(ImageReference::from_position(index))
                    .unwrap(),
                color,
            );
        }
    }

    #[test]
    fn independent_fixture_preserves_non_contiguous_item_ids() {
        let document = Document::open(&fixture("imageio-wallpapper-h24.heic")).unwrap();
        assert_eq!(document.item_ids().collect::<Vec<_>>(), &[1, 3, 4, 5]);
        assert_eq!(
            document
                .images()
                .resolve(ImageReference::from_position(2))
                .map(HeifItemId::value),
            Some(4)
        );
        assert!(document.metadata().time().is_some());
        assert!(document.metadata().solar().is_none());
        assert!(document.metadata().appearance().is_none());
    }

    #[test]
    fn wallpapper_fixture_decodes_despite_the_coded_picture_margin() {
        // This file's SPS declares a 160x64 coded picture that crops to the
        // declared 8x8. libheif 1.23.1 and 1.23.2 tightened the permitted coded
        // size to one coding unit beyond the `ispe` dimensions, which is
        // 72x72 = 5184 here, and rejected the file before the decoder plugin
        // ran. libheif 1.23.3 added a 65536-pixel floor to that tightening
        // (upstream issue #1856), so the coded margin no longer decides whether
        // a valid stream decodes. See issue #50.
        let document = Document::open(&fixture("imageio-wallpapper-h24.heic")).unwrap();
        for (index, color) in [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ]
        .into_iter()
        .enumerate()
        {
            assert_color(
                &document
                    .decode(ImageReference::from_position(index))
                    .unwrap(),
                color,
            );
        }
    }

    #[test]
    fn retained_descriptor_survives_path_replacement_until_lazy_decode() {
        let directory = std::env::temp_dir().join(format!(
            "genkan-dynamic-heic-replacement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("wallpaper.heic");
        std::fs::copy(fixture("synthetic-all-properties.heic"), &path).unwrap();
        let document = Document::open(&path).unwrap();
        std::fs::write(directory.join("replacement"), b"not a HEIC").unwrap();
        std::fs::rename(directory.join("replacement"), &path).unwrap();

        assert_color(
            &document.decode(ImageReference::from_position(2)).unwrap(),
            [0, 0, 255, 255],
        );
        drop(document);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fifo_and_directory_sources_are_rejected_without_blocking() {
        use rustix::fs::Mode;

        let directory =
            std::env::temp_dir().join(format!("genkan-dynamic-heic-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let fifo = directory.join("wallpaper.heic");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();

        assert!(matches!(Document::open(&fifo), Err(Error::Open(_))));
        assert!(matches!(Document::open(&directory), Err(Error::Open(_))));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_relative_non_regular_and_oversized_sources() {
        assert!(matches!(
            Document::open(Path::new("relative.heic")),
            Err(Error::Open(_))
        ));
        assert!(matches!(
            Document::open(Path::new("/dev/null")),
            Err(Error::Open(_))
        ));
        let limits = Limits {
            max_source_bytes: 1,
            ..Limits::default()
        };
        assert!(matches!(
            Document::open_with_limits(&fixture("synthetic-all-properties.heic"), limits),
            Err(Error::SourceTooLarge)
        ));
    }

    /// Reads the embedded ICC profile of a fixture's first top-level image.
    fn fixture_profile(name: &str) -> Vec<u8> {
        let document = Document::open(&fixture(name)).unwrap();
        let handle = document
            .context
            .image_handle(document.item_ids().next().unwrap())
            .unwrap();
        handle
            .color_profile_raw()
            .expect("the fixture carries an ICC profile")
            .data
    }

    fn raw_profile(bytes: Vec<u8>) -> libheif_rs::ColorProfileRaw {
        libheif_rs::ColorProfileRaw::new(libheif_rs::color_profile_types::PROF, bytes)
    }

    /// Repoints one of a fixture's descriptive tags at an appended `mluc` tag
    /// with `records` localization records that all reference the same
    /// `payload` bytes, leaving every other tag intact. This is the shape that
    /// made the profile parser materialize far more memory than the profile
    /// occupies: it bounds each record against the tag but not the sum across
    /// records.
    fn repoint_at_mluc(
        profile: &mut Vec<u8>,
        signature: &[u8; 4],
        tag_size: usize,
        records: usize,
        payload: usize,
    ) {
        let tag_count =
            u32::from_be_bytes([profile[128], profile[129], profile[130], profile[131]]) as usize;
        let entry = (0..tag_count)
            .map(|index| 132 + 12 * index)
            .find(|entry| &profile[*entry..*entry + 4] == signature)
            .unwrap_or_else(|| {
                panic!(
                    "the fixture has no {} tag",
                    String::from_utf8_lossy(signature)
                )
            });
        let appended_at = profile.len();

        let mut tag = vec![0u8; tag_size];
        tag[0..4].copy_from_slice(b"mluc");
        tag[8..12].copy_from_slice(&(records as u32).to_be_bytes());
        let offset = tag_size - payload;
        // The first record's length and offset sit at fixed positions.
        tag[20..24].copy_from_slice(&(payload as u32).to_be_bytes());
        tag[24..28].copy_from_slice(&(offset as u32).to_be_bytes());
        for record in 1..records {
            let at = 28 + 12 * (record - 1);
            tag[at + 4..at + 8].copy_from_slice(&(payload as u32).to_be_bytes());
            tag[at + 8..at + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        }

        profile.extend_from_slice(&tag);
        let total = profile.len() as u32;
        profile[0..4].copy_from_slice(&total.to_be_bytes());
        profile[entry + 4..entry + 8].copy_from_slice(&(appended_at as u32).to_be_bytes());
        profile[entry + 8..entry + 12].copy_from_slice(&(tag_size as u32).to_be_bytes());
    }

    /// A fixture profile whose `desc` tag is an `mluc` tag of the given shape.
    fn mluc_profile(tag_size: usize, records: usize, payload: usize) -> Vec<u8> {
        let mut profile = fixture_profile("synthetic-icc.heic");
        repoint_at_mluc(&mut profile, b"desc", tag_size, records, payload);
        profile
    }

    #[test]
    fn the_fixture_profile_is_structurally_conforming() {
        // The fixture is the valid-profile control for the preflight, so its
        // own structure has to hold: a declared extent that matches the bytes,
        // a tag table that fits and is sorted, tag data on four-byte
        // boundaries, and the tags a v2 display profile is required to carry.
        // It also pins reproducibility, because the generator previously
        // serialized uninitialized reserved bytes and regenerating the fixture
        // changed its hash.
        for name in ["synthetic-icc.heic"] {
            let profile = fixture_profile(name);
            assert_eq!(
                usize::try_from(u32::from_be_bytes([
                    profile[0], profile[1], profile[2], profile[3]
                ]))
                .unwrap(),
                profile.len(),
                "{name} declares an extent that is not its length"
            );
            assert_eq!(profile.len() % 4, 0, "{name} is not four-byte aligned");
            let tag_count =
                u32::from_be_bytes([profile[128], profile[129], profile[130], profile[131]])
                    as usize;
            assert!(tag_count > 0 && 132 + 12 * tag_count <= profile.len());

            let mut signatures = Vec::new();
            for index in 0..tag_count {
                let entry = 132 + 12 * index;
                signatures.push(profile[entry..entry + 4].to_vec());
                let offset = u32::from_be_bytes([
                    profile[entry + 4],
                    profile[entry + 5],
                    profile[entry + 6],
                    profile[entry + 7],
                ]) as usize;
                let size = u32::from_be_bytes([
                    profile[entry + 8],
                    profile[entry + 9],
                    profile[entry + 10],
                    profile[entry + 11],
                ]) as usize;
                assert_eq!(offset % 4, 0, "{name} tag {index} is not aligned");
                assert!(size > 0 && offset + size <= profile.len());
                assert_eq!(
                    &profile[offset + 4..offset + 8],
                    &[0, 0, 0, 0],
                    "{name} tag {index} reserved bytes are not zeroed"
                );
            }
            // ICC requires unique signatures, not a particular order; the
            // generator writes them ascending so the fixture stays
            // deterministic, and pinning that catches an accidental reorder.
            let mut sorted = signatures.clone();
            sorted.sort();
            assert_eq!(signatures, sorted, "{name} tag table changed order");
            let mut unique = sorted.clone();
            unique.dedup();
            assert_eq!(unique.len(), sorted.len(), "{name} repeats a signature");
            for required in [
                b"desc".as_slice(),
                b"cprt".as_slice(),
                b"wtpt".as_slice(),
                b"rXYZ".as_slice(),
                b"gXYZ".as_slice(),
                b"bXYZ".as_slice(),
                b"rTRC".as_slice(),
                b"gTRC".as_slice(),
                b"bTRC".as_slice(),
            ] {
                assert!(
                    signatures.iter().any(|signature| signature == required),
                    "{name} is missing the required {} tag",
                    String::from_utf8_lossy(required)
                );
            }

            // The description carries the full v2 tail: a Unicode language code
            // and count, then the ScriptCode code, count, and its fixed 67-byte
            // description field.
            let index = signatures
                .iter()
                .position(|signature| signature == b"desc")
                .expect("the fixture has a desc tag");
            let entry = 132 + 12 * index;
            let offset = u32::from_be_bytes([
                profile[entry + 4],
                profile[entry + 5],
                profile[entry + 6],
                profile[entry + 7],
            ]) as usize;
            let size = u32::from_be_bytes([
                profile[entry + 8],
                profile[entry + 9],
                profile[entry + 10],
                profile[entry + 11],
            ]) as usize;
            let payload = &profile[offset..offset + size];
            assert_eq!(&payload[0..4], b"desc");
            let ascii =
                u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]) as usize;
            assert_eq!(ascii, 41, "{name} description length changed");
            assert_eq!(
                size - 12 - ascii,
                78,
                "{name} description is missing its v2 tail"
            );
            assert_eq!(
                payload[12 + ascii - 1],
                0,
                "{name} description is not NUL terminated"
            );
            assert_eq!(
                &payload[12 + ascii..],
                &[0u8; 78],
                "{name} description tail is not zero filled"
            );

            // The header has to name a v2.1.0 RGB display profile with an XYZ
            // connection space and the D50 PCS illuminant.
            assert_eq!(
                u32::from_be_bytes([profile[8], profile[9], profile[10], profile[11]]),
                0x0210_0000,
                "{name} is not a v2.1.0 profile"
            );
            assert_eq!(&profile[12..16], b"mntr", "{name} is not a display profile");
            assert_eq!(&profile[16..20], b"RGB ", "{name} is not an RGB profile");
            assert_eq!(&profile[20..24], b"XYZ ", "{name} PCS is not XYZ");
            assert_eq!(&profile[36..40], b"acsp", "{name} has no ICC signature");
            assert_eq!(
                [
                    u32::from_be_bytes([profile[68], profile[69], profile[70], profile[71]]),
                    u32::from_be_bytes([profile[72], profile[73], profile[74], profile[75]]),
                    u32::from_be_bytes([profile[76], profile[77], profile[78], profile[79]]),
                ],
                [0x0000_f6d6, 0x0001_0000, 0x0000_d32d],
                "{name} PCS illuminant is not D50"
            );

            // The copyright tag is a `text` type whose notice is NUL
            // terminated.
            let index = signatures
                .iter()
                .position(|signature| signature == b"cprt")
                .expect("the fixture has a cprt tag");
            let entry = 132 + 12 * index;
            let offset = u32::from_be_bytes([
                profile[entry + 4],
                profile[entry + 5],
                profile[entry + 6],
                profile[entry + 7],
            ]) as usize;
            let size = u32::from_be_bytes([
                profile[entry + 8],
                profile[entry + 9],
                profile[entry + 10],
                profile[entry + 11],
            ]) as usize;
            let payload = &profile[offset..offset + size];
            assert_eq!(&payload[0..4], b"text");
            assert_eq!(
                payload[size - 1],
                0,
                "{name} copyright is not NUL terminated"
            );

            // The preflight must accept what the generator produced.
            assert!(preflight_icc_profile(&profile, &Limits::default()).is_ok());
        }
    }

    #[test]
    fn icc_preflight_rejects_text_that_exceeds_the_byte_ceiling() {
        // Few enough records to stay under the record ceiling, but each one
        // references half the tag, so the sum is what exceeds the budget. This
        // is the shape that expanded a 64 KiB tag into 149 MB of retained
        // strings before the preflight existed.
        let profile = mluc_profile(512 * 1024, 16, 256 * 1024);
        assert!(profile.len() <= Limits::default().max_color_profile_bytes);
        assert!(matches!(
            IccTransform::new(&raw_profile(profile), &Limits::default()),
            Err(Error::UnsupportedColor("embedded ICC profile text size"))
        ));
    }

    #[test]
    fn icc_preflight_rejects_a_flood_of_zero_length_records() {
        // The parser keeps three strings per record whatever length the record
        // declares, so zero-length records cost record storage without costing
        // any text. Only the record ceiling can refuse this shape.
        let profile = mluc_profile(64 * 1024, (64 * 1024 - 28) / 12, 0);
        assert!(matches!(
            IccTransform::new(&raw_profile(profile), &Limits::default()),
            Err(Error::UnsupportedColor("embedded ICC profile text records"))
        ));
    }

    #[test]
    fn icc_preflight_accepts_a_heavily_localized_profile() {
        // A profile that localizes its description and copyright into dozens of
        // languages is legitimate, so the record ceiling has to sit well above
        // the per-tag count a real file reaches.
        let mut profile = fixture_profile("synthetic-icc.heic");
        repoint_at_mluc(&mut profile, b"desc", 32 * 1024, 200, 16);
        repoint_at_mluc(&mut profile, b"cprt", 32 * 1024, 200, 16);
        assert!(profile.len() <= Limits::default().max_color_profile_bytes);
        assert!(
            preflight_icc_profile(&profile, &Limits::default()).is_ok(),
            "400 localization records across two tags must be accepted"
        );
        // Two tags of 600 records each exceed the ceiling, and the records are
        // what refuses it: every payload is small enough that the text ceiling
        // would not.
        let mut profile = fixture_profile("synthetic-icc.heic");
        repoint_at_mluc(&mut profile, b"desc", 32 * 1024, 600, 16);
        repoint_at_mluc(&mut profile, b"cprt", 32 * 1024, 600, 16);
        assert!(matches!(
            preflight_icc_profile(&profile, &Limits::default()),
            Err(Error::UnsupportedColor("embedded ICC profile text records"))
        ));
    }

    #[test]
    fn icc_preflight_accepts_bounded_text_records() {
        // One record referencing a small payload is the ordinary shape and must
        // keep working, so the ceilings refuse amplification rather than text.
        let profile = mluc_profile(64 * 1024, 1, 32);
        assert!(preflight_icc_profile(&profile, &Limits::default()).is_ok());
    }

    #[test]
    fn icc_preflight_charges_decoded_rather_than_encoded_sizes() {
        // UTF-16 decodes to at most three UTF-8 bytes per two-byte code unit,
        // and lossy UTF-8 conversion turns every invalid byte into a three-byte
        // replacement character. A budget expressed in encoded lengths would
        // undercount both by up to a factor of three.
        let limits = Limits::default();
        let mut text = 0usize;
        let mut records = 0usize;
        // Two code units decode to six bytes, not four.
        let mut mluc = vec![0u8; 32];
        mluc[0..4].copy_from_slice(b"mluc");
        mluc[8..12].copy_from_slice(&1u32.to_be_bytes());
        mluc[20..24].copy_from_slice(&4u32.to_be_bytes());
        mluc[24..28].copy_from_slice(&28u32.to_be_bytes());
        mluc.extend_from_slice(&[0u8; 4]);
        charge_icc_text(&mluc, &mut text, &mut records, &limits).unwrap();
        assert_eq!(text, 6);
        assert_eq!(records, 1);

        // A description's ASCII section is charged at its worst-case expansion,
        // and its Unicode section is charged too.
        let mut desc = vec![0u8; 12];
        desc[0..4].copy_from_slice(b"desc");
        desc[8..12].copy_from_slice(&4u32.to_be_bytes());
        desc.extend_from_slice(b"abcd");
        desc.extend_from_slice(&0u32.to_be_bytes()); // Unicode language code
        desc.extend_from_slice(&2u32.to_be_bytes()); // two UTF-16 code units
        desc.extend_from_slice(&[0u8; 4]);
        text = 0;
        records = 0;
        charge_icc_text(&desc, &mut text, &mut records, &limits).unwrap();
        assert_eq!(text, 12 + 6);
        assert_eq!(records, 1);
    }

    #[test]
    fn icc_preflight_rejects_a_declared_extent_that_does_not_match() {
        let limits = Limits::default();
        let mut too_small = fixture_profile("synthetic-icc.heic");
        too_small[0..4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(
            preflight_icc_profile(&too_small, &limits),
            Err(Error::UnsupportedColor("embedded ICC profile extent"))
        ));
        let mut too_large = fixture_profile("synthetic-icc.heic");
        too_large[0..4].copy_from_slice(&[0x00, 0x10, 0x00, 0x00]);
        assert!(matches!(
            preflight_icc_profile(&too_large, &limits),
            Err(Error::UnsupportedColor("embedded ICC profile extent"))
        ));
    }

    #[test]
    fn icc_preflight_rejects_a_tag_count_or_extent_above_the_ceiling() {
        let limits = Limits::default();
        let mut profile = fixture_profile("synthetic-icc.heic");
        profile[128..132].copy_from_slice(&(u32::MAX).to_be_bytes());
        assert!(matches!(
            preflight_icc_profile(&profile, &limits),
            Err(Error::UnsupportedColor("embedded ICC profile tag count"))
        ));

        let mut profile = fixture_profile("synthetic-icc.heic");
        profile[128..132].copy_from_slice(&1u32.to_be_bytes());
        // The single tag entry claims an extent past the profile.
        profile[132..136].copy_from_slice(b"desc");
        profile[136..140].copy_from_slice(&8u32.to_be_bytes());
        profile[140..144].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            preflight_icc_profile(&profile, &limits),
            Err(Error::UnsupportedColor("malformed embedded ICC profile"))
        ));

        let tight = Limits {
            max_color_profile_tags: 1,
            ..Limits::default()
        };
        assert!(matches!(
            preflight_icc_profile(&fixture_profile("synthetic-icc.heic"), &tight),
            Err(Error::UnsupportedColor("embedded ICC profile tag count"))
        ));
    }

    #[test]
    fn embedded_icc_profile_is_converted_to_srgb() {
        // The fixture's profile has the sRGB/Rec.709 primaries and a gamma 1.0
        // tone curve, so every pixel is the sRGB encoding of the linear-light
        // value the file declares. The expected values are that encoding
        // rounded to eight bits, and the tolerance covers the lossy HEVC round
        // trip through 4:2:0 chroma.
        let document = Document::open(&fixture("synthetic-icc.heic")).unwrap();
        assert_eq!(document.item_ids().count(), 5);
        for (position, expected) in [
            [241, 99, 99, 255],
            [99, 241, 99, 255],
            [99, 99, 241, 255],
            [188, 188, 188, 255],
            [99, 99, 99, 255],
        ]
        .into_iter()
        .enumerate()
        {
            assert_color(
                &document
                    .decode(ImageReference::from_position(position))
                    .unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn an_unconvertible_embedded_profile_fails_closed() {
        let document = Document::open(&fixture("synthetic-icc-unsupported.heic")).unwrap();
        assert!(matches!(
            document.decode(ImageReference::from_position(0)),
            Err(Error::UnsupportedColor("non-RGB embedded ICC profile"))
        ));
    }

    #[test]
    fn icc_validation_rejects_malformed_empty_and_oversized_profiles() {
        let limits = Limits::default();
        assert!(matches!(
            IccTransform::new(&raw_profile(Vec::new()), &limits),
            Err(Error::UnsupportedColor("embedded ICC profile size"))
        ));
        assert!(matches!(
            IccTransform::new(&raw_profile(b"not a color profile".to_vec()), &limits),
            Err(Error::UnsupportedColor("malformed embedded ICC profile"))
        ));
        assert!(matches!(
            IccTransform::new(
                &raw_profile(fixture_profile("synthetic-icc-unsupported.heic")),
                &limits
            ),
            Err(Error::UnsupportedColor("non-RGB embedded ICC profile"))
        ));
        let tight = Limits {
            max_color_profile_bytes: 8,
            ..Limits::default()
        };
        assert!(matches!(
            IccTransform::new(&raw_profile(fixture_profile("synthetic-icc.heic")), &tight),
            Err(Error::UnsupportedColor("embedded ICC profile size"))
        ));
    }

    #[test]
    fn premultiplied_channels_are_recovered_before_a_conversion() {
        let mut pixels = [64, 32, 0, 128, 255, 255, 255, 0];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [128, 64, 0, 128, 0, 0, 0, 0]);
        composite_over_black(&mut pixels, false);
        assert_eq!(pixels, [64, 32, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn packing_converts_premultiplied_colors_before_compositing() {
        // Drives a partially transparent pixel through the real ICC transform,
        // which is the only way to catch a reordering of the conversion and the
        // compositing step: a nonlinear transform does not commute with
        // premultiplication, so converting first and compositing second gives a
        // different answer than the reverse.
        let profile = fixture_profile("synthetic-icc.heic");
        let transform = IccTransform::new(&raw_profile(profile), &Limits::default()).unwrap();
        let limits = Limits::default();

        // Half-transparent mid gray, already premultiplied by the decoder.
        let premultiplied = [64, 64, 64, 128];
        let packed = pack_rgba(
            &premultiplied,
            4,
            1,
            1,
            &limits,
            true,
            Some(transform.transform.as_ref()),
        )
        .unwrap();

        // Straightening gives 128, the conversion encodes linear 128/255 to
        // 188, and compositing over black at half alpha gives 94.
        assert_eq!(packed.pixels, [94, 94, 94, 255]);

        // A straight pixel at full alpha only exercises the conversion.
        let straight = [128, 128, 128, 255];
        let packed = pack_rgba(
            &straight,
            4,
            1,
            1,
            &limits,
            false,
            Some(transform.transform.as_ref()),
        )
        .unwrap();
        assert_eq!(packed.pixels, [188, 188, 188, 255]);
    }

    #[test]
    fn rejects_malformed_container_and_tight_limits() {
        let path = std::env::temp_dir().join(format!("genkan-invalid-heic-{}", std::process::id()));
        std::fs::write(&path, b"not an image").unwrap();
        assert!(matches!(Document::open(&path), Err(Error::Container(_))));
        std::fs::remove_file(path).unwrap();

        let limits = Limits {
            max_top_level_images: 1,
            ..Limits::default()
        };
        assert!(
            Document::open_with_limits(&fixture("synthetic-all-properties.heic"), limits).is_err()
        );
    }

    #[test]
    fn xmp_is_namespace_aware_and_supports_elements() {
        let plist = binary_plist(&Value::Dictionary(Dictionary::new()));
        let encoded = base64::engine::general_purpose::STANDARD.encode(&plist);
        let xml = format!(
            r#"<x:xmpmeta xmlns:x="x" xmlns:a="http://ns.apple.com/namespace/1.0/" xmlns:no="no"><a:h24>{encoded}</a:h24><x:item no:solar="{encoded}" a:apr="{encoded}"/></x:xmpmeta>"#
        );
        let properties = parse_xmp(xml.as_bytes(), &Limits::default()).unwrap();
        assert_eq!(
            properties.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![PropertyName::H24, PropertyName::Apr]
        );

        let escaped = format!("&#10;{encoded}");
        let xml =
            format!(r#"<a:h24 xmlns:a="http://ns.apple.com/namespace/1.0/">{escaped}</a:h24>"#);
        assert_eq!(
            parse_xmp(xml.as_bytes(), &Limits::default()).unwrap()[0].1,
            plist
        );
    }

    #[test]
    fn xmp_rejects_doctype_nested_markup_and_limits() {
        assert!(parse_xmp(b"<!DOCTYPE x><x/>", &Limits::default()).is_err());
        assert!(parse_xmp(
            br#"<a:h24 xmlns:a="http://ns.apple.com/namespace/1.0/"><x/></a:h24>"#,
            &Limits::default()
        )
        .is_err());
        assert!(parse_xmp(
            br#"<a:h24 xmlns:a="http://ns.apple.com/namespace/1.0/"><!-- split --></a:h24>"#,
            &Limits::default()
        )
        .is_err());
        let limits = Limits {
            max_xml_depth: 1,
            ..Limits::default()
        };
        assert!(parse_xmp(b"<x><y/></x>", &limits).is_err());
    }

    #[test]
    fn plist_preflight_rejects_object_depth_and_expansion_limits() {
        let value = Value::Array(vec![Value::Array(vec![Value::String("x".into())])]);
        let bytes = binary_plist(&value);
        let mut limits = Limits {
            max_plist_depth: 2,
            ..Limits::default()
        };
        assert!(matches!(
            preflight_binary_plist(&bytes, &limits),
            Err(Error::Limit("property list nesting depth"))
        ));
        limits.max_plist_depth = 32;
        limits.max_expanded_plist_objects = 2;
        assert!(matches!(
            preflight_binary_plist(&bytes, &limits),
            Err(Error::Limit("expanded property list object count"))
        ));

        limits.max_expanded_plist_objects = 4;
        assert!(matches!(
            preflight_binary_plist(&repeated_reference_plist(), &limits),
            Err(Error::Limit("expanded property list object count"))
        ));

        let limits = Limits {
            max_expanded_plist_bytes: 400,
            ..Limits::default()
        };
        assert!(matches!(
            preflight_binary_plist(&repeated_data_plist(100, 4), &limits),
            Err(Error::Limit("expanded property list memory"))
        ));
    }

    #[test]
    fn metadata_rejects_fractional_or_missing_indices_and_invalid_domains() {
        let images = TopLevelImages::new(vec![HeifItemId::new(1)]);
        for value in [
            h24_value(Value::Real(0.5), Value::Real(0.0)),
            h24_value(Value::Integer(1u64.into()), Value::Real(0.0)),
            h24_value(Value::Integer(0u64.into()), Value::Real(1.0)),
            h24_value(Value::Integer(0u64.into()), Value::Real(f64::NAN)),
        ] {
            assert!(parse_property(
                PropertyName::H24,
                &binary_plist(&value),
                &images,
                &Limits::default(),
            )
            .is_err());
        }
    }

    #[test]
    fn metadata_enforces_schedule_and_binary_plist_limits() {
        let images = TopLevelImages::new(vec![HeifItemId::new(1)]);
        let value = h24_value(Value::Integer(0u64.into()), Value::Real(0.0));
        let bytes = binary_plist(&value);
        let limits = Limits {
            max_schedule_points: 0,
            ..Limits::default()
        };
        assert!(matches!(
            parse_property(PropertyName::H24, &bytes, &images, &limits),
            Err(Error::Metadata(_))
        ));
        assert!(matches!(
            parse_property(
                PropertyName::H24,
                b"<?xml version='1.0'?><dict/>",
                &images,
                &limits
            ),
            Err(Error::Metadata(_))
        ));
    }

    #[test]
    fn packing_rejects_bad_stride_truncation_and_output_limit() {
        let output_bytes = Limits {
            max_output_bytes: 4,
            ..Limits::default()
        };
        assert!(pack_rgba(&[0; 8], 3, 1, 1, &output_bytes, false, None).is_err());
        assert!(pack_rgba(&[0; 3], 4, 1, 1, &output_bytes, false, None).is_err());
        let too_small = Limits {
            max_output_bytes: 3,
            ..Limits::default()
        };
        assert!(matches!(
            pack_rgba(&[0; 4], 4, 1, 1, &too_small, false, None),
            Err(Error::Limit("decoded image memory"))
        ));
    }

    #[test]
    fn packing_makes_straight_and_premultiplied_pixels_opaque() {
        let limits = Limits::default();
        assert_eq!(
            pack_rgba(&[64, 32, 0, 128], 4, 1, 1, &limits, true, None)
                .unwrap()
                .pixels,
            [64, 32, 0, 255]
        );
        assert_eq!(
            pack_rgba(&[64, 32, 0, 128], 4, 1, 1, &limits, false, None)
                .unwrap()
                .pixels,
            [32, 16, 0, 255]
        );
        assert_eq!(
            pack_rgba(
                &[255, 100, 1, 0, 7, 8, 9, 255],
                8,
                2,
                1,
                &limits,
                false,
                None
            )
            .unwrap()
            .pixels,
            [0, 0, 0, 255, 7, 8, 9, 255]
        );
    }

    #[test]
    fn conflicting_metadata_invalidates_only_that_property() {
        let appearance = Appearance {
            light: ImageReference::from_position(0),
            dark: ImageReference::from_position(1),
        };
        let mut metadata = Metadata::default();
        insert_metadata_property(
            &mut metadata,
            AppleProperty::Appearance,
            PropertyValue::Appearance(appearance),
        )
        .unwrap();
        insert_metadata_property(
            &mut metadata,
            AppleProperty::Appearance,
            PropertyValue::Appearance(Appearance {
                light: ImageReference::from_position(1),
                dark: ImageReference::from_position(0),
            }),
        )
        .unwrap();
        insert_metadata_property(
            &mut metadata,
            AppleProperty::Appearance,
            PropertyValue::Appearance(appearance),
        )
        .unwrap();

        assert!(metadata.appearance().is_none());
    }

    #[test]
    fn tile_based_wallpapers_are_not_limited_by_top_level_image_count() {
        // A tiled 4K image contributes one `ipma` entry and one `iloc` record
        // per tile, so a real multi-image wallpaper declares hundreds of
        // container items while exposing only a handful of images. The two
        // ceilings must therefore be independent.
        let limits = Limits::default();
        assert!(limits.max_container_items >= 512);
        assert!(u32::try_from(limits.max_top_level_images).unwrap() < limits.max_container_items);
        assert!(u32::try_from(limits.max_metadata_blocks).unwrap() < limits.max_container_items);

        // A synthetic fixture with four images still opens and decodes under
        // the default ceilings.
        let document = Document::open(&fixture("synthetic-all-properties.heic")).unwrap();
        assert_eq!(document.item_ids().count(), 4);
        assert_color(
            &document.decode(ImageReference::from_position(0)).unwrap(),
            [255, 0, 0, 255],
        );
    }

    #[test]
    fn accepts_xml_property_lists_like_real_dynamic_wallpapers() {
        // Apple's own dynamic wallpapers and the `wallpapper` and `Equinox`
        // generators all emit XML property lists, so the binary-only path this
        // replaces could not read any real file.
        let xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "#,
            r#""http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
            r#"<plist version="1.0"><dict>"#,
            r#"<key>ti</key><array>"#,
            r#"<dict><key>i</key><integer>0</integer><key>t</key><real>0.0</real></dict>"#,
            r#"<dict><key>i</key><integer>1</integer><key>t</key><real>0.5</real></dict>"#,
            r#"</array></dict></plist>"#,
        );
        let images = TopLevelImages::new(vec![HeifItemId::new(1), HeifItemId::new(2)]);
        let (property, value) = parse_property(
            PropertyName::H24,
            xml.as_bytes(),
            &images,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(property, AppleProperty::Time);
        let PropertyValue::Time(schedule) = value else {
            panic!("expected a time schedule");
        };
        assert_eq!(schedule.points().len(), 2);
        assert_eq!(schedule.points()[1].image, ImageReference::from_position(1));

        // The same document with an appearance pair, as the time-of-day
        // wallpapers embed it.
        let xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<plist version="1.0"><dict>"#,
            r#"<key>ap</key><dict><key>d</key><integer>1</integer>"#,
            r#"<key>l</key><integer>0</integer></dict>"#,
            r#"<key>ti</key><array>"#,
            r#"<dict><key>i</key><integer>0</integer><key>t</key><integer>0</integer></dict>"#,
            r#"<dict><key>i</key><integer>1</integer><key>t</key><real>0.5</real></dict>"#,
            r#"</array></dict></plist>"#,
        );
        let (_, value) = parse_property(
            PropertyName::H24,
            xml.as_bytes(),
            &images,
            &Limits::default(),
        )
        .unwrap();
        let PropertyValue::Time(schedule) = value else {
            panic!("expected a time schedule");
        };
        let appearance = schedule.appearance.expect("embedded appearance");
        assert_eq!(appearance.light, ImageReference::from_position(0));
        assert_eq!(appearance.dark, ImageReference::from_position(1));

        // An XML solar schedule, as the solar dynamic wallpapers encode it.
        let xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<plist version="1.0"><dict><key>si</key><array>"#,
            r#"<dict><key>i</key><integer>0</integer><key>a</key><real>-8.0</real>"#,
            r#"<key>z</key><real>164.8</real></dict>"#,
            r#"<dict><key>i</key><integer>1</integer><key>a</key><real>2.8</real>"#,
            r#"<key>z</key><real>75.2</real></dict>"#,
            r#"</array></dict></plist>"#,
        );
        let (property, value) = parse_property(
            PropertyName::Solar,
            xml.as_bytes(),
            &images,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(property, AppleProperty::Solar);
        let PropertyValue::Solar(schedule) = value else {
            panic!("expected a solar schedule");
        };
        assert_eq!(schedule.points().len(), 2);
    }

    #[test]
    fn xml_property_lists_are_bounded_before_expansion() {
        let images = TopLevelImages::new(vec![HeifItemId::new(1)]);
        // A document that is not a property list in either encoding.
        assert!(matches!(
            parse_property(
                PropertyName::H24,
                b"not a plist",
                &images,
                &Limits::default()
            ),
            Err(Error::Metadata(_))
        ));
        // An XML document that exceeds the encoded byte ceiling.
        let oversized = format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>ti</key><string>{}</string></dict></plist>"#,
            "x".repeat(Limits::default().max_plist_bytes)
        );
        assert!(matches!(
            parse_property(
                PropertyName::H24,
                oversized.as_bytes(),
                &images,
                &Limits::default()
            ),
            Err(Error::Limit("property list size"))
        ));
        // Deep nesting is rejected before `plist` expands it.
        let deep = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\">{}{}",
            "<array>".repeat(64),
            "</array>".repeat(64)
        );
        assert!(matches!(
            parse_property(
                PropertyName::H24,
                deep.as_bytes(),
                &images,
                &Limits::default()
            ),
            Err(Error::Limit("property list nesting depth"))
        ));
        // Object-count expansion is bounded.
        let limits = Limits {
            max_expanded_plist_objects: 2,
            ..Limits::default()
        };
        let wide = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><array>{}</array></plist>",
            "<string>x</string>".repeat(16)
        );
        assert!(matches!(
            parse_property(PropertyName::H24, wide.as_bytes(), &images, &limits),
            Err(Error::Limit("expanded property list object count"))
        ));
    }

    #[test]
    fn color_policy_accepts_only_convertible_sdr_nclx_profiles() {
        use libheif_rs::{ColorPrimaries, TransferCharacteristics};

        let mut profile = ColorProfileNCLX::new().unwrap();
        profile.set_color_primaries(ColorPrimaries::ITU_R_BT_709_5);
        assert!(validate_nclx(&profile).is_ok());

        assert!(matches!(
            validate_nclx_encoding(
                ColorPrimaries::ITU_R_BT_2020_2_and_2100_0,
                TransferCharacteristics::IEC_61966_2_1,
            ),
            Err(Error::UnsupportedColor(_))
        ));
        assert!(matches!(
            validate_nclx_encoding(
                ColorPrimaries::ITU_R_BT_709_5,
                TransferCharacteristics::ITU_R_BT_709_5,
            ),
            Err(Error::UnsupportedColor(_))
        ));
    }
}
