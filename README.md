# rgba

A Game Boy Advance emulator for the modern Rust ecosystem — cycle-accurate
ARM7TDMI, the full mGBA feature set, and a debugger, with a clean library /
frontend split.

Play GBA and GB/GBC games, with the extras developers and speedrunners
actually want: breakpoints and watchpoints, a GDB stub, per-frame video
logging (mVL), cheats, rewind, savestates compatible with mGBA, and accurate
original-hardware peripherals (RTC, rumble, solar sensor, e-Reader, link
cables).

## Quickstart

```sh
cargo build --release
./target/release/rgba path/to/game.gba
```

Also runs `.gb`/`.gbc` ROMs. Useful flags:

```
rgba game.gba --bios path/to/gba_bios.bin   # real BIOS (HLE BIOS otherwise)
rgba game.gb  --gb-model cgb                # dmg | cgb | sgb | mgb | sgb2 | scgb
rgba game.gba --debug                       # CLI debugger (stdin/stdout), F3 breaks in
rgba game.gba --cheats codes.txt            # one code per line (GameShark/CB/PARv3)
rgba game.gba --patch romhack.ips           # .ips, .ups, .bps rom patches
rgba game.gba --rewind                      # hold R mid-game to rewind
rgba game.gba --video-log out.mvl           # record the frame stream for replay/diff
rgba game.gba --gbp                         # Game Boy Player detection screen check
```

Default keyboard: **Z** = A, **X** = B, **Esc**/Return = Select/Start, arrows
= d-pad, **A**/**S** = L/R. **F1** save / **F2** load savestate.

## Architecture

Three core crates, mirroring how the emulator is layered, plus the executable:

| Crate | Contents |
|---|---|
| `rgba-core` | Timing/event scheduler (`Timing`), savestate codec, cheat device core, memory search engine, rewind, video-logger format (mVL), patch apply, ELF/IPS/UPS helpers, ring buffer, windowed-sinc audio resampler |
| `rgba-gb` | Game Boy/GBC — SM83 CPU, MBCs, PPU incl. SGB borders, APU (2 pulse + wave + noise), SIO incl. link-cable lockstep and the GB Printer |
| `rgba-gba` | Game Boy Advance — ARM7TDMI (ARM + Thumb), memory bus with waitstates + prefetch, software PPU (modes 0–5, sprites, windows, blending, mosaic), DMA, timers, SIO (lockstep, Dolphin link, Game Boy Player, battlechip), BIOS HLE, GPIO carts (RTC, rumble, gyro/tilt, light), savedata (SRAM/Flash512/Flash1M/EEPROM), e-Reader, unlicensed carts |
| `rgba-debugger` | Full mGBA-style debugger: breakpoints (hardware + software via BKPT patching), read/write/change watchpoints, condition expression parser, stack traces, symbols (map/ELF formats) |
| `rgba` | The egui + cpal frontend (this crate's `main.rs`) |

The `Core` trait is the seam between frontend and consoles; the debugger sits
on top as a pluggable `DebuggerModule` list (CLI today; the GDB stub is a
module).

State files (savestates) use the mGBA formats byte-for-byte, so saves are
interoperable with mGBA itself.

## Building from source

Requires a stable Rust toolchain. Audio uses cpal (ALSA on Linux — on
Debian/Ubuntu install `libasound2-dev`).

```sh
cargo build --release         # binary: target/release/rgba
cargo test --workspace        # full test suite (~240 integration/unit tests)
cargo test -p rgba-gba        # just the GBA core
```

## Ubuntu package

```sh
packaging/build-deb.sh        # builds target/deb/rgba_<version>_amd64.deb
sudo dpkg -i target/deb/rgba_*_amd64.deb
```

The package installs the `rgba` binary, an application-menu entry (with the
glacier-blue GBA icon) and a man page. Tagged releases (`git tag v0.1.0 &&
git push --tags`) build the `.deb` and a standalone tarball automatically via
the release workflow and attach them to the GitHub release.

## Accuracy

Cycle-count and bus behavior mirror mGBA's: instruction timing tables, SIO
transfer lengths, waitstate/prefetch modeling on the ROM bus, and the GB's
per-cycle PPU/double-speed state machines. A frame-perfect execution is the
goal; where the ports trade off purely for performance (mTileCache-style
caches) the output is bit-identical by construction and the delta is
documented in `AGENTS.md`.

## Debugging

`--debug` starts the CLI debugger on stdio. Inside: `break`, `watch`,
`watch-range/c`, `trace`, `disassemble`, `status`, `x/4`, `p/x`, `backtrace`,
`stack trace ...`. See `mgba.io`'s debugger docs — the command table is the
same one mGBA ships. The access logger and GDB stub exist behind the same
module API.

## Acknowledgements

`rgba` is a Rust re-implementation of [mGBA](https://mgba.io/) by Jeffrey
Pfau (endrift) and contributors; the C code driven these ports was mGBA
0.11.0-dev. mGBA is freely available under the Mozilla Public License 2.0.

Graphics/fonts/game assets in tests are made by the project, drop-in
placeholder test ROMs assembled by hand.

## License

Mozilla Public License 2.0 (`LICENSE`). Every ported source file marks its
mGBA origin with a `// Ported from mgba/...` comment at the top.
