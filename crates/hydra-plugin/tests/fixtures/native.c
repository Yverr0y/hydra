#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <stdio.h>

typedef int32_t (*emit_fn)(void *, const uint8_t *, size_t);
typedef int32_t (*poll_fn)(void *, uint64_t *);
#if defined(_WIN32)
__declspec(dllexport)
#endif
int32_t hydra_native_v1(const uint8_t *method, size_t method_len,
                       const uint8_t *request, size_t request_len,
                       emit_fn emit, poll_fn poll, void *context) {
    char input[4096];
    if (request_len >= sizeof input) return 1;
    memcpy(input, request, request_len);
    input[request_len] = 0;
    if (method_len == 4 && memcmp(method, "fail", 4) == 0) return 7;
    if (strstr(input, "null_reply")) return emit(context, NULL, 1);
    if (strstr(input, "oversized")) return emit(context, (const uint8_t *)"", 21 * 1024 * 1024);
    if (strstr(input, "invalid_log")) {
        const char *frame = "{\"state\":\"complete\",\"logs\":[{\"level\":\"unknown\",\"message\":\"test\"}]}";
        return emit(context, (const uint8_t *)frame, strlen(frame));
    }
    if (strstr(input, "no_reply")) return 0;
    if (strstr(input, "malformed")) return emit(context, (const uint8_t *)"{", 1);
    if (strstr(input, "error_reply")) {
        const char *error = "{\"state\":\"error\",\"error\":\"disk full\"}";
        return emit(context, (const uint8_t *)error, strlen(error));
    }
    if (method_len == 8 && memcmp(method, "download", 8) == 0) {
        uint64_t limit;
        const char *state = poll(context, &limit) ? "stopped" : "complete";
        if (strstr(input, "unfinished")) state = "downloading";
        const char *progress = "{\"state\":\"downloading\",\"done\":5,\"total\":10,\"logs\":[{\"level\":\"info\",\"message\":\"Transfer started\"}]}";
        if (emit(context, (const uint8_t *)progress, strlen(progress))) return 1;
        char final[128];
        snprintf(final, sizeof final, "{\"state\":\"%s\",\"done\":10,\"total\":10}", state);
        return emit(context, (const uint8_t *)final, strlen(final));
    }
    return emit(context, request, request_len);
}
