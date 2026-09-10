#include <libheif/heif.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define WIDTH 12
#define HEIGHT 8

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
        {255, 0, 0}, {0, 255, 0}, {0, 0, 255},
        {255, 255, 0}, {255, 0, 255}, {0, 255, 255},
    };
    struct heif_context *context;
    const struct heif_encoder_descriptor *descriptors[8];
    struct heif_encoder *encoder = NULL;
    struct heif_image *image = NULL;
    struct heif_image_handle *handle = NULL;
    unsigned char *pixels;
    int count;
    int stride;

    if (argc != 2) {
        fail("usage: generate-geometry OUTPUT.heic");
    }
    context = heif_context_alloc();
    if (context == NULL) {
        fail("failed to allocate libheif context");
    }
    count = heif_get_encoder_descriptors(
        heif_compression_HEVC, "x265", descriptors,
        (int)(sizeof(descriptors) / sizeof(descriptors[0])));
    for (int index = 0; index < count; index++) {
        if (strcmp(heif_encoder_descriptor_get_id_name(descriptors[index]), "x265") == 0) {
            check(heif_context_get_encoder(context, descriptors[index], &encoder));
            break;
        }
    }
    if (encoder == NULL) {
        fail("libheif x265 encoder is unavailable");
    }
    check(heif_encoder_set_lossless(encoder, 1));
    check(heif_image_create(WIDTH, HEIGHT, heif_colorspace_RGB,
                            heif_chroma_interleaved_RGB, &image));
    check(heif_image_add_plane(image, heif_channel_interleaved, WIDTH, HEIGHT, 8));
    pixels = heif_image_get_plane(image, heif_channel_interleaved, &stride);
    if (pixels == NULL) {
        fail("failed to allocate image plane");
    }
    for (int y = 0; y < HEIGHT; y++) {
        for (int x = 0; x < WIDTH; x++) {
            const unsigned char *color = colors[(y / 4) * 3 + x / 4];
            memcpy(pixels + y * stride + x * 3, color, 3);
        }
    }
    /* Break rotational symmetry without relying on text or third-party art. */
    memset(pixels, 255, 3);

    check(heif_context_encode_image(context, image, encoder, NULL, &handle));
    check(heif_context_write_to_file(context, argv[1]));

    heif_image_handle_release(handle);
    heif_image_release(image);
    heif_encoder_release(encoder);
    heif_context_free(context);
    return EXIT_SUCCESS;
}
