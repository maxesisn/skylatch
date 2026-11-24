#ifndef SKYLATCH_CLIENT_CORE_H
#define SKYLATCH_CLIENT_CORE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SkylatchClient SkylatchClient;

typedef struct SkylatchCallbacks {
    void (*on_frame)(void *user_data, uint32_t generation, uint32_t sequence,
                     uint16_t width, uint16_t height, uint64_t capture_us,
                     bool is_keyframe, const uint8_t *annex_b, size_t annex_b_len);
    /* Zero dimensions and NULL rgba restore the local cursor. */
    void (*on_cursor)(void *user_data, uint16_t width, uint16_t height,
                      uint16_t xhot, uint16_t yhot, const uint8_t *rgba, size_t rgba_len);
    void (*on_clipboard_notice)(void *user_data);
    /* Last callback. error is NULL on a clean close, otherwise a UTF-8 description. */
    void (*on_disconnected)(void *user_data, const char *error);
} SkylatchCallbacks;

#define SKYLATCH_OK 0
#define SKYLATCH_ERR_NULL (-1)
#define SKYLATCH_ERR_TRUNCATED (-2)
#define SKYLATCH_ERR_BAD_MAGIC (-3)
#define SKYLATCH_ERR_BAD_VERSION (-4)
#define SKYLATCH_ERR_UNKNOWN_TYPE (-5)
#define SKYLATCH_ERR_BAD_FLAGS (-6)
#define SKYLATCH_ERR_ZERO_GENERATION (-7)
#define SKYLATCH_ERR_TOO_LARGE (-8)
#define SKYLATCH_ERR_CLOSED (-9)
#define SKYLATCH_ERR_QUEUE_FULL (-10)
#define SKYLATCH_ERR_NOT_READY (-11)
#define SKYLATCH_ERR_INVALID (-12)

/* Numeric NSL1 wire version. The separate library version string is static. */
uint32_t skylatch_proto_version(void);
const char *skylatch_version_string(void);

/*
 * Discovery and connect calls block during setup; call them off the UI thread.
 * Strings are NUL-terminated UTF-8 and are only read during the call.
 * Failure returns NULL and writes a NUL-terminated diagnostic to err_buf (if
 * non-NULL and nonempty); success clears that buffer. It must not overlap inputs.
 * Hub URLs use ws:// or wss:// (http:// and https:// are accepted aliases).
 * The successful discovery result is a read-only JSON array of device objects.
 * Free it exactly once with skylatch_free_string, not the system allocator.
 */
char *skylatch_list_devices_json(const char *hub_url, const char *token,
                                 char *err_buf, size_t err_buf_len);
void skylatch_free_string(char *s);

/*
 * Callback table and individual functions may be NULL. The table is copied.
 * Callbacks run serially on the connection's worker thread and may start
 * before connect returns. Keep them short, and do not unwind across the ABI.
 * Data pointers passed to callbacks are borrowed until that callback returns.
 * Keep callback code and user_data alive until skylatch_destroy returns.
 * With prefer_direct, uncredentialed advertised endpoints are tried first;
 * the hub account token is never forwarded to them.
 */
SkylatchClient *skylatch_connect_hub(const char *hub_url, const char *token,
                                    const char *device_id, bool prefer_direct,
                                    const SkylatchCallbacks *callbacks,
                                    void *user_data, char *err_buf, size_t err_buf_len);

/* ws_url is a ws:// or wss:// origin or /stream URL. The core sets the stream
 * query. token_or_null, if non-NULL, is the host's 64-hex-digit direct token. */
SkylatchClient *skylatch_connect_direct(const char *ws_url, const char *token_or_null,
                                       const SkylatchCallbacks *callbacks,
                                       void *user_data, char *err_buf, size_t err_buf_len);

/* Thread-safe, non-blocking queue operations. SKYLATCH_OK means queued, not
 * delivered. Input requires an NSL1 generation to have been received first. */
int32_t skylatch_send_ack(SkylatchClient *client, uint32_t generation,
                          uint32_t sequence, uint32_t viewer_wait_us);
int32_t skylatch_send_input(SkylatchClient *client, const uint8_t record[16]);

/* Disconnect and join the worker. No callbacks occur after return. Call once
 * per handle, never from its callbacks or concurrently with other handle calls.
 * NULL is accepted by destroy and free_string. */
void skylatch_destroy(SkylatchClient *client);

typedef struct SkylatchNsl1Header {
    uint8_t msg_type;
    uint16_t flags;
    uint32_t generation;
    uint32_t sequence;
    uint32_t payload_len;
} SkylatchNsl1Header;

/* Inspect the first 20 bytes of buf; ignores trailing bytes. */
int32_t skylatch_parse_nsl1_header(const uint8_t *buf, size_t len, SkylatchNsl1Header *out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SKYLATCH_CLIENT_CORE_H */
