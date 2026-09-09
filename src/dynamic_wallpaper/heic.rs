//! Bounded parser and decoder for macOS dynamic HEIC wallpapers.

use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

use base64::Engine as _;
use libheif_rs::{
    ColorProfileNCLX, ColorSpace, DecodingOptions, HeifContext, LibHeif, RgbChroma, StreamReader,
};
use plist::{Dictionary, Value};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};
use thiserror::Error;

use super::{
    Appearance, AppleProperty, HeifItemId, ImageReference, Metadata, ModelError, NormalizedTime,
    PropertyValue, Schedule, SolarPoint, SolarPosition, TimePoint, TopLevelImages,
};

const APPLE_DESKTOP_NAMESPACE: &str = "http://ns.apple.com/namespace/1.0/";

/// Resource ceilings applied before metadata or pixel allocations.
#[derive(Clone, Copy, Debug)]
struct Limits {
    max_source_bytes: u64,
    max_items: usize,
    max_tiles: u32,
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
    max_metadata_block_bytes: usize,
    max_metadata_bytes: usize,
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
            // This is a total container-item ceiling, including hidden tiles and metadata.
            // Keeping it coupled to the parse-phase block limit below bounds libheif's
            // eagerly retained metadata to 64 MiB.
            max_items: 64,
            max_tiles: 4_096,
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 32 * 1024 * 1024,
            max_metadata_block_bytes: 1024 * 1024,
            max_metadata_bytes: 4 * 1024 * 1024,
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

/// An opened HEIC whose source descriptor remains stable for its lifetime.
///
/// Replacing the pathname does not change the source. As with any open file,
/// however, an owner with write access can still modify the bound inode.
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

        let _ = libheif();
        let mut context = HeifContext::new().map_err(container_error)?;
        let mut security_limits = libheif_rs::SecurityLimits::default();
        security_limits.set_max_image_size_pixels(limits.max_pixels);
        security_limits.set_max_number_of_tiles(u64::from(limits.max_tiles));
        security_limits
            .set_max_bayer_pattern_pixels(u32::try_from(limits.max_pixels).unwrap_or(u32::MAX));
        security_limits.set_max_items(u32::try_from(limits.max_items).unwrap_or(u32::MAX));
        security_limits.set_max_color_profile_size(
            u32::try_from(limits.max_metadata_block_bytes).unwrap_or(u32::MAX),
        );
        // libheif eagerly loads metadata while parsing. The pinned Nix build has no
        // compressed-metadata codecs, so this per-item ceiling and max_items bound
        // retained native metadata before Rust can inspect aggregate sizes.
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
            .read_reader(Box::new(StreamReader::new(file, source_bytes)))
            .map_err(container_error)?;

        let item_ids = context.image_ids();
        if item_ids.is_empty() {
            return Err(Error::Container("no top-level images".into()));
        }
        if item_ids.len() > limits.max_items {
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
        validate_color(&handle)?;

        let mut options = DecodingOptions::new()
            .ok_or_else(|| Error::Decode("could not allocate decoding options".into()))?;
        options.set_strict_decoding(true);
        options.set_convert_hdr_to_8bit(false);
        options.set_output_image_nclx_profile_passthrough(true);
        let image = libheif()
            .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), Some(options))
            .map_err(decode_error)?;
        if image.color_profile_raw().is_some() {
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
            self.limits.max_output_bytes,
            premultiplied_alpha,
        )
    }
}

fn libheif() -> &'static LibHeif {
    static LIBHEIF: OnceLock<LibHeif> = OnceLock::new();
    LIBHEIF.get_or_init(LibHeif::new)
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

fn validate_color(handle: &libheif_rs::ImageHandle) -> Result<(), Error> {
    if handle.luma_bits_per_pixel() > 8 || handle.chroma_bits_per_pixel() > 8 {
        return Err(Error::UnsupportedColor("HDR bit depth"));
    }
    if handle.color_profile_raw().is_some() {
        return Err(Error::UnsupportedColor("embedded ICC profile"));
    }
    if let Some(profile) = handle.color_profile_nclx() {
        validate_nclx(&profile)?;
    }
    Ok(())
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

fn pack_rgba(
    source: &[u8],
    stride: usize,
    width: u32,
    height: u32,
    max_output_bytes: usize,
    premultiplied_alpha: bool,
) -> Result<RgbaFrame, Error> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(Error::Limit("decoded image memory"))?;
    let output_bytes = row_bytes
        .checked_mul(usize::try_from(height).map_err(|_| Error::Limit("decoded image memory"))?)
        .ok_or(Error::Limit("decoded image memory"))?;
    if output_bytes > max_output_bytes {
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
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if !premultiplied_alpha {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        pixel[3] = 255;
    }
    Ok(RgbaFrame {
        width,
        height,
        pixels,
    })
}

fn read_metadata(
    handle: &libheif_rs::ImageHandle,
    images: &TopLevelImages,
    limits: &Limits,
) -> Result<Metadata, Error> {
    let count = handle.number_of_metadata_blocks(0).max(0) as usize;
    if count > limits.max_items {
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
                        properties.push((name, decode_plist_base64(&value, limits)?));
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
                        properties.push((name, decode_plist_base64(&value, limits)?));
                    }
                }
                let (namespace, local) = reader.resolver().resolve_element(empty.name());
                if let Some(name) = property_name(namespace, local.as_ref()) {
                    properties.push((name, decode_plist_base64("", limits)?));
                }
            }
            Event::Text(text) => {
                if let Some((_, _, value)) = active.as_mut() {
                    value.push_str(&text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(data) => {
                if let Some((_, _, value)) = active.as_mut() {
                    value.push_str(&data.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::End(_) => {
                if let Some((_, property_depth, _)) = active.as_ref() {
                    if *property_depth == depth {
                        let (name, _, value) = active.take().unwrap();
                        properties.push((name, decode_plist_base64(&value, limits)?));
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
                active.as_mut().unwrap().2.push(character);
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
    let compact: String = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
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
    preflight_binary_plist(bytes, limits)?;
    let value = Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| Error::Metadata(format!("invalid binary property list: {error}")))?;
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

    #[test]
    fn rejects_malformed_container_and_tight_limits() {
        let path = std::env::temp_dir().join(format!("genkan-invalid-heic-{}", std::process::id()));
        std::fs::write(&path, b"not an image").unwrap();
        assert!(matches!(Document::open(&path), Err(Error::Container(_))));
        std::fs::remove_file(path).unwrap();

        let limits = Limits {
            max_items: 1,
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
        assert!(pack_rgba(&[0; 8], 3, 1, 1, 4, false).is_err());
        assert!(pack_rgba(&[0; 3], 4, 1, 1, 4, false).is_err());
        assert!(matches!(
            pack_rgba(&[0; 4], 4, 1, 1, 3, false),
            Err(Error::Limit("decoded image memory"))
        ));
    }

    #[test]
    fn packing_makes_straight_and_premultiplied_pixels_opaque() {
        assert_eq!(
            pack_rgba(&[64, 32, 0, 128], 4, 1, 1, 4, true)
                .unwrap()
                .pixels,
            [64, 32, 0, 255]
        );
        assert_eq!(
            pack_rgba(&[64, 32, 0, 128], 4, 1, 1, 4, false)
                .unwrap()
                .pixels,
            [32, 16, 0, 255]
        );
        assert_eq!(
            pack_rgba(&[255, 100, 1, 0, 7, 8, 9, 255], 8, 2, 1, 8, false)
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
