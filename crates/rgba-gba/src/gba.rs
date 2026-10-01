// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/include/mgba/internal/gba/gba.h and mgba/src/gba/gba.c.

use rgba_core::cheats::CheatDevice;
use rgba_core::timing::Timing;
use rgba_core::{mlog, Level};

use crate::arm::{ArmCore, PrivilegeMode, ARM_SP};
use crate::audio::Audio;
use crate::cart::CartridgeHardware;
use crate::dma::Dma;
use crate::io::*;
use crate::memory::{ActiveMemoryRegion, Memory};
use crate::savedata::Savedata;
use crate::sio::Sio;
use crate::timers::Timer;
use crate::video::{Video, VIDEO_HORIZONTAL_LENGTH, VIDEO_TOTAL_LENGTH};

use crate::cart::{EReader, Matrix, UnlCart};

pub const GBA_ARM7TDMI_FREQUENCY: u32 = 0x1000000;

/// interface.h GBA_IDLE_LOOP_NONE
pub const GBA_IDLE_LOOP_NONE: u32 = 0xFFFF_FFFF;

pub const GBA_SP_BASE_SYSTEM: i32 = 0x03007F00;
pub const GBA_SP_BASE_IRQ: i32 = 0x03007FA0;
pub const GBA_SP_BASE_SUPERVISOR: i32 = 0x03007FE0;

// Hardware device flags (interface.h GBAHardwareDevice)
pub const HW_NO_OVERRIDE: u32 = 0x8000;
pub const HW_NONE: u32 = 0;
pub const HW_RTC: u32 = 1;
pub const HW_RUMBLE: u32 = 2;
pub const HW_LIGHT_SENSOR: u32 = 4;
pub const HW_GYRO: u32 = 8;
pub const HW_TILT: u32 = 16;
pub const HW_GB_PLAYER: u32 = 32;
pub const HW_GB_PLAYER_DETECTION: u32 = 64;
pub const HW_EREADER: u32 = 128;
pub const HW_GPIO: u32 = HW_RTC | HW_RUMBLE | HW_LIGHT_SENSOR | HW_GYRO | HW_TILT;

// IRQ bits (gba.h enum GBAIRQ)
pub const GBA_IRQ_VBLANK: u16 = 0;
pub const GBA_IRQ_HBLANK: u16 = 1;
pub const GBA_IRQ_VCOUNTER: u16 = 2;
pub const GBA_IRQ_TIMER0: u16 = 3;
pub const GBA_IRQ_DMA3: u16 = 0xB;
pub const GBA_IRQ_KEYPAD: u16 = 0xC;
pub const GBA_IRQ_GAMEPAK: u16 = 0xD;

pub const DMA_COUNT: usize = 4;

/// Video/audio/timer/sio/dma event ids on the GBA scheduler.
/// Order/priorities mirror mGBA's gba.c + friends:
/// video.event priority 0x08, dmaEvent 0x40... (see each module).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum EventId {
    Video = 0,
    Dma = 13,
    AudioSample = 14,
    Timer0 = 15,
    Timer1 = 16,
    Timer2 = 17,
    Timer3 = 18,
    Sio = 19,
    IrqEvent = 20,
    /// "GBA SIO Lockstep" (gba/sio/lockstep.c, priority 0x80)
    SioLockstep = 21,
    /// "GBA SIO Lockstep"/dolphin (gba/sio/dolphin.c, priority 0x80)
    SioDolphin = 22,
    /// "GBA Unlicensed Multicart Settle" (gba/cart/unlicensed.c, priority 0x71)
    UnlCartSettle = 23,
}

impl EventId {
    pub fn priority(self) -> u32 {
        match self {
            EventId::Video => 8,
            EventId::Dma => 0x40,
            EventId::AudioSample => 0x18,
            EventId::Timer0 | EventId::Timer1 | EventId::Timer2 | EventId::Timer3 => 0x20,
            EventId::Sio => 0x80,
            EventId::SioLockstep | EventId::SioDolphin => 0x80,
            EventId::UnlCartSettle => 0x71,
            EventId::IrqEvent => 0,
        }
    }
}

