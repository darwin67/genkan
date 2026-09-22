/*
 * Authors the embedded-ICC-profile dynamic HEIC fixtures.
 *
 * The synthetic fixture generator (`generate.c`) attaches no color profile, so
 * nothing in the checked-in set exercised the ICC-to-sRGB conversion path.
 * These fixtures carry a self-authored profile instead of a third-party one, so
 * the repository redistributes no profile it does not own.
 *
 * usage: generate-icc OUTPUT.heic linear|unsupported|sixk
 *
 *   linear       8x8, five solid colors, with a linear-light matrix-shaper RGB
 *                profile. Its colorants are the sRGB/Rec.709 primaries adapted
 *                to the ICC D50 connection space, so the only difference from
 *                sRGB is the tone curve. Converting it to sRGB therefore has a
 *                predictable, observable result.
 *   unsupported  8x8, one solid color, with a profile whose data color space is
 *                CMYK. The decoder must refuse it rather than display it with
 *                unspecified color.
 *   sixk         6016x6016, one solid color, with the same linear-light
 *                profile. Its dimensions are above the 33,554,432-pixel and
 *                128 MiB ceilings the decoder used to apply, so it fails if
 *                either ceiling regresses, and it proves the conversion runs at
 *                the largest frame the decoder admits.
 *
 * Build with a C compiler and libheif development files:
 *
 *   cc $(pkg-config --cflags libheif) generate-icc.c \
 *     $(pkg-config --libs libheif) -lm -o generate-icc
 *   ./generate-icc synthetic-icc.heic linear
 *   ./generate-icc synthetic-icc-unsupported.heic unsupported
 *   ./generate-icc synthetic-icc-6k.heic sixk
 */

#include <libheif/heif.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Room for the header, the tag table, a complete v2 description, a
 * copyright tag, the white point, three colorants, and three curves. */
#define PROFILE_CAPACITY 1024
#define HEADER_BYTES 128
#define TAG_ENTRY_BYTES 12

static void fail(const char *message)
{
    fprintf(stderr, "%s\n", message);
    exit(EXIT_FAILURE);
}

static void check(struct heif_error error)
{
    if (error.code != heif_error_Ok) {
        fail(error.message);
    }
}

static void put_u16(unsigned char *out, unsigned int value)
{
    out[0] = (unsigned char)((value >> 8) & 0xffu);
    out[1] = (unsigned char)(value & 0xffu);
}

static void put_u32(unsigned char *out, unsigned int value)
{
    out[0] = (unsigned char)((value >> 24) & 0xffu);
    out[1] = (unsigned char)((value >> 16) & 0xffu);
    out[2] = (unsigned char)((value >> 8) & 0xffu);
    out[3] = (unsigned char)(value & 0xffu);
}

/* ICC `s15Fixed16Number`: a signed 16.16 fixed point value. */
static void put_s15f16(unsigned char *out, double value)
{
    put_u32(out, (unsigned int)(int)lround(value * 65536.0));
}

/* Writes the 128-byte profile header for an ICC v2.1.0 profile. */
static void write_header(unsigned char *out, size_t size, const char *space,
                         const char *device_class)
{
    memset(out, 0, HEADER_BYTES);
    put_u32(out, (unsigned int)size);
    /* ICC v2.1.0, because the smallest real-world samples that reach this
     * decoder are v2.1 profiles. */
    put_u32(out + 8, 0x02100000u);
    memcpy(out + 12, device_class, 4);
    memcpy(out + 16, space, 4);
    memcpy(out + 20, "XYZ ", 4);
    put_u16(out + 24, 2026);
    put_u16(out + 26, 1);
    put_u16(out + 28, 1);
    memcpy(out + 36, "acsp", 4);
    /* PCS illuminant: D50, the ICC connection space white point. */
    put_s15f16(out + 68, 0.9642);
    put_s15f16(out + 72, 1.0);
    put_s15f16(out + 76, 0.8249);
}

/*
 * Builds a minimal matrix-shaper profile: a `desc` tag, the media white point,
 * the three colorants, and three gamma tone curves.
 *
 * Returns the profile length in bytes.
 */
