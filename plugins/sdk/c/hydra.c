#include "hydra.h"
#include <stdlib.h>
#include <string.h>

__attribute__((import_module("hydra"), import_name("hydra_host_call")))
extern int64_t host_call(int32_t, int32_t, int32_t, int32_t);

__attribute__((export_name("hydra_api"))) int32_t hydra_api(void) { return 1; }
__attribute__((export_name("hydra_alloc"))) int32_t hydra_alloc(int32_t len) {
    if (len < 0 || len > 16 * 1024 * 1024) abort();
    void *p = malloc(len ? (size_t)len : 1);
    if (!p) abort();
    return (int32_t)(uintptr_t)p;
}
__attribute__((export_name("hydra_call")))
int64_t hydra_call(int32_t mp, int32_t ml, int32_t rp, int32_t rl) {
    hydra_bytes method = {(char *)(uintptr_t)(uint32_t)mp, (uint32_t)ml};
    hydra_bytes request = {(char *)(uintptr_t)(uint32_t)rp, (uint32_t)rl};
    hydra_bytes reply = hydra_dispatch(method, request);
    int32_t ptr = hydra_alloc((int32_t)reply.len);
    memcpy((void *)(uintptr_t)(uint32_t)ptr, reply.data, reply.len);
    free((void *)method.data);
    free((void *)request.data);
    return (int64_t)(((uint64_t)(uint32_t)ptr << 32) | reply.len);
}
hydra_bytes hydra_host_call_json(const char *name, hydra_bytes request) {
    uint64_t result = (uint64_t)host_call((int32_t)(uintptr_t)name, (int32_t)strlen(name),
                                         (int32_t)(uintptr_t)request.data, (int32_t)request.len);
    return (hydra_bytes){(char *)(uintptr_t)(uint32_t)(result >> 32), (uint32_t)result};
}
hydra_bytes hydra_json(const char *json) { return (hydra_bytes){json, (uint32_t)strlen(json)}; }
int hydra_method_is(hydra_bytes method, const char *name) {
    return strlen(name) == method.len && !memcmp(method.data, name, method.len);
}