impl From<EventId> for u32 {
    fn from(e: EventId) -> u32 {
        e as u32
    }
}


pub struct Gba {
    pub hw: CartridgeHardware,
    pub cpu: ArmCore,
    pub memory: Memory,
    /// GBAMemory.ereader (cart/ereader.rs)
    pub ereader: EReader,
    /// GBAMemory.matrix (cart/matrix.rs)
    pub matrix: Matrix,
    /// GBAMemory.unl (cart/unlicensed.rs)
    pub unl: UnlCart,
    pub video: Video,
    /// The mVL video-log recorder endpoint (mgba/src/gba/core.c's
    /// `logContext`+`vlProxy` pair, minus the proxy renderer shim —
    /// GBAVideoProxyRenderer*'s logger halves are inlined into the concrete
    /// renderer entry points in video/renderers.rs). Set by
    /// `start_video_log`, torn down by `end_video_log`.
    pub video_logger: Option<Box<crate::video_log::GbaVideoLog>>,
    pub audio: Audio,
    pub sio: Sio,
    pub savedata: Savedata,
    /// The cheat device (mCheatDevice hung off cpu->components in the C).
    /// GBA-side per-set decoder state lives in `gba_cheat_sets`, kept in
    /// lockstep with `cheats.cheats` via `cheat_add_set`/`cheat_remove_set`.
    pub cheats: CheatDevice,
    /// struct GBACheatSet, one per set in `cheats.cheats` (cart/…-style
    /// per-console half of the shared CheatSet).
    pub gba_cheat_sets: Vec<crate::cheats::GbaCheatSet>,
    pub timers: [Timer; DMA_COUNT],
    pub dma: [Dma; DMA_COUNT],
    pub dma_event_id: u32, // which channel is inside the Dma event callback
    pub active_dma: i32,

    pub timing: Timing,

    pub bus: u32,
    pub performing_dma: i32,
    pub dma_pc: u32,

    pub bios_checksum: u32,
    /// bios.c: HLE-cycle bookkeeping for stall'd SWIs (C uint32_t; values
    /// are always small positives, kept i32 for the cycle math below).
    pub bios_stall: i32,
    pub is_pristine: bool,
    pub pristine_rom_size: usize,
    pub yanked_rom_size: usize,
    pub rom_crc32: u32,
    pub has_bios: bool,

    pub keys_active: u16,
    /// Core-level mirror of the frontend's gba.forceGbp setting.
    pub force_gbp: bool,
    pub keys_last: u16,
    pub allow_opposing_directions: bool,

    /// The debugger (mDebugger + ARMDebugger platform).
    pub debugger: Option<Box<crate::debugger::GbaDebugger>>,
    /// Memory-shim installed flag (C: ARM memory shim function pointers).
    pub dbg_watchpoints_active: bool,

    pub cpu_blocked: bool,
    pub early_exit: bool,
    pub halt_pending: bool,

    pub idle_optimization: i32,
    pub idle_loop: u32,
    pub idle_detection_step: i32,
    pub idle_detection_failures: u32,
    pub tainted_registers: [bool; 16],
    pub cached_registers: [i32; 16],
    pub last_jump: u32,

    pub vba_bug_compat: bool,
    pub hard_crash: bool,

    pub debug: bool,
    pub debug_string: Vec<u8>,

    /// Savedata persistence handled by the frontend via savedata()/load.
    pub frame_counter_bug: bool,

    /// Host-provided input, in mCore key order (A,B,Select,Start,Right,Left,Up,Down,R,L).
    pub keys: u32,
    /// RTC clock hook (unix seconds); default is the host wall clock.
    pub rtc_time_fn: fn() -> i64,
    pub rumble_active: bool,
    pub light_sensor_level: u8,
}

pub const GBA_MEMORY_MAGIC_UNUSED: u32 = 0;

fn default_rtc_time_gba() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}



