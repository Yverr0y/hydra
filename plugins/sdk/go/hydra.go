// Package hydra implements Hydra's JSON Wasm guest ABI. Build with Go 1.24+.
package hydra

import (
	"encoding/json"
	"fmt"
	"runtime"
	"unsafe"
)

// Handler processes a hook and returns its JSON-compatible reply.
type Handler func(string, json.RawMessage) (any, error)

var allocations = map[uint32][]byte{}

// Alloc retains a buffer until this fresh guest instance is destroyed.
func Alloc(n uint32) uint32 {
	if n > 16*1024*1024 {
		panic("Hydra payload too large")
	}
	b := make([]byte, max(n, 1))
	p := uint32(uintptr(unsafe.Pointer(&b[0])))
	allocations[p] = b
	return p
}
func bytesAt(p, n uint32) []byte { return unsafe.Slice((*byte)(unsafe.Pointer(uintptr(p))), n) }

// Call dispatches one host entry and converts errors to the Hydra error envelope.
func Call(mp, ml, rp, rl uint32, handler Handler) uint64 {
	result, err := handler(string(bytesAt(mp, ml)), json.RawMessage(bytesAt(rp, rl)))
	delete(allocations, mp)
	delete(allocations, rp)
	if err != nil {
		result = map[string]any{"error": map[string]any{"code": "internal", "message": err.Error()}}
	}
	b, err := json.Marshal(result)
	if err != nil {
		b = []byte(`{"error":{"code":"invalid_reply","message":"reply is not JSON"}}`)
	}
	p := Alloc(uint32(len(b)))
	copy(allocations[p], b)
	return uint64(p)<<32 | uint64(len(b))
}

//go:wasmimport hydra hydra_host_call
func hostCall(np, nl, rp, rl uint32) uint64

// Host calls a granted capability and decodes its reply, including host errors.
func Host(name string, request any, reply any) error {
	b, err := json.Marshal(request)
	if err != nil {
		return err
	}
	n := []byte(name)
	if len(n) == 0 {
		return fmt.Errorf("empty host method")
	}
	result := hostCall(uint32(uintptr(unsafe.Pointer(&n[0]))), uint32(len(n)), uint32(uintptr(unsafe.Pointer(&b[0]))), uint32(len(b)))
	runtime.KeepAlive(n)
	runtime.KeepAlive(b)
	p, size := uint32(result>>32), uint32(result)
	data := bytesAt(p, size)
	defer delete(allocations, p)
	var envelope struct {
		Error *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(data, &envelope); err != nil {
		return err
	}
	if envelope.Error != nil {
		return fmt.Errorf("%s: %s", envelope.Error.Code, envelope.Error.Message)
	}
	return json.Unmarshal(data, reply)
}