static size_t build_matrix_shaper_profile(unsigned char *out, double gamma,
                                          const char *description)
{
    /* Every tag carries four reserved bytes and the payloads are padded to a
     * four-byte boundary, so the whole buffer is cleared first: leaving them
     * uninitialized would serialize stack contents into the fixture and make
     * its hash depend on the build environment. */
    /* sRGB/Rec.709 primaries adapted to D50, matching the colorants a
     * conforming sRGB profile carries. */
    static const double colorants[3][3] = {
        { 0.4358437020317384, 0.22237346998171753, 0.01392884984376424 },
        { 0.3853088106132035, 0.7170281521655508, 0.09715675318321043 },
        { 0.14305951977573775, 0.060598377591300316, 0.7141027109713133 },
    };
    /* ICC does not require a particular tag order, only that signatures are
     * unique; writing them ascending keeps the fixture deterministic and easy
     * to read. `cprt` is a required tag for a display profile. */
    static const char *signatures[] = { "bTRC", "bXYZ", "cprt", "desc", "gTRC",
                                        "gXYZ", "rTRC", "rXYZ", "wtpt" };
    static const char copyright[] =
        "Authored for Genkan under the repository MIT license.";
    size_t tag_count = sizeof(signatures) / sizeof(signatures[0]);
    size_t description_length = strlen(description) + 1;
    unsigned char *tags = out + HEADER_BYTES;
    unsigned char *cursor = out + HEADER_BYTES + 4 + TAG_ENTRY_BYTES * tag_count;
    size_t index;

    put_u32(tags, (unsigned int)tag_count);
    for (index = 0; index < tag_count; index++) {
        unsigned char *entry = tags + 4 + TAG_ENTRY_BYTES * index;
        size_t written;

        memcpy(entry, signatures[index], 4);
        put_u32(entry + 4, (unsigned int)(cursor - out));
        if (strcmp(signatures[index], "desc") == 0) {
            /* A complete v2 textDescriptionType: the ASCII section, then the
             * Unicode language code and count, then the ScriptCode code, count,
             * and its fixed 67-byte description field. The trailing sections
             * are empty but must be present at their full size. */
            written = 12 + description_length + 4 + 4 + 2 + 1 + 67;
            memcpy(cursor, "desc", 4);
            put_u32(cursor + 8, (unsigned int)description_length);
            memcpy(cursor + 12, description, description_length);
            /* Unicode language code, Unicode count, Unicode string, ScriptCode
             * code, count, and description all stay zero from the cleared
             * buffer. */
        } else if (strcmp(signatures[index], "cprt") == 0) {
            /* A required `textType` copyright notice. */
            written = 8 + sizeof(copyright);
            memcpy(cursor, "text", 4);
            memcpy(cursor + 8, copyright, sizeof(copyright));
        } else if (strcmp(signatures[index], "wtpt") == 0) {
            written = 20;
            memcpy(cursor, "XYZ ", 4);
            put_s15f16(cursor + 8, 0.9642);
            put_s15f16(cursor + 12, 1.0);
            put_s15f16(cursor + 16, 0.8249);
        } else if (strcmp(signatures[index], "bXYZ") == 0) {
            written = 20;
            memcpy(cursor, "XYZ ", 4);
            put_s15f16(cursor + 8, colorants[2][0]);
            put_s15f16(cursor + 12, colorants[2][1]);
            put_s15f16(cursor + 16, colorants[2][2]);
        } else if (strcmp(signatures[index], "gXYZ") == 0) {
            written = 20;
            memcpy(cursor, "XYZ ", 4);
            put_s15f16(cursor + 8, colorants[1][0]);
            put_s15f16(cursor + 12, colorants[1][1]);
            put_s15f16(cursor + 16, colorants[1][2]);
        } else if (strcmp(signatures[index], "rXYZ") == 0) {
            written = 20;
            memcpy(cursor, "XYZ ", 4);
            put_s15f16(cursor + 8, colorants[0][0]);
            put_s15f16(cursor + 12, colorants[0][1]);
            put_s15f16(cursor + 16, colorants[0][2]);
        } else {
            /* `curv` with a single entry is the v2 gamma encoding. */
            written = 14;
            memcpy(cursor, "curv", 4);
            put_u32(cursor + 8, 1);
            put_u16(cursor + 12, (unsigned int)lround(gamma * 256.0));
        }
        put_u32(entry + 8, (unsigned int)written);
        /* ICC requires each tag's data to begin on a four-byte boundary. */
        cursor += (written + 3u) & ~(size_t)3u;
    }

    write_header(out, (size_t)(cursor - out), "RGB ", "mntr");
    return (size_t)(cursor - out);
}

/*
 * Builds a header-only profile whose data color space is CMYK, which is a
 * colour space this decoder does not convert. A conforming CMYK profile would
 * carry A2B/B2A tag tables; none are needed to prove the colour space is
 * refused, and the empty tag table keeps the fixture's intent obvious.
 */