impl Gba {
    pub fn new() -> Box<Gba> {
        let mut gba = Box::new(Gba {
            hw: CartridgeHardware::new(),
            cpu: ArmCore::new(),
            memory: Memory::new(),
            ereader: EReader::new(),
            matrix: Matrix::new(),
            unl: UnlCart::new(),
            video: Video::new(),
            video_logger: None,
            audio: Audio::new(),
            sio: Sio::new(),
            savedata: Savedata::new(),
            cheats: crate::cheats::gba_cheat_device_create(),
            gba_cheat_sets: Vec::new(),
            timers: [Timer::new(), Timer::new(), Timer::new(), Timer::new()],
            dma: [Dma::new(), Dma::new(), Dma::new(), Dma::new()],
            dma_event_id: 0,
            active_dma: -1,
            timing: Timing::new(),
            bus: 0,
            performing_dma: 0,
            dma_pc: 0,
            bios_checksum: 0,
            bios_stall: 0,
            is_pristine: false,
            pristine_rom_size: 0,
            yanked_rom_size: 0,
            rom_crc32: 0,
            has_bios: false,
            keys_active: 0,
            force_gbp: false,
            keys_last: 0,
            allow_opposing_directions: false,
            debugger: None,
            dbg_watchpoints_active: false,
            cpu_blocked: false,
            early_exit: false,
            halt_pending: false,
            idle_optimization: crate::memory::IDLE_LOOP_REMOVE,
            idle_loop: GBA_IDLE_LOOP_NONE,
            idle_detection_step: 0,
            idle_detection_failures: 0,
            tainted_registers: [false; 16],
            cached_registers: [0; 16],
            last_jump: 0,
            vba_bug_compat: false,
            hard_crash: false,
            debug: false,
            debug_string: vec![0; 0x100],
            frame_counter_bug: false,
            keys: 0,
            rtc_time_fn: default_rtc_time_gba,
            rumble_active: false,
            light_sensor_level: 0xFF,
        });
        // GBAMemoryInit: gba->memory.bios = (uint32_t*) hleBios
        crate::bios::bios_install(&mut gba);
        gba
    }

    /// Convenience scheduler wrappers (same discipline as the GB core).
    pub fn schedule(&mut self, id: EventId, when: i32) {
        self.timing.set_relative_cycles(self.cpu.cycles);
        self.timing.schedule(id.into(), id.priority(), when);
        self.cpu.next_event = self.timing.next_event_cycles_owned();
    }
    pub fn deschedule(&mut self, id: EventId) {
        self.timing.deschedule(id.into());
    }
    pub fn is_scheduled(&self, id: EventId) -> bool {
        self.timing.is_scheduled(id.into())
    }
    pub fn until(&mut self, id: EventId) -> i32 {
        self.timing.set_relative_cycles(self.cpu.cycles);
        self.timing.until(id.into())
    }
    pub fn current_time(&self) -> i32 {
        self.timing.master_cycles as i32 + self.cpu.cycles
    }
    pub fn with_timing<R>(&mut self, f: impl FnOnce(&mut Self, &mut Timing) -> R) -> R {
        let mut timing = std::mem::take(&mut self.timing);
        timing.set_relative_cycles(self.cpu.cycles);
        let r = f(self, &mut timing);
        self.timing = timing;
        self.cpu.next_event = self.timing.next_event_cycles_owned();
        r
    }

    pub fn bios_none(&self) -> bool {
        !self.has_bios
    }

    /// GBATestIRQ (gba.c)
    pub fn test_irq(&mut self, cycles_late: u32) {
        self.test_irq_late(cycles_late);
    }

    /// GBAHalt
    pub fn halt(&mut self) {
        self.cpu.halted = 1;
        // Halt the CPU until an interrupt wakes it
        self.early_exit = true;
    }

    /// GBAInterrupt — signals "current frame / lockstep boundary reached";
    /// used by the video event when the frame ends. Moves the pending event
    /// queue aside (`mTimingInterrupt`) so anything dispatching next runs
    /// after the frontend has had a shot at the frame.
    pub fn gba_interrupt(&mut self) {
        self.early_exit = true;
        self.timing.interrupt();
    }

