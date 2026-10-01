# rgba — Rust port of mGBA

`rgba` is a Rust re-implementation of the [mGBA](https://mgba.io/) emulator
(source snapshot 0.11.0-dev, vendored in `./mgba/` for reference). It targets the
GBA (ARM7TDMI) and GB (SM83) consoles, mirroring mGBA's module structure and,
where it matters, its timing/cycle semantics.

## License

mGBA is licensed MPL-2.0; this port is a derived work and is also MPL-2.0.
Each ported source file carries the MPL header and a comment naming the original
mGBA file it was ported from.

## Layout (mirrors mGBA)

| Rust path | mGBA source it ports |
|---|---|
| `crates/rgba-core/src/timing.rs` | `src/core/timing.c`, `include/mgba/core/timing.h` |
| `crates/rgba-core/src/core.rs` | `include/mgba/core/core.h` (the `mCore` vtable → `Core` trait) |
| `crates/rgba-core/src/cheats.rs` | `src/core/cheats.c` |
| `crates/rgba-core/src/blip.rs` | `src/third-party/blip_buf/blip_buf.c` |
| `crates/rgba-core/src/ring.rs` | `src/util/ring-fifo.c` / `circle-buffer.c` |
| `crates/rgba-core/src/serialize.rs` | `src/core/serialize.c` |
| `crates/rgba-core/src/log.rs` | `src/core/log.c` (subset) |
| `crates/rgba-gb/src/cpu/*` | `src/sm83/*` |
| `crates/rgba-gb/src/gb.rs` | `src/gb/gb.c` |
| `crates/rgba-gb/src/memory.rs` | `src/gb/memory.c` |
| `crates/rgba-gb/src/io.rs` | `src/gb/io.c` |
| `crates/rgba-gb/src/video.rs` | `src/gb/video.c` (+ `src/gb/renderers/software.c`) |
| `crates/rgba-gb/src/audio.rs` | `src/gb/audio.c` |
| `crates/rgba-gb/src/timer.rs` | `src/gb/timer.c` |
| `crates/rgba-gb/src/sio.rs` | `src/gb/sio*.c` |
| `crates/rgba-gb/src/mbc/*` | `src/gb/mbc/*` |
| `crates/rgba-gb/src/serialize.rs` | `src/gb/serialize.c` |
| `crates/rgba-gb/src/cheats.rs` | `src/gb/cheats.c` |
| `crates/rgba-gba/src/cpu/*` | `src/arm/*` |
| `crates/rgba-gba/src/gba.rs` | `src/gba/gba.c` |
| `crates/rgba-gba/src/memory.rs` | `src/gba/memory.c` |
| `crates/rgba-gba/src/io.rs` | `src/gba/io.c` |
| `crates/rgba-gba/src/dma.rs` | `src/gba/dma.c` |
| `crates/rgba-gba/src/video.rs` | `src/gba/video.c` (+ `src/gba/renderers/*` software renderer) |
| `crates/rgba-gba/src/audio.rs` | `src/gba/audio.c` |
| `crates/rgba-gba/src/timers.rs` | `src/gba/timer.c` |
| `crates/rgba-gba/src/sio.rs` | `src/gba/sio*.c` |
| `crates/rgba-gba/src/savedata.rs` | `src/gba/savedata.c` |
| `crates/rgba-gba/src/cart/mod.rs` | `src/gba/cart/gpio.c` (GPIO: RTC, rumble, light, gyro/tilt) |
| `crates/rgba-gba/src/cart/ereader.rs` | `src/gba/cart/ereader.c` (register file, dotcode strip gen, serial state machine; no image-scan/frontend bits) |
| `crates/rgba-gba/src/cart/unlicensed.rs` | `src/gba/cart/unlicensed.c`, `src/gba/cart/vfame.c` |
| `crates/rgba-gba/src/cart/matrix.rs` | `src/gba/cart/matrix.c` |
| `crates/rgba-gba/src/bios.rs` | `src/gba/bios.c`, `src/gba/hle-bios.*` |
| `crates/rgba-gba/src/serialize.rs` | `src/gba/serialize.c` |
| `crates/rgba-gba/src/cheats.rs` | `src/gba/cheats.c`, `src/gba/cheats/{gameshark,parv3,codebreaker}.c` (GBACheatHook breakpoints not ported) |
| `crates/rgba-debugger/src/debugger.rs` | `src/debugger/debugger.c` (`mDebugger`, `mDebuggerModule`, platform glue; platform vtable → `DebugConsole` trait + inherent `Gb`/`Gba` methods) |
| `crates/rgba-debugger/src/access_logger.rs` | `src/debugger/access-logger.c`, `include/mgba/internal/debugger/access-logger.h` (`mDebuggerAccessLogger` module, mAL\1 region/flag tables; recording is driven by the consoles' memory-shim hooks into `AccessLoggerCore` held by `GbDebugger`/`GbaDebugger`, instead of C's per-region watchpoints) |
| `crates/rgba-debugger/src/gdb.rs` | `src/debugger/gdb-stub.c` (GDB remote serial stub; TCP via `std::net`, nonblocking; test injection via `feed_input`/`take_output`) |
| `crates/rgba-debugger/src/cli.rs` | `src/debugger/cli-debugger.c` (command table, dv arg parser, tab completion; `CliBackend` backend vtable, `CallbackBackend` for tests; ARM platform command table from `src/arm/debugger/cli-debugger.c` gated on `DebugConsole::dbg_cli_platform()`) |
| `crates/rgba-debugger/src/parser.rs` | `src/debugger/parser.c` (breakpoint/watchpoint condition expressions) |
| `crates/rgba-debugger/src/symbols.rs` | `src/debugger/symbols.c` |
| `crates/rgba-debugger/src/stack_trace.rs` | `src/debugger/stack-trace.c` |
| `crates/rgba-gb/src/cpu/decoder.rs` | `src/sm83/decoder.c` (decode + disassemble, debugger use) |
| `crates/rgba-gba/src/arm/decoder.rs` | `src/arm/decoder.c`, `decoder-arm.c`, `decoder-thumb.c` (debuggers/trace only) |
| `crates/rgba-gb/src/debugger.rs` | `src/sm83/debugger/*`, `src/gb/debugger/debugger.c` |
| `crates/rgba-gba/src/debugger.rs` | `src/arm/debugger/*` + GBA glue from `src/gba/gba.c`/`core.c` |
| `crates/rgba/src/*` | SDL2 frontend (like `src/platform/sdl/sdl-main.c`) |

## Conventions

- Timing: all console code drives `rgba_core::timing::Timing` exactly like
  `mTiming`; callbacks are dispatched per-console via `EventId` enums (Rust has no
  void*; the pattern is: `Timing::tick` returns/dispatches event ids, the console
  matches on them). Insertion order semantics (sorted by `(when, priority)`,
  stable) must be preserved.
- Memory access widths behave like the C: helpers `load8/16/32`, `store8/16/32`.
- Keep identifiers close to the C names (`io_read` ↔ `GBIORead`, etc.) so the C
  source remains a readable reference.
- No `unsafe` unless a measurable perf win demands it and it is justified in a comment.
- Build everything with `cargo build --workspace`; run tests with `cargo test --workspace`.
