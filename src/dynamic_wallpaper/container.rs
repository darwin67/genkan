//! Structural preflight for untrusted HEIF containers.
//!
//! libheif applies its `max_items` security limit to each box on its own, so a
//! container assembled from many individually legal boxes can still exceed
//! every per-box ceiling. `ipma` associations are merged across boxes without
//! an aggregate check, `iinf` uses the same value as a per-box child count, and
//! `iloc` records and `iref` references are each counted per box. libheif also
//! copies every non-image item's extent into its own allocation while parsing,
//! before Rust can inspect the result.
//!
//! This walk therefore runs before libheif sees the file and enforces ceilings
//! summed over the whole container. It is deliberately strict: anything it
//! cannot parse is rejected, so a container that reaches libheif has already
//! satisfied the aggregate bounds.
//!
//! Two properties keep the walk in step with the pinned parser:
//!
//! * Boxes are dispatched by type wherever they appear, not by the scope they
//!   sit in, because libheif's `Box::read` dispatches by four-character code
//!   independently of the parent. A box the walk does not recognise is left
//!   alone, so `is_container` must list every type whose parse reads children.
//! * Item classification mirrors `item_type_is_image` in `libheif/context.cc`,
//!   and the string reader mirrors `BitstreamRange::read_string`, because a
//!   classification difference would move an item out of the eager-load budget.

use super::heic::Error;

/// Aggregate ceilings enforced before libheif parses the container.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// Items, `iloc` records, `iref` references, and `ipma` entries summed
    /// across every box of each kind.
    pub(crate) max_container_items: u32,
    /// Property associations summed across every `ipma` box.
    pub(crate) max_container_associations: u32,
    /// Non-image items, which libheif loads eagerly while parsing.
    pub(crate) max_metadata_items: u32,
    /// Declared extent bytes of those items, counted once per item that
    /// references them.
    pub(crate) max_metadata_bytes: u64,
    /// Boxes visited by the walk, which bounds the native box objects the
    /// container can make libheif allocate.
    pub(crate) max_container_boxes: u32,
    /// Nesting depth of container boxes.
    pub(crate) max_container_depth: u32,
    /// Extents a single `iloc` record may declare, matching libheif's own
    /// `max_iloc_extents_per_item`.
    pub(crate) max_item_extents: u32,
}

/// Walks the container and enforces `budget` over the whole file.
pub(crate) fn preflight(bytes: &[u8], budget: &Budget) -> Result<(), Error> {
    Walk {
        bytes,
        budget,
        totals: Totals::default(),
        boxes: 0,
        metadata_items: Vec::new(),
        extents: Vec::new(),
        saw_meta: false,
        saw_mini: false,
        sequence_brand: false,
    }
    .run()
}

/// Counts accumulated across every relevant box.
#[derive(Default)]
struct Totals {
    items: u32,
    iloc_records: u32,
    iref_references: u32,
    ipma_entries: u32,
    ipma_associations: u32,
}

/// One `iloc` record's declared extent bytes.
struct Extent {
    id: u32,
    bytes: u64,
}

/// A parsed box header.
struct Header {
    kind: [u8; 4],
    body_start: usize,
    body_end: usize,
    next: usize,
}

struct Walk<'a> {
    bytes: &'a [u8],
    budget: &'a Budget,
    totals: Totals,
    boxes: u32,
    metadata_items: Vec<u32>,
    extents: Vec<Extent>,
    saw_meta: bool,
    saw_mini: bool,
    sequence_brand: bool,
}

impl<'a> Walk<'a> {
    fn run(mut self) -> Result<(), Error> {
        let end = self.bytes.len();
        self.walk(0, end, 0)?;
        // libheif accepts a minimised `mini` file and a sequence-brand file with
        // a movie box and no `meta` box, so the walk must not require metadata
        // where the parser does not.
        if !self.saw_meta && !self.saw_mini && !self.sequence_brand {
            return Err(Error::Container("no metadata box".into()));
        }
        self.check_metadata_bytes()
    }

    /// Sums the declared extents of every eagerly loaded item. libheif reads
    /// each item's data into its own allocation, so an extent referenced by
    /// several items is charged once per item rather than once per extent.
    ///
    /// An item can be declared by more than one `iloc` box, and libheif only
    /// reads the record belonging to the metadata box it selects. Charging the
    /// largest record rather than the first one keeps a decoy box from
    /// declaring a zero-length location that hides the real extent, without
    /// charging a conforming file twice for the same extent.
    fn check_metadata_bytes(&self) -> Result<(), Error> {
        let mut total = 0u64;
        for id in &self.metadata_items {
            let mut largest = 0u64;
            for extent in &self.extents {
                if extent.id == *id {
                    largest = largest.max(extent.bytes);
                }
            }
            total = total
                .checked_add(largest)
                .ok_or(Error::Limit("metadata extent bytes"))?;
        }
        if total > self.budget.max_metadata_bytes {
            return Err(Error::Limit("metadata extent bytes"));
        }
        Ok(())
    }

    fn walk(&mut self, start: usize, end: usize, depth: u32) -> Result<(), Error> {
        if start > end {
            return Err(Error::Container("box is truncated".into()));
        }
        let mut offset = start;
        while offset < end {
            let header = self.read_header(offset, end)?;
            self.visit(&header, depth)?;
            offset = header.next;
        }
        Ok(())
    }