    /// GBAStop
    pub fn stop(&mut self) {
        // The stub: stop is not yet meaningfully emulated in mGBA either.
        self.halt();
    }

    /// GBARaiseIRQ
    pub fn raise_irq(&mut self, irq: u16) {
        self.memory.io[(GBA_REG_IF >> 1) as usize] |= 1 << irq;
        self.test_irq_late(0);
    }

    /// GBATestIRQ
    pub fn test_irq_late(&mut self, _cycles_late: u32) {
        if self.memory.io[(GBA_REG_IE >> 1) as usize] & self.memory.io[(GBA_REG_IF >> 1) as usize]
            != 0
        {
            if !self.is_scheduled(EventId::IrqEvent) {
                self.schedule(EventId::IrqEvent, 7);
            }
        }
    }

    /// _triggerIRQ
    pub fn trigger_irq_event(&mut self) {
        self.cpu.halted = 0;
        if self.memory.io[(GBA_REG_IE >> 1) as usize] & self.memory.io[(GBA_REG_IF >> 1) as usize]
            == 0
        {
            return;
        }
        if self.memory.io[(GBA_REG_IME >> 1) as usize] != 0 && !self.cpu.cpsr.i() {
            self.arm_raise_irq();
        }
    }

    /// GBATestKeypadIRQ
    pub fn test_keypad_irq(&mut self) {
        let keycnt = self.memory.io[(GBA_REG_KEYCNT >> 1) as usize];
        let keypad_active = (self.keys_active & !(self.keys_last)) as u32;
        self.keys_last = self.keys_active;
        if keycnt & 0x4000 == 0 {
            return;
        }
        let and = (keycnt & 0x8000) != 0;
        let mask = (keycnt & 0x3FF) as u32;
        let keys = !self.keys_active as u32 & 0x3FF;
        let cond = if and {
            keys & mask == mask
        } else {
            keys & mask != 0
        };
        if cond || keypad_active & mask != 0 {
            // raise keypad IRQ
            self.memory.io[(GBA_REG_IF >> 1) as usize] |= 1 << GBA_IRQ_KEYPAD;
            self.test_irq(0);
        }
    }

    /// GBAProcessEvents (gba.c). `cycles are tracked relative to next_event`.
    pub fn process_events(&mut self) {
        // Mirror the C loop exactly: recapture `cycles` fresh every tick
        // while handlers may push cycles back onto cpu.cycles.
        let mut timing = std::mem::take(&mut self.timing);
        let mut next_event = self.cpu.next_event;
                while self.cpu.cycles >= next_event {
            self.cpu.next_event = i32::MAX;
            next_event = 0;
            loop {
                let cycles = self.cpu.cycles;
                self.cpu.cycles = 0;
                let to_tick = if cycles < next_event {
                    next_event
                } else {
                    cycles
                };
                next_event = timing.tick(to_tick, self, &mut |gba: &mut Gba, timing, id, late| {
                    gba.process_event(timing, id, late);
                });
                if !(self.cpu_blocked && !self.early_exit) {
                    break;
                }
            }
            self.cpu.next_event = next_event;
            if self.cpu.halted != 0 {
                self.cpu.cycles = next_event;
                if self.memory.io[(GBA_REG_IME >> 1) as usize] == 0
                    || self.memory.io[(GBA_REG_IE >> 1) as usize] == 0
                {
                    break;
                }
            }
            if self.early_exit {
                break;
            }
        }
        self.timing = timing;
        self.early_exit = false;
        if self.cpu_blocked {
            self.cpu.cycles = self.cpu.next_event;
        }
    }

