#ifndef HYDRA_NATIVE_H
#define HYDRA_NATIVE_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* Native ABI 1: all buffers are borrowed for the synchronous call.
 * Callbacks must run on the calling thread; no exception may cross this ABI.
 * Every library thread must exit before hydra_native_v1 returns.
 * emit accepts one JSON object; nonzero means stop. poll returns nonzero on
 * cancellation. A download emits complete/stopped only after durable checkpointing. */
typedef int32_t (*hydra_native_emit)(void *, const uint8_t *, size_t);
/* poll writes a live download cap: 0 unlimited, UINT64_MAX unchanged,
 * UINT64_MAX-1 suspends transfer traffic until the cap changes. */
typedef int32_t (*hydra_native_poll)(void *, uint64_t *);
#if defined(_WIN32)
#define HYDRA_NATIVE_EXPORT __declspec(dllexport)
#else
#define HYDRA_NATIVE_EXPORT __attribute__((visibility("default")))
#endif
HYDRA_NATIVE_EXPORT int32_t hydra_native_v1(
    const uint8_t *method, size_t method_len,
    const uint8_t *request, size_t request_len,
    hydra_native_emit emit, hydra_native_poll poll, void *context);
#ifdef __cplusplus
}
#endif
#endif