    /// Dispatches one box by type and counts it.
    ///
    /// The depth and box ceilings are checked here rather than in `walk`,
    /// because the counted parsers descend through this function directly and
    /// would otherwise let a chain of one-child `iinf` or `stsd` boxes recurse
    /// without ever re-entering `walk`.
    fn visit(&mut self, header: &Header, depth: u32) -> Result<(), Error> {
        if depth > self.budget.max_container_depth {
            return Err(Error::Limit("box nesting depth"));
        }
        self.boxes = self.boxes.checked_add(1).ok_or(Error::Limit("box count"))?;
        if self.boxes > self.budget.max_container_boxes {
            return Err(Error::Limit("box count"));
        }
        match &header.kind {
            b"meta" => {
                self.saw_meta = true;
                self.walk(header.body_start + 4, header.body_end, depth + 1)
            }
            b"iinf" => self.parse_iinf(header, depth),
            b"infe" => self.parse_infe(header),
            b"iloc" => self.parse_iloc(header),
            b"iref" => self.parse_iref(header),
            b"ipma" => self.parse_ipma(header),
            b"stsd" => self.parse_stsd(header, depth),
            // A data reference box carries a full-box header and an entry count
            // before its children.
            b"dref" => self.walk(header.body_start + 8, header.body_end, depth + 1),
            // A URI meta sample entry carries six reserved bytes and a data
            // reference index before its children.
            b"urim" => self.walk(header.body_start + 8, header.body_end, depth + 1),
            b"tref" => self.parse_tref(header),
            b"ftyp" => self.parse_ftyp(header),
            b"mini" => {
                self.saw_mini = true;
                Ok(())
            }
            kind if is_container(kind) => self.walk(header.body_start, header.body_end, depth + 1),
            // A visual sample entry carries a fixed prefix before its children.
            kind if is_visual_sample_entry(kind) => self.walk_visual_sample_entry(header, depth),
            _ => Ok(()),
        }
    }

    /// Walks the children of a visual sample entry.
    ///
    /// The prefix ends with a length-prefixed compressor name. When that length
    /// does not fit the field, the pinned parser returns after the length byte
    /// without consuming the rest of the name, so it reads children from a
    /// different offset than a well-formed entry places them at. Reject such an
    /// entry rather than walking a layout the parser will not follow.
    fn walk_visual_sample_entry(&mut self, header: &Header, depth: u32) -> Result<(), Error> {
        let prefix = self.read(
            header.body_start,
            VISUAL_SAMPLE_ENTRY_PREFIX,
            header.body_end,
        )?;
        if prefix[VISUAL_SAMPLE_ENTRY_NAME_LENGTH] > 31 {
            return Err(Error::Container(
                "sample entry compressor name is too long".into(),
            ));
        }
        self.walk(
            header.body_start + VISUAL_SAMPLE_ENTRY_PREFIX,
            header.body_end,
            depth + 1,
        )
    }

    /// Reads the file type box so a sequence-brand file is not mistaken for a
    /// file with no metadata. The brands are tested in place rather than
    /// collected, because a source-sized box would otherwise allocate a slice
    /// descriptor for every four bytes of its payload.
    fn parse_ftyp(&mut self, header: &Header) -> Result<(), Error> {
        let body = self.read(
            header.body_start,
            header.body_end - header.body_start,
            header.body_end,
        )?;
        if body.len() < 8 {
            return Err(Error::Container("file type box is truncated".into()));
        }
        self.sequence_brand =
            is_sequence_brand(&body[0..4]) || body[8..].chunks_exact(4).any(is_sequence_brand);
        Ok(())
    }

    /// Reads an item information box. libheif reads at most `entry_count`
    /// children and then stops, so this walk requires the declaration to match
    /// the children actually present rather than ignoring a remainder. Each
    /// child is dispatched by type, because a nested container inside `iinf` is
    /// parsed the same way as one outside it.
    fn parse_iinf(&mut self, header: &Header, depth: u32) -> Result<(), Error> {
        let version = self.read_u8(header.body_start, header.body_end)?;
        let mut offset = header.body_start + 4;
        let declared = if version == 0 {
            let count = self.read_u16(offset, header.body_end)?;
            offset += 2;
            u32::from(count)
        } else {
            let count = self.read_u32(offset, header.body_end)?;
            offset += 4;
            count
        };
        let mut seen = 0u32;
        while offset < header.body_end {
            let child = self.read_header(offset, header.body_end)?;
            self.visit(&child, depth + 1)?;
            seen = seen
                .checked_add(1)
                .ok_or(Error::Limit("container item count"))?;
            offset = child.next;
        }
        if seen != declared {
            return Err(Error::Container(
                "item information count does not match its entries".into(),
            ));
        }
        Ok(())
    }

    /// Reads a sample description box. libheif dispatches each declared entry
    /// by type and sample entries read children of their own.
    fn parse_stsd(&mut self, header: &Header, depth: u32) -> Result<(), Error> {
        let version = self.read_u8(header.body_start, header.body_end)?;
        if version != 0 {
            return Err(Error::Container(
                "unsupported sample description version".into(),
            ));
        }
        let mut offset = header.body_start + 4;
        let count = self.read_u32(offset, header.body_end)?;
        offset += 4;
        for _ in 0..count {
            if offset >= header.body_end {
                return Err(Error::Container("sample description is truncated".into()));
            }
            let child = self.read_header(offset, header.body_end)?;
            self.visit(&child, depth + 1)?;
            offset = child.next;
        }
        Ok(())
    }

    /// Reads one item information entry and records whether libheif will load
    /// its data eagerly.
    fn parse_infe(&mut self, header: &Header) -> Result<(), Error> {
        let version = self.read_u8(header.body_start, header.body_end)?;
        if version > 3 {
            return Err(Error::Container("unsupported item info version".into()));
        }
        let mut offset = header.body_start + 4;
        let (id, item_type, content_type) = if version <= 1 {
            let id = u32::from(self.read_u16(offset, header.body_end)?);
            offset += 2;
            offset += 2; // protection index
            self.read_cstring(&mut offset, header.body_end)?;
            let content_type = self.read_cstring(&mut offset, header.body_end)?;
            self.read_cstring(&mut offset, header.body_end)?;
            (id, [0u8; 4], content_type.to_vec())
        } else {
            let id = if version == 2 {
                u32::from(self.read_u16(offset, header.body_end)?)
            } else {
                self.read_u32(offset, header.body_end)?
            };
            offset += if version == 2 { 2 } else { 4 };
            offset += 2; // protection index
            let item_type = self.read_fourcc(offset, header.body_end)?;
            offset += 4;
            self.read_cstring(&mut offset, header.body_end)?;
            let content_type = if &item_type == b"mime" {
                self.read_cstring(&mut offset, header.body_end)?.to_vec()
            } else {
                Vec::new()
            };
            if &item_type == b"uri " {
                self.read_cstring(&mut offset, header.body_end)?;
            }
            (id, item_type, content_type)
        };

        self.totals.items = self
            .totals
            .items
            .checked_add(1)
            .ok_or(Error::Limit("container item count"))?;
        if self.totals.items > self.budget.max_container_items {
            return Err(Error::Limit("container item count"));
        }

        // libheif loads every non-image item in its ordinary metadata pass and
        // then loads every `mime` item again in its text pass, so a MIME item is
        // eagerly read even when its content type makes it an image.
        if is_image_item(&item_type, &content_type) && &item_type != b"mime" {
            return Ok(());
        }
        // libheif keys items by identifier and keeps the first declaration, so a
        // repeated one does not add another eager load.
        if self.metadata_items.contains(&id) {
            return Ok(());
        }
        if self.metadata_items.len() as u32 >= self.budget.max_metadata_items {
            return Err(Error::Limit("metadata item count"));
        }
        self.metadata_items.push(id);
        Ok(())
    }

