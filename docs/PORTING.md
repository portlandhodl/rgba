# Porting guide: mGBA C → rgba Rust

All core code is a **mechanical, behavior-preserving port** of the mGBA C
sources vendored in `./mgba/`. Follow these rules exactly so modules compose.

## Naming

- Functions: C name lowercased, underscores kept (`GBIOWrite` → `io_write`,
  `_GBVideoDummyRenderer` → `dummy_renderer`). Types keep PascalCase
  (`GBMemory` → `Memory`).
- Every file starts with the MPL header and a `// Ported from
  mgba/<path>` comment.
- C enum constants keep names: `GB_REG_IF` → `GB_REG_IF` (associated `pub
  const` in `io.rs`).

## Ownership/borrow model

The console struct (`Gb` / `Gba`) owns **everything** (cpu, memory, video,
audio, timer, sio, timing) as direct fields — no Box, no Rc. All cross-module
operations are `impl Gb { fn ...(&mut self) }` methods, exactly like the C
functions that take `struct GB*`; sub-structs never hold back-pointers.

The mGBA idiom `timer.p = gb;`-style back-pointers are replaced by calling
`gb.<subsystem_method>()` from `impl Gb`.

`struct SM83Memory`/`struct ARMCore` function pointers (`load8`, `store8`,
`cpuLoad8`, `setActiveRegion`, ...) become plain methods on the console:
`gb.load8(addr)`, `gb.store8(addr, v)`, `gb.cpu_load8(addr)`, ...

## Timing

All scheduled callbacks are ids in a per-console `EventId` enum (see
`gb.rs`). The scheduler is `rgba_core::timing::Timing`. C:

```c
mTimingSchedule(&gb->timing, &gb->memory.dmaEvent, delay);
```

Rust:

```rust
gb.timing.schedule(EventId::Dma.into(), PRIORITY_DMA, delay);
```

Event dispatch happens in `Gb::process_event(EventId, cycles_late)`, called
from `Timing::tick`'s callback inside `process_events()` (the port of
`GBProcessEvents`). Because `tick` needs `&mut Timing` and `&mut Gb`, the
loop body does `let mut timing = std::mem::take(&mut gb.timing)` /
`gb.timing = timing` around it (Timing is `Default`).

## Integer semantics

- C `uintN_t` overflow → `wrapping_*` in Rust.
- C `u8 = u16 & 0xFF` truncation → `as u8` etc.
- C bitfield flags (`DECL_BITFIELD`) → plain `u8/u16` with bit accessor
  methods or masks.
- `int32_t cycles` arithmetic is signed; keep `i32` and don't "improve" it.
  When a C `uint32_t when` wraps around, use `wrapping_add/sub`.

## No speculative fixes

If the C looks odd (e.g. `sramBank1` booleans, "GROSS" comments), port it
as-is, including the comment. Do not fix suspected upstream bugs.
