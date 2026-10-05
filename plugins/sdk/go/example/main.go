package main

import (
	"encoding/json"
	"fmt"
	hydra "hydra.local/sdk"
)

//go:wasmexport hydra_api
func api() uint32 { return 1 }

//go:wasmexport hydra_alloc
func alloc(n uint32) uint32 { return hydra.Alloc(n) }

//go:wasmexport hydra_call
func call(mp, ml, rp, rl uint32) uint64 { return hydra.Call(mp, ml, rp, rl, dispatch) }

func dispatch(method string, request json.RawMessage) (any, error) {
	switch method {
	case "hooks":
		return map[string]any{"api": 1, "hooks": []string{"resolve", "check"}}, nil
	case "check":
		return map[string]any{}, nil
	case "resolve":
		var settings map[string]any
		if err := hydra.Host("settings", map[string]any{}, &settings); err != nil {
			return nil, err
		}
		var req struct {
			URL string `json:"url"`
		}
		if err := json.Unmarshal(request, &req); err != nil {
			return nil, err
		}
		return map[string]any{"plan": map[string]any{"id": "file", "title": "Go example", "tracks": []any{
			map[string]any{"id": "file", "kind": "file", "sources": []any{map[string]any{"url": req.URL}}},
		}}}, nil
	default:
		return nil, fmt.Errorf("unsupported method: %s", method)
	}
}
func main() {}