    fn process_event(&mut self, timing: &mut Timing, id: u32, cycles_late: i32) {
        match id {
            x if x == EventId::Video as u32 => self.video_event(timing, cycles_late as u32),
            x if x == EventId::Dma as u32 => {
                let dma = self.dma_event_id as usize;
                self.dma_event(dma, timing, cycles_late);
            }
            x if x == EventId::AudioSample as u32 => self.audio_sample_event(timing, cycles_late),
            x if x == EventId::Timer0 as u32 => self.timer_fired(0, timing, cycles_late as u32),
            x if x == EventId::Timer1 as u32 => self.timer_fired(1, timing, cycles_late as u32),
            x if x == EventId::Timer2 as u32 => self.timer_fired(2, timing, cycles_late as u32),
            x if x == EventId::Timer3 as u32 => self.timer_fired(3, timing, cycles_late as u32),
            x if x == EventId::UnlCartSettle as u32 => self.multicart_settle(cycles_late as u32),
            x if x == EventId::Sio as u32 => self.sio_complete_event(timing, cycles_late as u32),
            x if x == EventId::SioDolphin as u32 => {
                // GBASIODolphinProcessEvents
                if let Some(mut dol) = self.sio.dolphin.take() {
                    dol.process_events(self, cycles_late);
                    self.sio.dolphin = Some(dol);
                }
            }
            x if x == EventId::SioLockstep as u32 => {
                self.sio_lockstep_event(timing, cycles_late as u32)
            }
            x if x == EventId::IrqEvent as u32 => self.trigger_irq_event(),
            _ => unreachable!(),
        }
    }

    /// irqh wiring -----------------------------------------------------------

    pub fn irq_reset(&mut self) {
        self.gba_reset();
    }
    pub fn irq_read_cpsr(&mut self) {}
    /// irqh.swi16 → GBASwi16 (gba/bios.c)
    pub fn swi16(&mut self, immediate: i32) {
        crate::bios::bios_syscall_swi(self, immediate);
    }
    /// irqh.swi32 → GBASwi32
    pub fn swi32(&mut self, immediate: i32) {
        crate::bios::bios_syscall_swi32(self, immediate);
    }
    /// Helper for the view* raw reads (C: GBAView* bypass the memory shim).
    pub(crate) fn debugger_view_guard(&mut self, set: bool) -> bool {
        if let Some(dbg) = self.debugger.as_mut() {
            let old = dbg.in_view_read;
            dbg.in_view_read = set;
            old
        } else {
            false
        }
    }

