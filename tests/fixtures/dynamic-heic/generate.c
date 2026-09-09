#include <libheif/heif.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define WIDTH 8
#define HEIGHT 8

static const char xmp[] =
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">"
    "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">"
    "<rdf:Description xmlns:apple_desktop=\"http://ns.apple.com/namespace/1.0/\" "
    "apple_desktop:h24=\"YnBsaXN0MDDSAQIDCFJhcFJ0adIEBQYHUWRRbBADEACkCQ0QE9IKCwcMUWlRdCMAAAAAAAAAANIKCw4PEAEjP9AAAAAAAADSCgsREhACIz/gAAAAAAAA0goLBhQjP+gAAAAAAAAIDRATGBocHiAlKiwuNzw+R0xOV1wAAAAAAAABAQAAAAAAAAAVAAAAAAAAAAAAAAAAAAAAZQ==\" "
    "apple_desktop:solar=\"YnBsaXN0MDDSAQIDCFJhcFJzadIEBQYHUWRRbBADEACkCQ8TF9MKCwwNBw5RYVFpUXojwDQAAAAAAAAjAAAAAAAAAADTCgsMEBESI0AuAAAAAAAAEAEjQFaAAAAAAADTCgsMFBUWI0BLgAAAAAAAEAIjQGaAAAAAAADTCgsMGAYZI0AkAAAAAAAAI0Bw4AAAAAAACA0QExgaHB4gJSwuMDI7REtUVl9mb3F6gYoAAAAAAAABAQAAAAAAAAAaAAAAAAAAAAAAAAAAAAAAkw==\" "
    "apple_desktop:apr=\"YnBsaXN0MDDSAQIDBFFkUWwQAxAACA0PERMAAAAAAAABAQAAAAAAAAAFAAAAAAAAAAAAAAAAAAAAFQ==\"/>"
    "</rdf:RDF></x:xmpmeta>";

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

int main(int argc, char **argv)
{
    static const unsigned char colors[][3] = {
        { 255, 0, 0 },
        { 0, 255, 0 },
        { 0, 0, 255 },
        { 255, 255, 255 },
    };
    struct heif_context *context;
    const struct heif_encoder_descriptor *encoders[8];
    struct heif_encoder *encoder = NULL;
    struct heif_image_handle *primary = NULL;
    int encoder_count;
    int encoder_index;
    size_t color_index;

    if (argc != 2) {
        fail("usage: generate OUTPUT.heic");
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

    for (color_index = 0; color_index < sizeof(colors) / sizeof(colors[0]);
         color_index++) {
        struct heif_image *image = NULL;
        struct heif_image_handle *handle = NULL;
        unsigned char *pixels;
        int stride;
        int x;
        int y;

        check(heif_image_create(WIDTH, HEIGHT, heif_colorspace_RGB,
                                heif_chroma_interleaved_RGB, &image));
        check(heif_image_add_plane(image, heif_channel_interleaved, WIDTH,
                                   HEIGHT, 8));
        pixels = heif_image_get_plane(image, heif_channel_interleaved, &stride);
        if (pixels == NULL) {
            fail("failed to allocate image plane");
        }
        for (y = 0; y < HEIGHT; y++) {
            for (x = 0; x < WIDTH; x++) {
                memcpy(pixels + y * stride + x * 3, colors[color_index], 3);
            }
        }

        check(heif_context_encode_image(context, image, encoder, NULL, &handle));
        heif_image_release(image);
        if (color_index == 0) {
            primary = handle;
        } else {
            heif_image_handle_release(handle);
        }
    }

    check(heif_context_add_XMP_metadata(context, primary, xmp,
                                        (int)strlen(xmp)));
    check(heif_context_write_to_file(context, argv[1]));

    heif_image_handle_release(primary);
    heif_encoder_release(encoder);
    heif_context_free(context);
    return EXIT_SUCCESS;
}