static size_t build_cmyk_profile(unsigned char *out)
{
    write_header(out, HEADER_BYTES + 4, "CMYK", "prtr");
    put_u32(out + HEADER_BYTES, 0);
    return HEADER_BYTES + 4;
}

static const unsigned char linear_colors[][3] = {
    { 224, 32, 32 },
    { 32, 224, 32 },
    { 32, 32, 224 },
    { 128, 128, 128 },
    { 32, 32, 32 },
};

static void encode(struct heif_context *context, struct heif_encoder *encoder,
                   const unsigned char *profile, size_t profile_size, int width,
                   int height, unsigned char red, unsigned char green,
                   unsigned char blue)
{
    struct heif_image *image = NULL;
    struct heif_image_handle *handle = NULL;
    unsigned char *pixels;
    int stride;
    int x;
    int y;

    check(heif_image_create(width, height, heif_colorspace_RGB,
                            heif_chroma_interleaved_RGB, &image));
    check(heif_image_add_plane(image, heif_channel_interleaved, width, height,
                               8));
    if (profile != NULL) {
        check(heif_image_set_raw_color_profile(image, "prof", profile,
                                               profile_size));
    }
    pixels = heif_image_get_plane(image, heif_channel_interleaved, &stride);
    if (pixels == NULL) {
        fail("failed to allocate image plane");
    }
    for (y = 0; y < height; y++) {
        for (x = 0; x < width; x++) {
            unsigned char *pixel = pixels + y * stride + x * 3;
            pixel[0] = red;
            pixel[1] = green;
            pixel[2] = blue;
        }
    }

    check(heif_context_encode_image(context, image, encoder, NULL, &handle));
    heif_image_release(image);
    heif_image_handle_release(handle);
}

int main(int argc, char **argv)
{
    const struct heif_encoder_descriptor *encoders[8];
    struct heif_context *context;
    struct heif_encoder *encoder = NULL;
    unsigned char profile[PROFILE_CAPACITY];
    size_t profile_size = 0;
    int encoder_count;
    int encoder_index;
    size_t color_index;
    int width;
    int height;

    if (argc != 3) {
        fail("usage: generate-icc OUTPUT.heic linear|unsupported|sixk");
    }

    memset(profile, 0, sizeof(profile));

    if (strcmp(argv[2], "linear") == 0) {
        profile_size = build_matrix_shaper_profile(
            profile, 1.0, "Genkan linear-light Rec.709 test profile");
        width = 8;
        height = 8;
    } else if (strcmp(argv[2], "unsupported") == 0) {
        profile_size = build_cmyk_profile(profile);
        width = 8;
        height = 8;
    } else if (strcmp(argv[2], "sixk") == 0) {
        profile_size = build_matrix_shaper_profile(
            profile, 1.0, "Genkan linear-light Rec.709 test profile");
        width = 6016;
        height = 6016;
    } else {
        fail("unknown fixture kind");
        return EXIT_FAILURE;
    }

    context = heif_context_alloc();
    if (context == NULL) {
        fail("failed to allocate libheif context");
    }
    encoder_count = heif_get_encoder_descriptors(
        heif_compression_HEVC, "x265", encoders,
        (int)(sizeof(encoders) / sizeof(encoders[0])));
    for (encoder_index = 0; encoder_index < encoder_count; encoder_index++) {
        if (strcmp(heif_encoder_descriptor_get_id_name(encoders[encoder_index]),
                   "x265") == 0) {
            check(heif_context_get_encoder(context, encoders[encoder_index],
                                           &encoder));
            break;
        }
    }
    if (encoder == NULL) {
        fail("libheif x265 encoder is unavailable");
    }
    check(heif_encoder_set_lossless(encoder, 1));

    if (strcmp(argv[2], "linear") == 0) {
        for (color_index = 0;
             color_index < sizeof(linear_colors) / sizeof(linear_colors[0]);
             color_index++) {
            encode(context, encoder, profile, profile_size, width, height,
                   linear_colors[color_index][0], linear_colors[color_index][1],
                   linear_colors[color_index][2]);
        }
    } else {
        encode(context, encoder, profile_size == 0 ? NULL : profile,
               profile_size, width, height, 128, 128, 128);
    }

    check(heif_context_write_to_file(context, argv[1]));

    heif_encoder_release(encoder);
    heif_context_free(context);
    return EXIT_SUCCESS;
}