    /// GBABreakpoint (BKPT instruction hook)
    pub fn bkpt16(&mut self, immediate: i32) {
        self.gba_breakpoint(immediate);
    }
    pub fn bkpt32(&mut self, immediate: i32) {
        self.gba_breakpoint(immediate);
    }
    /// GBABreakpoint: the immediate picks the BKPT "component" (debugger=0,
    /// cheats=1; see debugger.rs). Anything else falls through to
    /// ARMRaiseUndefined, as in the C.
    fn gba_breakpoint(&mut self, immediate: i32) {
        match immediate {
            0 => {
                if self.debugger.is_some() {
                    use rgba_debugger::debugger::*;
                    let pc_addr = self.dbg_pc_address();
                    let info = DebuggerEntryInfo {
                        address: pc_addr,
                        type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
                            opcode: 0,
                            break_type: BreakpointType::Software,
                        })),
                        point_id: -1,
                        ..Default::default()
                    };
                    self.debugger_enter(DebuggerEntryReason::Breakpoint, info);
                    return;
                }
            }
            1 => {
                self.dbg_cheat_breakpoint();
                return;
            }
            _ => self.arm_raise_undefined(),
        }
    }
    pub fn hit_illegal(&mut self, opcode: u32) {
        mlog!(
            Level::GameError,
            rgba_core::log::ARM,
            "Illegal opcode: {:08X}",
            opcode
        );
        if self.debugger.is_some() {
            use rgba_debugger::debugger::*;
            let pc_addr = self.dbg_pc_address();
            let info = DebuggerEntryInfo {
                address: pc_addr,
                type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
                    opcode,
                    break_type: BreakpointType::Hardware,
                })),
                ..Default::default()
            };
            self.debugger_enter(DebuggerEntryReason::IllegalOp, info);
        }
        self.arm_raise_undefined();
    }
    pub fn hit_stub(&mut self, opcode: u32) {
        if self.debugger.is_some() {
            use rgba_debugger::debugger::*;
            let pc_addr = self.dbg_pc_address();
            let info = DebuggerEntryInfo {
                address: pc_addr,
                type_info: Some(EntryTypeInfo::Bp(BreakpointEntryInfo {
                    opcode,
                    break_type: BreakpointType::Hardware,
                })),
                ..Default::default()
            };
            self.debugger_enter(DebuggerEntryReason::IllegalOp, info);
        }
        mlog!(
            Level::Stub,
            rgba_core::log::ARM,
            "Stub opcode: {:08X}",
            opcode
        );
    }
    pub fn cp_mcr(&mut self, _crn: i32, _crm: i32, _op1: i32, _op2: i32, _val: i32) {}
    pub fn cp_cdp(&mut self, _crn: i32, _crm: i32, _crd: i32, _op1: i32, _op2: i32) {}
    pub fn cp_mrc(&mut self, _crn: i32, _crm: i32, _op1: i32, _op2: i32) -> i32 {
        0
    }

    /// GBAReset — the irqh.reset path.
    pub fn gba_reset(&mut self) {
        self.timing.clear();
        self.memory_reset();
        self.io_init();
        self.video_reset();
        self.dma_reset();
        self.audio_reset();
        self.sio_reset();
        self.timers_reset();
        self.io_deserialize_resets();

        self.cpu.cycles = 0;
        self.cpu.next_event = 0;
        self.memory.io[(GBA_REG_IE >> 1) as usize] = 0;

        // gba.c GBAReset: Matrix carts re-run the initial window remap.
        if self.matrix.has_rom() {
            self.matrix_reset();
        }

        if !self.has_bios {
            self.skip_bios();
        }
        self.cpu.cycles = 0;
        self.cpu.next_event = 0;
        self.set_active_region(self.cpu.gprs[crate::arm::ARM_PC] as u32);
        self.try_keypad_irq();
    }

    fn try_keypad_irq(&mut self) {}

    fn io_deserialize_resets(&mut self) {}

    /// GBASkipBIOS
    pub fn skip_bios(&mut self) {
        // Stack pointers (gba.c)
        self.cpu.gprs[ARM_SP] = GBA_SP_BASE_SYSTEM;
        self.cpu.banked_registers[PrivilegeMode::Supervisor.bank()][0] = GBA_SP_BASE_SUPERVISOR;
        self.cpu.banked_registers[PrivilegeMode::Irq.bank()][0] = GBA_SP_BASE_IRQ;
        self.cpu.gprs[crate::arm::ARM_PC] = 0x08000000;
        let cost = self.arm_write_pc();
        self.cpu.cycles += cost;
    }

    /// _GBACoreRunFrame equivalent
    pub fn run_frame(&mut self) {
        let frame_counter = self.video.frame_counter;
        let start_cycle = self.current_time();
        while self.video.frame_counter == frame_counter
            && self.current_time() - start_cycle
                < (VIDEO_TOTAL_LENGTH + VIDEO_HORIZONTAL_LENGTH) as i32
        {
            self.arm_run_loop();
        }
        // GBASIOPlayerUpdate runs when the GB Player (detection) device is
        // live. The detection bit starts at 0; frontends set it to opt in.
        if self.hw.devices & (crate::gba::HW_GB_PLAYER | crate::gba::HW_GB_PLAYER_DETECTION) != 0 {
            crate::sio::gbp::gbp_update(self);
        }
    }

    pub fn step(&mut self) {
        self.arm_run();
    }
}

// ---------------------------------------------------------------------------
// Core glue: GBALoadROM / GBAReset / game overrides

use crate::cart::GBA_LUX_LEVELS as _GBA_LUX_LEVELS_UNUSED;
use crate::memory::{GBA_SIZE_AGB_PRINT, GBA_SIZE_ROM0};

pub const GBA_ROM_MAGIC: [u8; 1] = [0xEA];
pub const GBA_ROM_MAGIC_OFFSET: usize = 0x3;
pub const GBA_ROM_MAGIC2: [u8; 1] = [0x96];
pub const GBA_ROM_MAGIC_OFFSET2: usize = 0xB2;

