# Phase 6: Multi-Language Bindings

## TL;DR

Add stable C/C++ and Python bindings to expose the public API across languages.

## Scope

- C ABI layer for core types and allocators
- C++ wrapper (RAII) for channel and buffer APIs
- Python bindings for high-level workflows

## Deliverables

- `lava_flow_sys` C ABI crate
- C++ header + minimal examples
- Python module with packaging and CI checks

## Example (API Shape)

```c
lava_flow_context_t* ctx = lava_flow_context_new();
lava_flow_buffer_t* buf = lava_flow_alloc_gpu(ctx, 1000000);
```

## Related Docs

- [ADR-011 Multi-Language Bindings](../adr/011-multi-language-bindings.md)
- [Architecture](../spec/architecture.md)

## External References

- [Rust FFI Guide](https://doc.rust-lang.org/nomicon/ffi.html)
- [cbindgen](https://github.com/mozilla/cbindgen)
- [pyo3](https://pyo3.rs/)