    /// Reads an item location box, counting records and retaining each item's
    /// declared extent bytes for the eager-load budget.
    fn parse_iloc(&mut self, header: &Header) -> Result<(), Error> {
        let version = self.read_u8(header.body_start, header.body_end)?;
        if version > 2 {
            return Err(Error::Container("unsupported item location version".into()));
        }
        let mut offset = header.body_start + 4;
        let widths = self.read_u16(offset, header.body_end)?;
        offset += 2;
        let offset_size = usize::from((widths >> 12) & 0xF);
        let length_size = usize::from((widths >> 8) & 0xF);
        let base_offset_size = usize::from((widths >> 4) & 0xF);
        let index_size = if version >= 1 {
            usize::from(widths & 0xF)
        } else {
            0
        };
        // libheif only reads these fields at four or eight bytes and silently
        // leaves the others zero, which desynchronises the record walk. Refuse
        // the ambiguity instead of guessing.
        for width in [offset_size, length_size, base_offset_size, index_size] {
            if !matches!(width, 0 | 4 | 8) {
                return Err(Error::Container(
                    "unsupported item location field width".into(),
                ));
            }
        }
        let item_count = if version < 2 {
            let count = u32::from(self.read_u16(offset, header.body_end)?);
            offset += 2;
            count
        } else {
            let count = self.read_u32(offset, header.body_end)?;
            offset += 4;
            count
        };
        self.totals.iloc_records = self
            .totals
            .iloc_records
            .checked_add(item_count)
            .ok_or(Error::Limit("item location record count"))?;
        if self.totals.iloc_records > self.budget.max_container_items {
            return Err(Error::Limit("item location record count"));
        }
        for _ in 0..item_count {
            let id = if version < 2 {
                let id = u32::from(self.read_u16(offset, header.body_end)?);
                offset += 2;
                id
            } else {
                let id = self.read_u32(offset, header.body_end)?;
                offset += 4;
                id
            };
            if version >= 1 {
                offset += 2; // construction method
            }
            offset += 2; // data reference index
            offset = self.advance(offset, base_offset_size, header.body_end)?;
            let extent_count = u32::from(self.read_u16(offset, header.body_end)?);
            offset += 2;
            // A record whose field widths are all zero consumes no bytes per
            // extent, so an unbounded count would let a tiny box drive a very
            // large loop. libheif rejects more extents than this anyway.
            if extent_count > self.budget.max_item_extents {
                return Err(Error::Limit("item extent count"));
            }
            let mut total = 0u64;
            for _ in 0..extent_count {
                offset = self.advance(offset, index_size, header.body_end)?;
                offset = self.advance(offset, offset_size, header.body_end)?;
                total = total
                    .checked_add(self.read_uint(offset, length_size, header.body_end)?)
                    .ok_or(Error::Limit("item extent bytes"))?;
                offset = self.advance(offset, length_size, header.body_end)?;
            }
            self.extents.push(Extent { id, bytes: total });
        }
        Ok(())
    }

    /// Reads a reference box, counting references summed across every child.
    ///
    /// libheif reads each child's fields directly from the enclosing stream
    /// rather than skipping to the end of the declared child size, so a child
    /// that declares a larger size than its fields need would be read again as
    /// further reference records. Require the fields to fill the child exactly,
    /// and refuse the zero-count lists libheif rejects.
    fn parse_iref(&mut self, header: &Header) -> Result<(), Error> {
        // Read the whole full-box header, so a box too short to hold one is
        // rejected instead of passing with an empty loop.
        let version = self.read(header.body_start, 4, header.body_end)?[0];
        if version > 1 {
            return Err(Error::Container("unsupported reference version".into()));
        }
        let id_size = if version == 0 { 2 } else { 4 };
        let mut offset = header.body_start + 4;
        while offset < header.body_end {
            let child = self.read_header(offset, header.body_end)?;
            let count = self.read_u16(child.body_start + id_size, child.body_end)?;
            if count == 0 {
                return Err(Error::Container("reference list is empty".into()));
            }
            self.totals.iref_references = self
                .totals
                .iref_references
                .checked_add(u32::from(count))
                .ok_or(Error::Limit("reference count"))?;
            if self.totals.iref_references > self.budget.max_container_items {
                return Err(Error::Limit("reference count"));
            }
            let expected = child
                .body_start
                .checked_add(id_size + 2 + usize::from(count) * id_size)
                .ok_or(Error::Limit("reference count"))?;
            if expected != child.body_end {
                return Err(Error::Container(
                    "reference list does not fill its box".into(),
                ));
            }
            offset = child.next;
        }
        Ok(())
    }

    /// Reads a track reference box.
    ///
    /// libheif reads each reference-list header directly from the enclosing
    /// stream and bounds only that list's destinations, so a container full of
    /// small lists would otherwise add references that no aggregate counter
    /// sees. Count the destinations of every list here as well.
    fn parse_tref(&mut self, header: &Header) -> Result<(), Error> {
        let mut offset = header.body_start;
        while offset < header.body_end {
            let declared = u32::from_be_bytes(
                self.read(offset, 4, header.body_end)?
                    .try_into()
                    .expect("four bytes"),
            );
            if declared == 0 {
                return Err(Error::Container(
                    "track reference list has an unspecified size".into(),
                ));
            }
            let child = self.read_header(offset, header.body_end)?;
            let length = child.body_end - child.body_start;
            if length < 4 || length % 4 != 0 {
                return Err(Error::Container(
                    "track reference list has an invalid size".into(),
                ));
            }
            let count = u32::try_from(length / 4).map_err(|_| Error::Limit("reference count"))?;
            self.totals.iref_references = self
                .totals
                .iref_references
                .checked_add(count)
                .ok_or(Error::Limit("reference count"))?;
            if self.totals.iref_references > self.budget.max_container_items {
                return Err(Error::Limit("reference count"));
            }
            offset = child.next;
        }
        Ok(())
    }