impl Gba {
    /// GBAIsROM
    pub fn is_rom(rom: &[u8]) -> bool {
        if rom.len() < 0xC0 {
            return false;
        }
        if rom[GBA_ROM_MAGIC_OFFSET] != 0xEA {
            return false;
        }
        if rom[GBA_ROM_MAGIC_OFFSET2] != 0x96 {
            // BIOS check; unfixed ROMs lacking the magic might still work
            let mut bits: u32 = 0;
            for i in 0..(0x9C / 4) {
                bits |= u32::from_le_bytes([
                    rom[4 + i * 4],
                    rom[5 + i * 4],
                    rom[6 + i * 4],
                    rom[7 + i * 4],
                ]);
            }
            if bits != 0 {
                return false;
            }
        }
        true
    }

    fn to_pow2(bits: u32) -> u32 {
        // next power of two
        if bits == 0 {
            return 0;
        }
        let mut b = 1;
        while b < bits {
            b <<= 1;
        }
        b
    }

    /// GBALoadROM (owned-ROM variant)
    pub fn load_rom(&mut self, rom: Vec<u8>) -> bool {
        self.is_pristine = true;
        self.pristine_rom_size = rom.len();
        // GBAUnloadROM's GBAUnlCartUnload; the new ROM gets a fresh detect.
        self.unl = UnlCart::new();
        self.matrix = Matrix::new();
        let mut rom = rom;

        if rom.len() > GBA_SIZE_ROM0 {
            let ident = rom[0xAC];
            if ident == b'M' {
                self.is_pristine = false;
                self.memory.rom_size = 0x01000000;
                // Matrix cart: the C mmaps a fresh (zeroed) 32MiB window
                // buffer and streams in the full image via _remapMatrix.
                let full = std::mem::take(&mut rom);
                self.matrix_attach_rom(full);
                rom = vec![0; GBA_SIZE_ROM0];
            } else {
                self.memory.rom_size = GBA_SIZE_ROM0;
                if rom.len() >= 0x04000000 && crate::cart::unlicensed::is_multicart_id(&rom) {
                    // Bootleg multicart (GBAUnlCartDetect's multicart branch).
                    // TODO: Identify a bit more precisely
                    self.is_pristine = false;
                    self.savedata_init_sram();
                    let full = std::mem::take(&mut rom);
                    rom = full[..GBA_SIZE_ROM0].to_vec();
                    self.unl.cart_type = crate::cart::unlicensed::UnlCartType::Multicart;
                    self.unl.multi.init_multicart(full);
                    // The C also drops the GPIO base pointer here; our GPIO
                    // is keyed off hw.devices, which stays clean.
                }
                rom.truncate(GBA_SIZE_ROM0);
            }
            self.pristine_rom_size = GBA_SIZE_ROM0;
        } else if rom.len() == 0x00100000 {
            // 1 MiB ROMs (e.g. Classic NES) all appear as 4x mirrored
            self.is_pristine = false;
            self.memory.rom_size = 0x00400000;
            let mut padded = vec![0xFF; GBA_SIZE_ROM0];
            padded[..0x100000].copy_from_slice(&rom);
            for off in [0x100000, 0x80000 * 2, 0xC0000] {
                let src = &padded[..0x100000].to_vec();
                if off + 0x100000 <= padded.len() {
                    padded[off..off + 0x100000].copy_from_slice(src);
                }
            }
            rom = padded;
        } else {
            self.memory.rom_size = rom.len();
        }
        self.yanked_rom_size = 0;
        self.memory.rom_mask = Gba::to_pow2(self.memory.rom_size as u32).wrapping_sub(1);
        self.rom_crc32 = crate::crc32(&rom);
        if self.memory.rom_size.count_ones() != 1 {
            self.memory.rom_size = GBA_SIZE_ROM0;
            self.memory.rom_mask = (GBA_SIZE_ROM0 - 1) as u32;
            self.is_pristine = false;
        }
        self.memory.rom = rom;
        if self.memory.active_region >= crate::memory::GBA_REGION_ROM0 as i32 {
            self.set_active_region(self.cpu.gprs[crate::arm::ARM_PC] as u32);
        }
        self.apply_overrides();
        self.unl_cart_detect();
        true
    }

