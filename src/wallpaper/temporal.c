/* Eligibility checks using GStreamer's existing HEVC parser.
 * Unsupported streams are not rejected for playback: they are not optimized.
 */
#define GST_USE_UNSTABLE_API
#include <gst/codecparsers/gsth265parser.h>
#include <string.h>

typedef struct {
  guint64 pictures[17];
  guint count;
} References;

typedef struct {
  GstH265Parser *parser;
  GBytes *parameters[3];
  gboolean first;
  guint64 epoch, previous_tid0, maximum_index;
  References full, extracted;
} TemporalParser;

void temporal_parser_free(TemporalParser *self) {
  if (!self) return;
  for (unsigned i = 0; i < 3; i++)
    g_clear_pointer(&self->parameters[i], g_bytes_unref);
  gst_h265_parser_free(self->parser);
  g_free(self);
}

static gboolean header_supported(const GstH265NalUnit *nal) {
  return nal->size >= 2 && !(nal->data[nal->offset] & 0x80) &&
         nal->layer_id == 0 && nal->temporal_id_plus1 >= 1 &&
         nal->temporal_id_plus1 <= 5;
}

TemporalParser *temporal_parser_new(const guint8 *data, gsize size) {
  TemporalParser *self = g_new0(TemporalParser, 1);
  self->parser = gst_h265_parser_new();
  self->first = TRUE;
  GstH265DecoderConfigRecord *config = NULL;
  if (gst_h265_parser_parse_decoder_config_record(self->parser, data, size,
                                                 &config) != GST_H265_PARSER_OK)
    goto reject;
  if (config->configuration_version != 1 ||
      config->general_profile_space != 0 || config->general_profile_idc != 2 ||
      config->chroma_format_idc != 1 || config->bit_depth_luma_minus8 != 2 ||
      config->bit_depth_chroma_minus8 != 2 || config->num_temporal_layers != 5 ||
      config->length_size_minus_one != 3 || config->nalu_array->len != 3)
    goto reject;
  /* Exactly one VPS, SPS and PPS, in dependency order. No out-of-band SEI. */
  for (unsigned i = 0; i < 3; i++) {
    GstH265DecoderConfigRecordNalUnitArray *array =
        &g_array_index(config->nalu_array, GstH265DecoderConfigRecordNalUnitArray, i);
    if (array->nal_unit_type != GST_H265_NAL_VPS + i || array->nalu->len != 1)
      goto reject;
    GstH265NalUnit *nal = &g_array_index(array->nalu, GstH265NalUnit, 0);
    if (!header_supported(nal) || nal->type != array->nal_unit_type ||
        nal->temporal_id_plus1 != 1 ||
        (i == 2 && nal->offset + nal->size != size))
      goto reject;
    GstH265ParserResult result;
    if (nal->type == GST_H265_NAL_SPS) {
      /* parse_nal disables VUI parsing, which leaves the reader at VUI bits
       * when it reads the extension flags. Validate the complete SPS instead. */
      GstH265SPS *sps = g_new0(GstH265SPS, 1);
      result = gst_h265_parser_parse_sps(self->parser, nal, sps, TRUE);
      g_free(sps);
    } else {
      result = gst_h265_parser_parse_nal(self->parser, nal);
    }
    if (result != GST_H265_PARSER_OK) goto reject;
    self->parameters[i] = g_bytes_new(nal->data + nal->offset, nal->size);
  }
  const GstH265VPS *vps = self->parser->last_vps;
  const GstH265SPS *sps = self->parser->last_sps;
  const GstH265PPS *pps = self->parser->last_pps;
  if (!vps || !sps || !pps || sps->vps_id != vps->id || pps->sps_id != sps->id ||
      vps->max_layers_minus1 != 0 || vps->max_layer_id != 0 ||
      vps->max_sub_layers_minus1 != 4 || sps->max_sub_layers_minus1 != 4 ||
      sps->profile_tier_level.profile_idc != 2 ||
      !sps->profile_tier_level.progressive_source_flag ||
      sps->profile_tier_level.interlaced_source_flag ||
      sps->chroma_format_idc != 1 || sps->separate_colour_plane_flag ||
      sps->bit_depth_luma_minus8 != 2 || sps->bit_depth_chroma_minus8 != 2 ||
      sps->sps_extension_flag || pps->pps_extension_flag ||
      (sps->vui_parameters_present_flag && sps->vui_params.field_seq_flag))
    goto reject;
  gst_h265_decoder_config_record_free(config);
  return self;
reject:
  if (config) gst_h265_decoder_config_record_free(config);
  temporal_parser_free(self);
  return NULL;
}

/* H.265 8.3.2: a used short-term reference must already be available, and
 * pictures absent from the current RPS cease to be references. Check both the
 * full and extracted streams: temporal labels alone do not prove dependency
 * closure, and a decoder can conceal missing references without an error. */
