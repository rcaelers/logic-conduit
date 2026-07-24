#include <glib.h>
#include <inttypes.h>
#include <libsigrokdecode/libsigrokdecode.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static void annotation_output(struct srd_proto_data *data, void *unused)
{
    struct srd_proto_data_annotation *annotation = data->data;
    (void)unused;
    printf("A %" PRIu64 " %" PRIu64 " %d %s\n", data->start_sample,
           data->end_sample, annotation->ann_class,
           annotation->ann_text[0] ? annotation->ann_text[0] : "");
}

static void binary_output(struct srd_proto_data *data, void *unused)
{
    struct srd_proto_data_binary *binary = data->data;
    uint64_t index;
    (void)unused;
    printf("B %" PRIu64 " %" PRIu64 " %d ", data->start_sample,
           data->end_sample, binary->bin_class);
    for (index = 0; index < binary->size; index++)
        printf("%02X", binary->data[index]);
    putchar('\n');
}

static int failed(int result, const char *operation)
{
    if (result == SRD_OK)
        return 0;
    fprintf(stderr, "%s failed: %s\n", operation, srd_strerror(result));
    return 1;
}

int main(int argc, char **argv)
{
    struct srd_session *session = NULL;
    struct srd_decoder_inst *instance;
    GHashTable *channels;
    GVariant *sample_rate;
    uint8_t input[] = {
        0x06, 0x02, 0x03, 0x02, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
        0x03, 0x02, 0x01, 0x00, 0x03, 0x02, 0x03, 0x02, 0x06,
    };
    int result = EXIT_FAILURE;

    if (argc != 2) {
        fprintf(stderr, "usage: %s DECODER_DIRECTORY\n", argv[0]);
        return EXIT_FAILURE;
    }
    if (failed(srd_init(argv[1]), "srd_init"))
        return EXIT_FAILURE;
    if (failed(srd_decoder_load("spi"), "srd_decoder_load"))
        goto done;
    if (failed(srd_session_new(&session), "srd_session_new"))
        goto done;
    instance = srd_inst_new(session, "spi", NULL);
    if (!instance) {
        fputs("srd_inst_new failed\n", stderr);
        goto done;
    }

    channels = g_hash_table_new(g_str_hash, g_str_equal);
    g_hash_table_insert(channels, "clk", g_variant_new_int32(0));
    g_hash_table_insert(channels, "mosi", g_variant_new_int32(1));
    g_hash_table_insert(channels, "cs", g_variant_new_int32(2));
    if (failed(srd_inst_channel_set_all(instance, channels),
               "srd_inst_channel_set_all")) {
        g_hash_table_destroy(channels);
        goto done;
    }
    g_hash_table_destroy(channels);

    if (failed(srd_pd_output_callback_add(session, SRD_OUTPUT_ANN,
                                          annotation_output, NULL),
               "annotation callback") ||
        failed(srd_pd_output_callback_add(session, SRD_OUTPUT_BINARY,
                                          binary_output, NULL),
               "binary callback"))
        goto done;
    sample_rate = g_variant_new_uint64(1000000000);
    if (failed(srd_session_metadata_set(session, SRD_CONF_SAMPLERATE,
                                        sample_rate),
               "sample rate") ||
        failed(srd_session_start(session), "srd_session_start") ||
        failed(srd_session_send(session, 0, sizeof(input) - 1, input,
                                sizeof(input), 1),
               "srd_session_send") ||
        failed(srd_session_send_eof(session), "srd_session_send_eof"))
        goto done;
    result = EXIT_SUCCESS;

done:
    if (session)
        srd_session_destroy(session);
    srd_exit();
    return result;
}