    fn apply_overrides(&mut self) {
        // GBAOverrideApplyDefaults (gba/overrides.c); config/ini overrides are
        // not ported, so this is the static table + Pokémon ROM-hack defaults.
        // The C clears HW_GB_PLAYER_DETECTION inside OverridesApply unless
        // the config's forceGbp is set (GBAStartedLoad); mirror that here.
        if !self.force_gbp {
            self.hw.devices &= !crate::gba::HW_GB_PLAYER_DETECTION;
        }
        crate::overrides::override_apply_defaults(self);
        if self.force_gbp {
            self.hw.devices |= crate::gba::HW_GB_PLAYER_DETECTION;
        }
    }
}

// ---------------------------------------------------------------------------
// rgba_core::Core impl

use rgba_core::core::{Core, CoreInfo, Platform};
use rgba_core::ring::RingI16;

impl Core for Gba {
    fn platform(&self) -> Platform {
        Platform::Gba
    }
    fn info(&self) -> CoreInfo {
        CoreInfo {
            platform: Platform::Gba,
            width: 240,
            height: 160,
            frequency: self.frequency(),
            frame_cycles: self.frame_cycles(),
        }
    }
    fn is_rom(rom: &[u8]) -> bool {
        Gba::is_rom(rom)
    }
    fn reset(&mut self) {
        self.arm_reset();
    }
    fn unload_rom(&mut self) {}
    fn run_frame(&mut self) {
        Gba::run_frame(self);
    }
    fn step(&mut self) {
        Gba::step(self);
    }
    fn set_keys(&mut self, keys: u32) {
        self.keys = keys;
        let keys16 = 0x3FFu32.wrapping_sub(keys as u16 as u32 & 0x3FF);
        self.keys_active = keys16 as u16;
        self.test_keypad_irq();
    }
    fn keys(&self) -> u32 {
        !self.keys_active as u32 & 0x3FF
    }
    fn video_buffer(&self) -> &[u32] {
        &self.video.sw.output
    }
    fn base_video_size(&self) -> (u32, u32) {
        (240, 160)
    }
    fn audio_buffer(&mut self) -> &mut RingI16 {
        &mut self.audio.buffer
    }
    fn audio_sample_rate(&self) -> i32 {
        32768
    }
    fn set_audio_buffer_size(&mut self, _samples: usize) {}
    fn debugger_attach(&mut self) -> bool {
        self.debugger_attach();
        true
    }

    fn debugger_run_frame(&mut self) {
        // Inherent method (impl Gb in crate::debugger) — the C's
        // mCoreThread loop calling mDebuggerRunFrame.
        self.debugger_run_frame();
    }

    fn debugger_break(&mut self) {
        use rgba_debugger::debugger::{DebuggerEntryInfo, DebuggerEntryReason};
        self.debugger_enter(DebuggerEntryReason::Manual, DebuggerEntryInfo::default());
    }

    fn debugger_attached(&self) -> bool {
        self.debugger.is_some()
    }

    fn frame_counter(&self) -> u32 {
        self.video.frame_counter
    }
    fn frame_cycles(&self) -> i32 {
        VIDEO_TOTAL_LENGTH
    }
    fn frequency(&self) -> i32 {
        0x1000000
    }
    fn state_size(&mut self) -> usize {
        crate::serialize::GBA_SAVESTATE_SIZE
    }
    fn save_state(&mut self, out: &mut Vec<u8>) -> Result<(), &'static str> {
        let v = crate::serialize::serialize(self)?;
        *out = v;
        Ok(())
    }
    fn load_state(&mut self, state: &[u8]) -> Result<(), &'static str> {
        crate::serialize::deserialize(self, state)
    }
    fn savedata(&self) -> Option<&[u8]> {
        if self.savedata.data.is_empty() {
            None
        } else {
            Some(&self.savedata.data)
        }
    }
    fn savedata_mut(&mut self) -> Option<&mut [u8]> {
        if self.savedata.data.is_empty() {
            None
        } else {
            Some(&mut self.savedata.data)
        }
    }
}