static gboolean update_references(References *references,
                                  const GstH265SliceHdr *slice, guint64 index,
                                  gboolean idr, gboolean reference_picture) {
  References next = {0};
  if (slice->num_long_term_sps || slice->num_long_term_pics) return FALSE;
  if (!idr) {
    const GstH265SPS *sps = slice->pps->sps;
    const GstH265ShortTermRefPicSet *rps = &slice->short_term_ref_pic_sets;
    if (slice->short_term_ref_pic_set_sps_flag) {
      if (slice->short_term_ref_pic_set_idx >= sps->num_short_term_ref_pic_sets)
        return FALSE;
      rps = &sps->short_term_ref_pic_set[slice->short_term_ref_pic_set_idx];
    }
    if (rps->NumNegativePics + rps->NumPositivePics > 16) return FALSE;
    for (guint direction = 0; direction < 2; direction++) {
      guint count = direction ? rps->NumPositivePics : rps->NumNegativePics;
      const gint32 *deltas = direction ? rps->DeltaPocS1 : rps->DeltaPocS0;
      const guint8 *used = direction ? rps->UsedByCurrPicS1 : rps->UsedByCurrPicS0;
      for (guint i = 0; i < count; i++) {
        gint64 target = (gint64) index + deltas[i];
        gboolean found = FALSE;
        for (guint j = 0; j < references->count; j++)
          found |= target >= 0 && references->pictures[j] == (guint64) target;
        if (used[i] && !found) return FALSE;
        if (found) next.pictures[next.count++] = target;
      }
    }
  }
  if (reference_picture) next.pictures[next.count++] = index;
  *references = next;
  return TRUE;
}

/* Return the picture's temporal ID, or -1 when this whole AU cannot safely be
 * kept/dropped unchanged. This checks original demuxed bytes, before h265parse
 * has an opportunity to repair or discard an invalid NAL. */
int temporal_parser_access_unit(TemporalParser *self, const guint8 *data,
                                gsize size, guint64 index) {
  if (!size || size > G_MAXUINT) return -1;
  guint offset = 0;
  int picture = -1;
  int keep = -1;
  while (offset < size) {
    GstH265NalUnit nal;
    if (gst_h265_parser_identify_nalu_hevc(self->parser, data, offset, size, 4,
                                         &nal) != GST_H265_PARSER_OK ||
        !header_supported(&nal) || nal.offset != offset + 4 ||
        nal.size > size - nal.offset)
      return -1;
    int tid = nal.temporal_id_plus1 - 1;
    int nal_keep = tid <= 1;
    if (keep != -1 && keep != nal_keep) return -1;
    keep = nal_keep;
    if (nal.type <= 31) {
      if (picture != -1 ||
          !((tid == 0 && (nal.type == 1 || nal.type == 19)) ||
            (tid >= 1 && tid <= 3 && nal.type == 3) ||
            (tid == 4 && nal.type == 2)) ||
          (self->first && nal.type != 19))
        return -1;
      GstH265SliceHdr slice = {0};
      gboolean supported =
          gst_h265_parser_parse_slice_hdr(self->parser, &nal, &slice) == GST_H265_PARSER_OK &&
          slice.first_slice_segment_in_pic_flag && slice.pic_output_flag &&
          !slice.no_output_of_prior_pics_flag &&
          slice.pps == self->parser->last_pps &&
          slice.pps->sps == self->parser->last_sps &&
          slice.pps->sps->vps == self->parser->last_vps &&
          update_references(&self->full, &slice, index, nal.type == 19, nal.type != 2) &&
          (tid > 1 || update_references(&self->extracted, &slice, index, nal.type == 19, TRUE));
      guint lsb = slice.pic_order_cnt_lsb;
      gst_h265_slice_hdr_free(&slice);
      if (!supported) return -1;
      if (nal.type == 19) {
        if (self->first ? index != 0 : index != self->maximum_index + 1) return -1;
        self->epoch = index;
        self->previous_tid0 = index;
      } else {
        if (index < self->epoch) return -1;
        /* H.265 8.3.1: bind decoder picture order, including LSB wraps, to the
         * validated presentation index. Only T0 updates the previous POC. */
        gint64 modulus = 1LL << (self->parser->last_sps->log2_max_pic_order_cnt_lsb_minus4 + 4);
        gint64 previous = self->previous_tid0 - self->epoch;
        gint64 previous_lsb = previous % modulus;
        gint64 poc = previous - previous_lsb + lsb;
        if (lsb < previous_lsb && previous_lsb - lsb >= modulus / 2) poc += modulus;
        else if (lsb > previous_lsb && lsb - previous_lsb > modulus / 2) poc -= modulus;
        if (poc != (gint64) (index - self->epoch)) return -1;
        if (tid == 0) self->previous_tid0 = index;
      }
      self->maximum_index = MAX(self->maximum_index, index);
      picture = tid;
    } else if (nal.type >= GST_H265_NAL_VPS && nal.type <= GST_H265_NAL_PPS) {
      GBytes *expected = self->parameters[nal.type - GST_H265_NAL_VPS];
      gsize length = 0;
      const guint8 *bytes = g_bytes_get_data(expected, &length);
      if (tid != 0 || length != nal.size || memcmp(bytes, data + nal.offset, length))
        return -1;
    } else if (nal.type == GST_H265_NAL_PREFIX_SEI || nal.type == GST_H265_NAL_SUFFIX_SEI) {
      GArray *messages = NULL;
      GstH265ParserResult result = gst_h265_parser_parse_sei(self->parser, &nal, &messages);
      gboolean supported = result == GST_H265_PARSER_OK && messages && messages->len;
      for (guint i = 0; messages && i < messages->len; i++) {
        const GstH265SEIMessage *message = &g_array_index(messages, GstH265SEIMessage, i);
        /* In particular, do not forward timing SEI that extraction must remove,
         * or silently remove persistent display metadata with a high-layer AU. */
        supported &= message->payloadType == GST_H265_SEI_USER_DATA_UNREGISTERED;
      }
      if (messages) g_array_unref(messages);
      if (!supported) return -1;
    } else {
      return -1;
    }
    offset = nal.offset + nal.size;
  }
  if (picture < 0) return -1;
  self->first = FALSE;
  return picture;
}