    /// Reads an item property association box. libheif merges every `ipma` box
    /// into the first without an aggregate check, so the walk counts entries
    /// and associations over all of them.
    fn parse_ipma(&mut self, header: &Header) -> Result<(), Error> {
        let version = self.read_u8(header.body_start, header.body_end)?;
        if version > 1 {
            return Err(Error::Container(
                "unsupported property association version".into(),
            ));
        }
        let wide = self.read_u8(header.body_start + 3, header.body_end)? & 1 != 0;
        let mut offset = header.body_start + 4;
        let entry_count = self.read_u32(offset, header.body_end)?;
        offset += 4;
        self.totals.ipma_entries = self
            .totals
            .ipma_entries
            .checked_add(entry_count)
            .ok_or(Error::Limit("property association count"))?;
        if self.totals.ipma_entries > self.budget.max_container_items {
            return Err(Error::Limit("property association count"));
        }
        for _ in 0..entry_count {
            offset = self.advance(offset, if version < 1 { 2 } else { 4 }, header.body_end)?;
            let associations = u32::from(self.read_u8(offset, header.body_end)?);
            offset += 1;
            self.totals.ipma_associations = self
                .totals
                .ipma_associations
                .checked_add(associations)
                .ok_or(Error::Limit("property association count"))?;
            if self.totals.ipma_associations > self.budget.max_container_associations {
                return Err(Error::Limit("property association count"));
            }
            let width = if wide { 2 } else { 1 };
            offset = self.advance(
                offset,
                usize::try_from(associations).unwrap_or(usize::MAX) * width,
                header.body_end,
            )?;
        }
        Ok(())
    }

    fn read_header(&self, offset: usize, limit: usize) -> Result<Header, Error> {
        let header = self.read(offset, 8, limit)?;
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = [header[4], header[5], header[6], header[7]];
        // The declared length is the whole box, header included, so the body is
        // derived from it rather than by adding a payload length to a header
        // length. That keeps the extended header of a `uuid` box from being
        // counted twice.
        let (box_length, header_length) = match size {
            // A zero size means the box runs to the end of its parent.
            0 => (limit - offset, 8),
            1 => {
                let start = offset
                    .checked_add(8)
                    .ok_or_else(|| Error::Container("box offset overflow".into()))?;
                let large = self.read(start, 8, limit)?;
                let large = u64::from_be_bytes(large.try_into().expect("eight bytes"));
                let large = usize::try_from(large)
                    .map_err(|_| Error::Container("box size is out of range".into()))?;
                (large, 16)
            }
            size => (
                usize::try_from(size)
                    .map_err(|_| Error::Container("box size is out of range".into()))?,
                8,
            ),
        };
        // A `uuid` box carries a sixteen-byte extended type between the header
        // and the body. libheif consumes it as part of the header, so reading
        // the body from the shorter header would misplace every later field.
        let header_length = if &kind == b"uuid" {
            header_length + 16
        } else {
            header_length
        };
        if box_length < header_length {
            return Err(Error::Container("box is smaller than its header".into()));
        }
        let body_start = offset
            .checked_add(header_length)
            .ok_or_else(|| Error::Container("box offset overflow".into()))?;
        let body_end = offset
            .checked_add(box_length)
            .ok_or_else(|| Error::Container("box offset overflow".into()))?;
        if body_end > limit {
            return Err(Error::Container("box extends past its parent".into()));
        }
        Ok(Header {
            kind,
            body_start,
            body_end,
            next: body_end,
        })
    }

    fn read(&self, offset: usize, length: usize, end: usize) -> Result<&'a [u8], Error> {
        let stop = offset
            .checked_add(length)
            .ok_or_else(|| Error::Container("box offset overflow".into()))?;
        if stop > end || stop > self.bytes.len() {
            return Err(Error::Container("box is truncated".into()));
        }
        Ok(&self.bytes[offset..stop])
    }

    fn advance(&self, offset: usize, length: usize, end: usize) -> Result<usize, Error> {
        let stop = offset
            .checked_add(length)
            .ok_or_else(|| Error::Container("box offset overflow".into()))?;
        if stop > end {
            return Err(Error::Container("box is truncated".into()));
        }
        Ok(stop)
    }

    fn read_u8(&self, offset: usize, end: usize) -> Result<u8, Error> {
        Ok(self.read(offset, 1, end)?[0])
    }

    fn read_u16(&self, offset: usize, end: usize) -> Result<u16, Error> {
        let bytes = self.read(offset, 2, end)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&self, offset: usize, end: usize) -> Result<u32, Error> {
        let bytes = self.read(offset, 4, end)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_uint(&self, offset: usize, width: usize, end: usize) -> Result<u64, Error> {
        match width {
            0 => Ok(0),
            4 => Ok(u64::from(self.read_u32(offset, end)?)),
            8 => {
                let high = self.read_u32(offset, end)?;
                let low = self.read_u32(offset + 4, end)?;
                Ok((u64::from(high) << 32) | u64::from(low))
            }
            _ => Err(Error::Container("unsupported integer width".into())),
        }
    }

    fn read_fourcc(&self, offset: usize, end: usize) -> Result<[u8; 4], Error> {
        let bytes = self.read(offset, 4, end)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Mirrors `BitstreamRange::read_string` in `libheif/bitstream.cc`.
    ///
    /// The reader stops at the terminating zero or at the end of the box, and
    /// does not append the byte that exhausts the box. A classification that
    /// differed by that byte would let an item be an image here and metadata to
    /// libheif, moving it out of the eager-load budget entirely.
    fn read_cstring(&self, offset: &mut usize, end: usize) -> Result<&'a [u8], Error> {
        if *offset > end || end > self.bytes.len() {
            return Err(Error::Container("box is truncated".into()));
        }
        let rest = &self.bytes[*offset..end];
        let (value, next) = match rest.iter().position(|byte| *byte == 0) {
            Some(length) => (&rest[..length], *offset + length + 1),
            None if !rest.is_empty() => (&rest[..rest.len() - 1], end),
            None => (rest, end),
        };
        *offset = next;
        Ok(value)
    }
}

