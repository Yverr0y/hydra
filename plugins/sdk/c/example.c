#include "hydra.h"
#include <stdlib.h>

hydra_bytes hydra_dispatch(hydra_bytes method, hydra_bytes request) {
    (void)request;
    if (hydra_method_is(method, "check")) return hydra_json("{}");
    if (hydra_method_is(method, "hooks"))
        return hydra_json("{\"api\":1,\"hooks\":[\"resolve\",\"check\"]}");
    if (hydra_method_is(method, "resolve")) {
        hydra_bytes settings = hydra_host_call_json("settings", hydra_json("{}"));
        free((void *)settings.data);
        return hydra_json("{\"plan\":{\"id\":\"file\",\"title\":\"C example\",\"tracks\":[{\"id\":\"file\",\"kind\":\"file\",\"sources\":[{\"url\":\"https://example.com/file\"}]}]}}");
    }
    return hydra_json("{\"error\":{\"code\":\"unsupported\",\"message\":\"unknown method\"}}");
}
