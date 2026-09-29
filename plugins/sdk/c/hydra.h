#ifndef HYDRA_PLUGIN_SDK_H
#define HYDRA_PLUGIN_SDK_H
#include <stdint.h>
#include <stddef.h>

/* UTF-8 JSON buffers are length-delimited, never assumed NUL-terminated. */
typedef struct { const char *data; uint32_t len; } hydra_bytes;
/* Implement this function in your plugin. The reply lives until the call returns. */
hydra_bytes hydra_dispatch(hydra_bytes method, hydra_bytes request);
/* Caller owns the returned buffer and releases it with free(). */
hydra_bytes hydra_host_call_json(const char *name, hydra_bytes request);
hydra_bytes hydra_json(const char *json);
int hydra_method_is(hydra_bytes method, const char *name);
#endif