/// Box types whose parse reads child boxes immediately after the box header.
///
/// Taken from the pinned libheif: the `Box_container` subclasses cover the movie
/// and track boxes, and the rest call `read_children` directly. Types whose
/// children start after a prefix are handled in `visit` instead, and `tilC` is
/// absent because the pinned build leaves experimental features disabled, which
/// makes it an opaque box there. A type missing from these lists would hide its
/// children from the aggregate ceilings, so they must be kept in step with the
/// pinned parser.
fn is_container(kind: &[u8; 4]) -> bool {
    matches!(
        kind,
        b"iprp"
            | b"ipco"
            | b"dinf"
            | b"grpl"
            | b"moov"
            | b"trak"
            | b"mdia"
            | b"minf"
            | b"stbl"
            | b"edts"
            | b"j2kH"
    )
}

/// Bytes a visual sample entry carries before its children.
///
/// `VisualSampleEntry::parse` reads six reserved bytes, a data reference index,
/// three fixed values, a width and height, two resolutions, four reserved bytes,
/// a frame count, a 32-byte compressor name, a depth and a pre-defined value.
const VISUAL_SAMPLE_ENTRY_PREFIX: usize = 78;

/// Offset of the compressor-name length byte inside that prefix.
const VISUAL_SAMPLE_ENTRY_NAME_LENGTH: usize = 42;

/// Sample entry types that are visual sample entries in the pinned libheif.
///
/// `uncv` is excluded because its dispatch sits behind the uncompressed-codec
/// feature, which the pinned build leaves disabled, so it is an opaque box
/// there. A type listed here is walked from the fixed prefix above.
fn is_visual_sample_entry(kind: &[u8; 4]) -> bool {
    matches!(
        kind,
        b"hvc1" | b"av01" | b"vvc1" | b"avc1" | b"mjpg" | b"j2ki"
    )
}

/// Brands for which libheif requires a movie box instead of a metadata box.
fn is_sequence_brand(brand: &[u8]) -> bool {
    matches!(brand, b"msf1" | b"isom" | b"mp41" | b"mp42")
}

/// Mirrors libheif's `item_type_is_image` in `libheif/context.cc`. Items that
/// are not images are loaded eagerly while the container is parsed.
fn is_image_item(item_type: &[u8; 4], content_type: &[u8]) -> bool {
    matches!(
        item_type,
        b"hvc1"
            | b"av01"
            | b"grid"
            | b"tili"
            | b"iden"
            | b"iovl"
            | b"avc1"
            | b"unci"
            | b"vvc1"
            | b"jpeg"
            | b"j2k1"
            | b"mski"
    ) || (item_type == b"mime" && content_type == b"image/jpeg")
}

/// Minimal ISOBMFF builders shared by the container and decoder tests.
#[cfg(test)]
pub(crate) mod fixture {
    /// Wraps `body` in a box header.
    pub(crate) fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend_from_slice(&u32::try_from(body.len() + 8).unwrap().to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    /// Wraps `body` in a full box header.
    pub(crate) fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![version];
        payload.extend_from_slice(&flags.to_be_bytes()[1..]);
        payload.extend_from_slice(body);
        boxed(kind, &payload)
    }

    fn cstring(value: &str) -> Vec<u8> {
        let mut out = value.as_bytes().to_vec();
        out.push(0);
        out
    }

    /// An `infe` entry declaring `id` with `item_type`.
    pub(crate) fn infe(id: u32, item_type: &[u8; 4], content_type: Option<&str>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u16::try_from(id).unwrap().to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(item_type);
        body.extend_from_slice(&cstring("item"));
        if let Some(content_type) = content_type {
            body.extend_from_slice(&cstring(content_type));
            body.extend_from_slice(&cstring(""));
        }
        full_box(b"infe", 2, 0, &body)
    }

    /// An `infe` entry whose MIME content type runs to the end of its box with
    /// no terminating zero, so the parser's last-byte rule applies.
    pub(crate) fn infe_mime_without_terminator(id: u32, content_type: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u16::try_from(id).unwrap().to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(b"mime");
        body.extend_from_slice(&cstring("item"));
        body.extend_from_slice(content_type.as_bytes());
        full_box(b"infe", 2, 0, &body)
    }

    /// An `iinf` box declaring `entries` as its children.
    pub(crate) fn iinf(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_be_bytes());
        for entry in entries {
            body.extend_from_slice(entry);
        }
        full_box(b"iinf", 0, 0, &body)
    }

    /// An `iloc` box with one four-byte-offset, four-byte-length record each.
    pub(crate) fn iloc(records: &[(u32, u32)]) -> Vec<u8> {
        iloc_version(0, records)
    }

    /// An `iloc` box in the requested version. Versions 1 and 2 add a
    /// construction-method field and widen the item identifier.
    pub(crate) fn iloc_version(version: u8, records: &[(u32, u32)]) -> Vec<u8> {
        iloc_extents(
            version,
            &records
                .iter()
                .map(|(id, len)| (*id, vec![*len]))
                .collect::<Vec<_>>(),
        )
    }

    /// An `iloc` box whose records declare the given extents each.
    pub(crate) fn iloc_extents(version: u8, records: &[(u32, Vec<u32>)]) -> Vec<u8> {
        let mut body = Vec::new();
        // Offset, length and base offset are four bytes each; the index size in
        // the low nibble is zero, so the records stay the same width.
        body.extend_from_slice(&0x4440u16.to_be_bytes());
        let count = u32::try_from(records.len()).unwrap();
        if version < 2 {
            body.extend_from_slice(&u16::try_from(count).unwrap().to_be_bytes());
        } else {
            body.extend_from_slice(&count.to_be_bytes());
        }
        for (id, extents) in records {
            if version < 2 {
                body.extend_from_slice(&u16::try_from(*id).unwrap().to_be_bytes());
            } else {
                body.extend_from_slice(&id.to_be_bytes());
            }
            if version >= 1 {
                body.extend_from_slice(&0u16.to_be_bytes());
            }
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&u16::try_from(extents.len()).unwrap().to_be_bytes());
            for length in extents {
                body.extend_from_slice(&0u32.to_be_bytes());
                body.extend_from_slice(&length.to_be_bytes());
            }
        }
        full_box(b"iloc", version, 0, &body)
    }

    /// An `iref` box holding one `cdsc` reference list.
    pub(crate) fn iref(references: &[u32]) -> Vec<u8> {
        let mut child = Vec::new();
        child.extend_from_slice(&1u16.to_be_bytes());
        child.extend_from_slice(&u16::try_from(references.len()).unwrap().to_be_bytes());
        for id in references {
            child.extend_from_slice(&u16::try_from(*id).unwrap().to_be_bytes());
        }
        full_box(b"iref", 0, 0, &boxed(b"cdsc", &child))
    }

    /// An `ipma` box associating `associations` properties with each entry.
    pub(crate) fn ipma(entries: &[(u32, usize)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u32::try_from(entries.len()).unwrap().to_be_bytes());
        for (id, associations) in entries {
            body.extend_from_slice(&u16::try_from(*id).unwrap().to_be_bytes());
            body.push(u8::try_from(*associations).unwrap());
            body.extend(std::iter::repeat_n(1, *associations));
        }
        full_box(b"ipma", 0, 0, &body)
    }

    /// A `meta` box holding `children`.
    pub(crate) fn meta(children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for child in children {
            body.extend_from_slice(child);
        }
        full_box(b"meta", 0, 0, &body)
    }

    /// A file type box with `major` and the given compatible brands.
    pub(crate) fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut body = major.to_vec();
        body.extend_from_slice(&0u32.to_be_bytes());
        for brand in compatible {
            body.extend_from_slice(*brand);
        }
        boxed(b"ftyp", &body)
    }

    /// A `uuid` box carrying a sixteen-byte extended type.
    pub(crate) fn uuid(extended: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let mut payload = extended.to_vec();
        payload.extend_from_slice(body);
        boxed(b"uuid", &payload)
    }

    /// A data reference box with the given entries.
    pub(crate) fn dref(children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u32::try_from(children.len()).unwrap().to_be_bytes());
        for child in children {
            body.extend_from_slice(child);
        }
        full_box(b"dref", 0, 0, &body)
    }

    /// A track reference box with the given reference lists.
    pub(crate) fn tref(children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for child in children {
            body.extend_from_slice(child);
        }
        boxed(b"tref", &body)
    }

    /// A visual sample entry holding `children` after its fixed prefix.
    pub(crate) fn visual_sample_entry(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        visual_sample_entry_named(kind, 0, children)
    }

    /// A visual sample entry with the compressor-name length byte set.
    pub(crate) fn visual_sample_entry_named(
        kind: &[u8; 4],
        name_length: u8,
        children: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut body = vec![0u8; 78];
        body[42] = name_length;
        for child in children {
            body.extend_from_slice(child);
        }
        boxed(kind, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{
        boxed, dref, ftyp, full_box, iinf, iloc, iloc_extents, iloc_version, infe,
        infe_mime_without_terminator, ipma, iref, meta, tref, uuid, visual_sample_entry,
        visual_sample_entry_named,
    };
    use super::*;

    fn budget() -> Budget {
        Budget {
            max_container_items: 4_096,
            max_container_associations: 32_768,
            max_metadata_items: 64,
            max_metadata_bytes: 4 * 1024 * 1024,
            max_container_boxes: 16_384,
            max_container_depth: 16,
            max_item_extents: 32,
        }
    }

    #[test]
    fn accepts_a_minimal_container() {
        let bytes = meta(&[
            iinf(&[
                infe(1, b"hvc1", None),
                infe(2, b"mime", Some("application/rdf+xml")),
            ]),
            iloc(&[(1, 1024), (2, 64)]),
            iref(&[1]),
            boxed(b"iprp", &ipma(&[(1, 1), (2, 1)])),
        ]);
        assert!(preflight(&bytes, &budget()).is_ok());
    }

    #[test]
    fn rejects_associations_that_are_legal_per_box() {
        // Each box stays under the per-box entry ceiling, but libheif merges
        // every `ipma` box into the first without an aggregate check.
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        let mut children = vec![iinf(&[infe(1, b"hvc1", None)])];
        for _ in 0..4 {
            children.push(boxed(b"iprp", &ipma(&entries)));
        }
        assert!(matches!(
            preflight(&meta(&children), &budget()),
            Err(Error::Limit("property association count"))
        ));
    }

    #[test]
    fn counts_boxes_wherever_the_native_parser_reaches_them() {
        // libheif dispatches by four-character code regardless of the parent, so
        // an `ipma` nested in `ipco`, or an `iinf` nested in `iinf`, is parsed
        // and allocated the same way as one in its conventional position.
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        let nested_ipma = meta(&[boxed(b"iprp", &boxed(b"ipco", &ipma(&entries)))]);
        assert!(matches!(
            preflight(&nested_ipma, &budget()),
            Err(Error::Limit("property association count"))
        ));

        let inner: Vec<Vec<u8>> = (0..5000).map(|id| infe(id + 1, b"hvc1", None)).collect();
        let nested_iinf = meta(&[iinf(&[iinf(&inner)])]);
        assert!(matches!(
            preflight(&nested_iinf, &budget()),
            Err(Error::Limit("container item count"))
        ));

        // A `trak` inside `moov` is reachable too.
        let deep = boxed(
            b"moov",
            &boxed(b"trak", &boxed(b"mdia", &boxed(b"minf", &ipma(&entries)))),
        );
        assert!(matches!(
            preflight(&deep, &budget()),
            Err(Error::Limit("property association count"))
        ));
    }

    #[test]
    fn counts_boxes_behind_container_prefixes() {
        // A data reference box, a URI meta sample entry and a visual sample
        // entry each carry a prefix before their children, so walking from the
        // box header would misread the prefix as a box.
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        let prefixed = [
            dref(&[boxed(b"url ", &[]), ipma(&entries)]),
            boxed(b"urim", &[vec![0u8; 8], ipma(&entries)].concat()),
            visual_sample_entry(b"hvc1", &[ipma(&entries)]),
        ];
        for bytes in prefixed {
            assert!(
                matches!(
                    preflight(&meta(&[bytes]), &budget()),
                    Err(Error::Limit("property association count"))
                ),
                "a container behind a prefix was not counted"
            );
        }
    }

    #[test]
    fn rejects_deeply_nested_counted_containers() {
        // The counted parsers descend through the shared dispatch rather than
        // through the walk, so the depth ceiling has to apply there too.
        let mut nested = iinf(&[]);
        for _ in 0..40 {
            nested = iinf(&[nested]);
        }
        assert!(matches!(
            preflight(&meta(&[nested]), &budget()),
            Err(Error::Limit("box nesting depth"))
        ));
    }

    #[test]
    fn counts_track_reference_destinations() {
        // Each list stays inside the per-list ceiling, but libheif bounds only
        // that list, so the walk counts destinations across all of them.
        let list: Vec<u8> = std::iter::repeat_n(1u32.to_be_bytes(), 3000)
            .flatten()
            .collect();
        let children: Vec<Vec<u8>> = (0..2).map(|_| boxed(b"hint", &list)).collect();
        assert!(matches!(
            preflight(&meta(&[tref(&children)]), &budget()),
            Err(Error::Limit("reference count"))
        ));
    }

    #[test]
    fn handles_uuid_extended_headers() {
        // A `uuid` box's sixteen-byte extended type is part of its header, so a
        // sibling must still be found after it and a trailing one must parse.
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        let bytes = meta(&[uuid(&[0u8; 16], &[]), boxed(b"iprp", &ipma(&entries))]);
        assert!(matches!(
            preflight(&bytes, &budget()),
            Err(Error::Limit("property association count"))
        ));
        assert!(preflight(&meta(&[uuid(&[0u8; 16], &[])]), &budget()).is_ok());
    }

    #[test]
    fn validates_the_sample_entry_compressor_name_length() {
        let entries: Vec<(u32, usize)> = (0..1024).map(|id| (id + 1, 255)).collect();
        // A name length that fits the field leaves the children where the walk
        // expects them.
        for length in [0u8, 31] {
            let bytes = meta(&[visual_sample_entry_named(
                b"hvc1",
                length,
                &[ipma(&entries)],
            )]);
            assert!(
                matches!(
                    preflight(&bytes, &budget()),
                    Err(Error::Limit("property association count"))
                ),
                "name length {length} should still reach the child"
            );
        }
        // A name length that does not fit makes the pinned parser read children
        // from a different offset, so the entry is refused instead of walked.
        for length in [32u8, 255] {
            let bytes = meta(&[visual_sample_entry_named(
                b"hvc1",
                length,
                &[ipma(&entries)],
            )]);
            assert!(
                matches!(preflight(&bytes, &budget()), Err(Error::Container(_))),
                "name length {length} should be refused"
            );
        }
    }

    #[test]
    fn does_not_traverse_an_opaque_sample_entry() {
        // `uncv` is not dispatched in the pinned build, so its payload is not a
        // box tree and must not be walked.
        assert!(preflight(&meta(&[boxed(b"uncv", &[0u8; 8])]), &budget()).is_ok());
    }

    #[test]
    fn rejects_a_track_reference_list_without_a_declared_size() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        assert!(matches!(
            preflight(&meta(&[boxed(b"tref", &body)]), &budget()),
            Err(Error::Container(_))
        ));
    }

    #[test]
    fn rejects_records_that_are_legal_per_box() {
        // Two `iinf` boxes each inside the per-box ceiling.
        let entries: Vec<Vec<u8>> = (0..3000).map(|id| infe(id + 1, b"hvc1", None)).collect();
        let children = vec![iinf(&entries), iinf(&entries)];
        assert!(matches!(
            preflight(&meta(&children), &budget()),
            Err(Error::Limit("container item count"))
        ));
    }

    #[test]
    fn rejects_item_location_records_that_are_legal_per_box() {
        let entries: Vec<Vec<u8>> = (0..3000).map(|id| infe(id + 1, b"hvc1", None)).collect();
        let records: Vec<(u32, u32)> = (0..3000).map(|id| (id + 1, 16)).collect();
        let children = vec![iinf(&entries), iloc(&records), iloc(&records)];
        assert!(matches!(
            preflight(&meta(&children), &budget()),
            Err(Error::Limit("item location record count"))
        ));
    }

    #[test]
    fn rejects_metadata_item_counts() {
        let entries: Vec<Vec<u8>> = (0..200).map(|id| infe(id + 10, b"Exif", None)).collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&entries)]), &budget()),
            Err(Error::Limit("metadata item count"))
        ));
    }

    #[test]
    fn counts_region_annotations_as_eagerly_loaded_items() {
        // libheif skips `rgan` in its ordinary metadata pass only because a
        // later pass loads and parses the regions.
        let entries: Vec<Vec<u8>> = (0..200).map(|id| infe(id + 10, b"rgan", None)).collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&entries)]), &budget()),
            Err(Error::Limit("metadata item count"))
        ));
    }

    #[test]
    fn charges_every_mime_item_because_a_later_pass_loads_them() {
        // libheif's text pass loads every `mime` item, so a MIME image is
        // eagerly read even though the ordinary metadata pass skips it.
        let terminated: Vec<Vec<u8>> = (0..200)
            .map(|id| infe(id + 10, b"mime", Some("image/jpeg")))
            .collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&terminated)]), &budget()),
            Err(Error::Limit("metadata item count"))
        ));
        // An unterminated content type reads one byte shorter and is therefore
        // not an image either, but it is charged for the same reason.
        let unterminated: Vec<Vec<u8>> = (0..200)
            .map(|id| infe_mime_without_terminator(id + 10, "image/jpeg"))
            .collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&unterminated)]), &budget()),
            Err(Error::Limit("metadata item count"))
        ));
    }

    #[test]
    fn charges_a_shared_extent_once_per_item() {
        // Fifty items that all point at the same one-megabyte extent stay under
        // the item ceiling but not under the byte budget, because libheif gives
        // each item its own allocation.
        let entries: Vec<Vec<u8>> = (0..50).map(|id| infe(id + 10, b"Exif", None)).collect();
        let records: Vec<(u32, u32)> = (0..50).map(|id| (id + 10, 1024 * 1024)).collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&entries), iloc(&records)]), &budget()),
            Err(Error::Limit("metadata extent bytes"))
        ));
    }

    #[test]
    fn charges_the_largest_record_for_an_item() {
        // libheif selects one metadata box and reads its locations, so a decoy
        // box declaring a zero-length extent must not hide the real one.
        let entries: Vec<Vec<u8>> = (0..8).map(|id| infe(id + 10, b"Exif", None)).collect();
        let decoy: Vec<(u32, u32)> = (0..8).map(|id| (id + 10, 0)).collect();
        let real: Vec<(u32, u32)> = (0..8).map(|id| (id + 10, 1024 * 1024)).collect();
        let children = [
            meta(&[iinf(&entries), iloc(&decoy)]),
            meta(&[iinf(&entries), iloc(&real)]),
        ];
        assert!(matches!(
            preflight(&children.concat(), &budget()),
            Err(Error::Limit("metadata extent bytes"))
        ));
    }

    #[test]
    fn counts_item_location_records_in_every_version() {
        // Eight metadata items that each declare a one-megabyte extent exceed
        // the byte budget, and the record walk must reach that conclusion for
        // every `iloc` version.
        let entries: Vec<Vec<u8>> = (0..8).map(|id| infe(id + 1, b"Exif", None)).collect();
        let records: Vec<(u32, u32)> = (0..8).map(|id| (id + 1, 1024 * 1024)).collect();
        for version in [0u8, 1, 2] {
            let bytes = meta(&[iinf(&entries), iloc_version(version, &records)]);
            assert!(
                matches!(
                    preflight(&bytes, &budget()),
                    Err(Error::Limit("metadata extent bytes"))
                ),
                "item location version {version} did not charge its extents"
            );
        }
    }

    #[test]
    fn rejects_extent_counts_beyond_the_item_ceiling() {
        // Field widths of zero mean an extent consumes no bytes, so an
        // unbounded count would let a tiny box drive a very large loop.
        let extents: Vec<u32> = vec![1; 40];
        let bytes = meta(&[
            iinf(&[infe(1, b"Exif", None)]),
            iloc_extents(0, &[(1, extents)]),
        ]);
        assert!(matches!(
            preflight(&bytes, &budget()),
            Err(Error::Limit("item extent count"))
        ));
    }

    #[test]
    fn rejects_reference_lists_that_do_not_fill_their_box() {
        // libheif reads a reference child's fields from the enclosing stream, so
        // a child declaring a larger size than its fields need would be read
        // again as further records.
        let mut child = Vec::new();
        child.extend_from_slice(&1u16.to_be_bytes());
        child.extend_from_slice(&1u16.to_be_bytes());
        child.extend_from_slice(&2u16.to_be_bytes());
        child.extend_from_slice(&0u32.to_be_bytes()); // padding the parser ignores
        let bytes = meta(&[full_box(b"iref", 0, 0, &boxed(b"cdsc", &child))]);
        assert!(matches!(
            preflight(&bytes, &budget()),
            Err(Error::Container(_))
        ));

        // A zero-length reference list is rejected by the parser as well.
        let empty = full_box(b"iref", 0, 0, &boxed(b"cdsc", &[0, 1, 0, 0]));
        assert!(matches!(
            preflight(&meta(&[empty]), &budget()),
            Err(Error::Container(_))
        ));
    }

    #[test]
    fn rejects_containers_beyond_the_box_and_depth_ceilings() {
        let limits = Budget {
            max_container_boxes: 4,
            ..budget()
        };
        let children: Vec<Vec<u8>> = (0..8).map(|id| infe(id + 1, b"hvc1", None)).collect();
        assert!(matches!(
            preflight(&meta(&[iinf(&children)]), &limits),
            Err(Error::Limit("box count"))
        ));

        let deep = Budget {
            max_container_depth: 1,
            ..budget()
        };
        let nested = meta(&[boxed(b"iprp", &boxed(b"ipco", &boxed(b"grpl", &[])))]);
        assert!(matches!(
            preflight(&nested, &deep),
            Err(Error::Limit("box nesting depth"))
        ));
    }

    #[test]
    fn rejects_malformed_or_incomplete_containers() {
        let limits = budget();
        // No metadata box at all, and no minimised or sequence-brand file.
        assert!(matches!(
            preflight(&ftyp(b"heic", &[b"mif1"]), &limits),
            Err(Error::Container(_))
        ));
        // A box that runs past the end of the file.
        let mut truncated = meta(&[iinf(&[infe(1, b"hvc1", None)])]);
        truncated.truncate(truncated.len() - 4);
        assert!(matches!(
            preflight(&truncated, &limits),
            Err(Error::Container(_))
        ));
        // An `iinf` whose declared count disagrees with its entries.
        let mut body = Vec::new();
        body.extend_from_slice(&3u16.to_be_bytes());
        body.extend_from_slice(&infe(1, b"hvc1", None));
        let mismatched = super::fixture::full_box(b"iinf", 0, 0, &body);
        assert!(matches!(
            preflight(&meta(&[mismatched]), &limits),
            Err(Error::Container(_))
        ));
        // An item location record whose widths the walk cannot follow.
        let mut body = Vec::new();
        body.extend_from_slice(&0x2220u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        let narrow = super::fixture::full_box(b"iloc", 0, 0, &body);
        assert!(matches!(
            preflight(&meta(&[narrow]), &limits),
            Err(Error::Container(_))
        ));
        // A `meta` box whose body cannot hold its own full-box header.
        let short = boxed(b"meta", &[0, 0]);
        assert!(matches!(
            preflight(&short, &limits),
            Err(Error::Container(_))
        ));
    }

    #[test]
    fn accepts_a_metadata_box_reached_through_a_movie_box() {
        let bytes = boxed(b"moov", &meta(&[iinf(&[infe(1, b"hvc1", None)])]));
        assert!(preflight(&bytes, &budget()).is_ok());
    }

    #[test]
    fn accepts_the_shapes_that_do_not_require_a_metadata_box() {
        // A minimised file carries its image in a `mini` box.
        let mini = boxed(b"mini", &[0; 32]);
        assert!(preflight(&mini, &budget()).is_ok());
        // A sequence brand uses a movie box instead.
        let sequence = boxed(b"moov", &[]);
        let mut file = ftyp(b"msf1", &[b"isom"]);
        file.extend_from_slice(&sequence);
        assert!(preflight(&file, &budget()).is_ok());
    }
}
