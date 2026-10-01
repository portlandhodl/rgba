// Copyright (c) 2013-2017 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gb/sio.c and include/mgba/internal/gb/sio.h,
// mgba/src/gb/sio/lockstep.c and include/mgba/internal/gb/sio/lockstep.h,
// mgba/src/gb/sio/printer.c and include/mgba/internal/gb/sio/printer.h.
//
// The `struct GBSIODriver` vtable becomes the `SioDriver` enum. The lockstep
// sync glue (`struct mLockstep`'s function pointers, in mGBA installed by the
// Qt frontend for threaded cores) is implemented in `GbSioLockstep` as a
// single-threaded, cooperative model: instead of blocking a core thread on
// `wait`, the master parks its node event and polls; `signal`/`addCycles`/
// `useCycles`/`unusedCycles`/`unload` follow the Qt glue's data flow
// (src/platform/qt/MultiplayerController.cpp, GB branches) exactly.
//
// Both linked `Gb`s therefore must live on one thread and share the lockstep
// through an `Rc<RefCell<GbSioLockstep>>` (the node holds a Weak handle).
//
// INTEGRATION NOTE: the lockstep node's timing event (priority 0x80 in C)
// does not have an `EventId` variant (gb.rs is fixed). It is scheduled under
// `GB_SIO_LOCKSTEP_EVENT_ID`; `Gb::process_event` needs one extra arm
// dispatching that id to `Gb::sio_lockstep_event` when linked play is wired
// up. Until then, attaching a lockstep node schedules an event no dispatcher
// recognizes.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use rgba_core::mlog;
use rgba_core::timing::Timing;
use rgba_core::Level;

use crate::gb::{EventId, Gb};
use crate::io::{GB_IRQ_SIO, GB_REG_IF, GB_REG_SB, GB_REG_SC};

/// GBSIOCyclesPerTransfer
pub const GB_SIO_CYCLES_PER_TRANSFER: [i32; 2] = [512, 16];

/// MAX_GBS
pub const MAX_GBS: usize = 2;

/// enum GBVideoConstants (include/mgba/internal/gb/video.h); kept local
/// because video.rs is ported separately.
const GB_VIDEO_HORIZONTAL_PIXELS: usize = 160;
const GB_VIDEO_VERTICAL_PIXELS: usize = 144;

// Register SC bit helpers (DECL_BIT(GBRegisterSC, ...)):
// bit 0: shift clock (internal clock), bit 1: clock speed, bit 7: enable.
const GB_REG_SC_ENABLE: u8 = 0x80;
const GB_REG_SC_SHIFT_CLOCK: u8 = 0x01;

/// struct GBSIO. The C `event` slot is folded into `EventId::Sio`
/// (priority 0x30, see gb.rs); `p` back-pointers become methods on `Gb`.
pub struct Sio {
    pub next_event: i32,
    pub period: i32,
    pub remaining_bits: i32,
    pub pending_sb: u8,
    pub driver: SioDriver,
}

impl Sio {
    /// GBSIOInit
    pub fn new() -> Self {
        Sio {
            next_event: i32::MAX,
            period: 0,
            remaining_bits: 0,
            pending_sb: 0xFF,
            driver: SioDriver::None,
        }
    }

    /// GBSIODeinit — nothing to do yet (mirrors the C).
    pub fn deinit(&mut self) {}
}

impl Default for Sio {
    fn default() -> Self {
        Self::new()
    }
}

/// struct GBSIODriver vtable, as an enum of link-cable peers.
pub enum SioDriver {
    None,
    Printer(GbPrinter),
    Lockstep(GbSioLockstepNode),
}

impl Default for SioDriver {
    fn default() -> Self {
        SioDriver::None
    }
}

impl Gb {
    /// GBSIOReset
    pub fn sio_reset(&mut self) {
        self.sio.next_event = i32::MAX;
        self.sio.remaining_bits = 0;
        // GBSIOSetDriver(sio, sio->driver): re-init the current driver.
        let driver = std::mem::replace(&mut self.sio.driver, SioDriver::None);
        let driver = match driver {
            SioDriver::None => None,
            other => Some(other),
        };
        self.sio_set_driver(driver);
    }

    /// GBSIODeinit
    pub fn sio_deinit(&mut self) {
        // Nothing to do yet
    }

    /// GBSIOSetDriver
    pub fn sio_set_driver(&mut self, driver: Option<SioDriver>) {
        // Deinit the old driver, if there is one.
        let mut old = std::mem::replace(&mut self.sio.driver, SioDriver::None);
        if let SioDriver::Lockstep(node) = &mut old {
            // GBPrinterDeinit only frees the buffer; the Vec handles that.
            node.deinit(self);
        }
        drop(old);
        let Some(mut driver) = driver else {
            return;
        };
        // driver->p = sio; if (driver->init) { ... }
        let ok = match &mut driver {
            SioDriver::None => true,
            SioDriver::Printer(p) => {
                p.init();
                true
            }
            SioDriver::Lockstep(node) => node.init(self),
        };
        if !ok {
            if let SioDriver::Lockstep(node) = &mut driver {
                node.deinit(self);
            }
            mlog!(
                Level::Error,
                rgba_core::log::GB_SIO,
                "Could not initialize SIO driver"
            );
            return;
        }
        self.sio.driver = driver;
    }

    /// _GBSIOProcessEvents (the mTimingEvent callback at priority 0x30)
    pub fn sio_event(&mut self, timing: &mut Timing, _cycles_late: u32) {
        let mut do_irq = false;
        if self.sio.remaining_bits != 0 {
            do_irq = true;
            self.sio.remaining_bits -= 1;
            let mask = (128u16 >> self.sio.remaining_bits) as u8;
            self.memory.io[GB_REG_SB as usize] &= !mask;
            self.memory.io[GB_REG_SB as usize] |= self.sio.pending_sb & mask;
        }
        if self.sio.remaining_bits == 0 {
            // GBRegisterSCClearEnable
            self.memory.io[GB_REG_SC as usize] &= !GB_REG_SC_ENABLE;
            if do_irq {
                self.memory.io[GB_REG_IF as usize] |= 1 << GB_IRQ_SIO;
                self.update_irqs();
                self.sio.pending_sb = 0xFF;
            }
        } else {
            let when = self.sio.period * (2 - self.double_speed as i32);
            timing.schedule(EventId::Sio.into(), EventId::Sio.priority(), when);
        }
    }

    /// GBSIOWriteSB
    pub fn sio_write_sb(&mut self, sb: u8) {
        match &mut self.sio.driver {
            SioDriver::None => (),
            SioDriver::Printer(p) => p.write_sb(sb),
            SioDriver::Lockstep(node) => node.write_sb(sb),
        }
    }

    /// GBSIOWriteSC (the driver's returned byte is ignored, as in the C;
    /// the SC register cache in memory.io is written by the io_write caller)
    pub fn sio_write_sc(&mut self, sc: u8) {
        self.sio.period = GB_SIO_CYCLES_PER_TRANSFER[((sc >> 1) & 1) as usize]; // TODO Shift Clock
        if sc & GB_REG_SC_ENABLE != 0 {
            if sc & GB_REG_SC_SHIFT_CLOCK != 0 {
                self.deschedule(EventId::Sio);
                let when = self.sio.period * (2 - self.double_speed as i32);
                self.schedule(EventId::Sio, when);
                self.sio.remaining_bits = 8;
            }
        } else {
            self.deschedule(EventId::Sio);
        }
        let driver = std::mem::replace(&mut self.sio.driver, SioDriver::None);
        match driver {
            SioDriver::None => {}
            SioDriver::Printer(mut p) => {
                p.write_sc(&mut self.sio.pending_sb, sc);
                self.sio.driver = SioDriver::Printer(p);
            }
            SioDriver::Lockstep(mut node) => {
                node.write_sc(self, sc);
                self.sio.driver = SioDriver::Lockstep(node);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Game Boy Printer (mgba/src/gb/sio/printer.c)
// ---------------------------------------------------------------------------

/// enum GBPrinterPacketByte
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GbPrinterPacketByte {
    Magic0,
    Magic1,
    Command,
    Compression,
    Length0,
    Length1,
    Data,
    Checksum0,
    Checksum1,
    Keepalive,
    Status,
    CompressedDatum,
    UncompressedData,
}

/// enum GBPrinterStatus
pub const GB_PRINTER_STATUS_CHECKSUM_ERROR: u8 = 0x01;
pub const GB_PRINTER_STATUS_PRINTING: u8 = 0x02;
pub const GB_PRINTER_STATUS_PRINT_REQ: u8 = 0x04;
pub const GB_PRINTER_STATUS_READY: u8 = 0x08;
pub const GB_PRINTER_STATUS_LOW_BATTERY: u8 = 0x10;
pub const GB_PRINTER_STATUS_TIMEOUT: u8 = 0x20;
pub const GB_PRINTER_STATUS_PAPER_JAM: u8 = 0x40;
pub const GB_PRINTER_STATUS_TEMPERATURE_ISSUE: u8 = 0x80;

/// enum GBPrinterCommand
pub const GB_PRINTER_COMMAND_INIT: u8 = 0x1;
pub const GB_PRINTER_COMMAND_PRINT: u8 = 0x2;
pub const GB_PRINTER_COMMAND_DATA: u8 = 0x4;
pub const GB_PRINTER_COMMAND_STATUS: u8 = 0xF;

// Palette used by the reference frontends for printed images
// (src/platform/qt/CoreController.cpp QGBPrinter).
const GB_PRINTER_SHADES: [u32; 4] = [0xFFF8F8F8, 0xFFA8A8A8, 0xFF505050, 0xFF000000];

/// struct GBPrinter. The C `void (*print)(...)` callback is replaced by
/// `pending_image`: when a print command completes, the tile buffer is
/// reshuffled (exactly like the C) and decoded to 0xFFRRGGBB pixels
/// (width GB_VIDEO_HORIZONTAL_PIXELS) into that buffer.
pub struct GbPrinter {
    buffer: Vec<u8>,
    checksum: u16,
    command: u8,
    remaining_bytes: u16,
    remaining_cmp_bytes: u8,
    current_index: usize,
    compression: bool,
    byte: u8,
    next: GbPrinterPacketByte,
    pub status: u8,
    print_wait: i32,

    /// Decoded 0xFFRRGGBB pixels of the most recent completed print.
    pub pending_image: Vec<u32>,
    image_ready: bool,
}

impl GbPrinter {
    /// GBPrinterCreate
    pub fn new() -> Self {
        GbPrinter {
            buffer: Vec::new(),
            checksum: 0,
            command: 0,
            remaining_bytes: 0,
            remaining_cmp_bytes: 0,
            current_index: 0,
            compression: false,
            byte: 0,
            next: GbPrinterPacketByte::Magic0,
            status: 0,
            print_wait: -1,
            pending_image: Vec::new(),
            image_ready: false,
        }
    }

    /// GBPrinterInit
    fn init(&mut self) {
        self.checksum = 0;
        self.command = 0;
        self.remaining_bytes = 0;
        self.current_index = 0;
        self.compression = false;
        self.byte = 0;
        self.next = GbPrinterPacketByte::Magic0;
        self.status = 0;
        self.print_wait = -1;
        if self.buffer.is_empty() {
            self.buffer = vec![0; GB_VIDEO_HORIZONTAL_PIXELS * GB_VIDEO_VERTICAL_PIXELS / 2];
        }
    }

    /// GBPrinterDonePrinting
    pub fn done_printing(&mut self) {
        self.status &= !(GB_PRINTER_STATUS_PRINTING | GB_PRINTER_STATUS_PRINT_REQ);
    }

    /// Decoded image of the last completed print, 0xFFRRGGBB per pixel with
    /// width `GB_VIDEO_HORIZONTAL_PIXELS` (160); `None` if no print has
    /// completed since the last `take_image`/`new`.
    pub fn printer_image(&self) -> Option<&[u32]> {
        if self.image_ready {
            Some(&self.pending_image)
        } else {
            None
        }
    }

    /// Callback-style ownership handoff of the completed image (the C feeds
    /// the buffer to `printer->print` exactly once per print).
    pub fn take_image(&mut self) -> Option<Vec<u32>> {
        if self.image_ready {
            self.image_ready = false;
            Some(std::mem::take(&mut self.pending_image))
        } else {
            None
        }
    }

    /// GBPrinterWriteSB
    fn write_sb(&mut self, value: u8) {
        self.byte = value;
    }

    /// _processByte
    fn process_byte(&mut self) {
        match self.command {
            GB_PRINTER_COMMAND_DATA => {
                if self.current_index < GB_VIDEO_VERTICAL_PIXELS * GB_VIDEO_HORIZONTAL_PIXELS / 2 {
                    self.buffer[self.current_index] = self.byte;
                    self.current_index += 1;
                }
            }
            // GB_PRINTER_COMMAND_PRINT: TODO (as in the C)
            _ => {}
        }
    }

    /// The `if (printer->print)` block of GBPrinterWriteSC: the in-place
    /// 2bpp planar→linear reshuffle, followed by the image delivery, which
    /// here decodes into `pending_image`.
    fn print_image(&mut self) {
        let mut buffer = std::mem::take(&mut self.buffer);
        for y in 0..self.current_index / (2 * GB_VIDEO_HORIZONTAL_PIXELS) {
            let mut line_buffer = [0u8; GB_VIDEO_HORIZONTAL_PIXELS * 2];
            {
                let buf = &mut buffer[line_buffer.len() * y..];
                let mut i = 0;
                while i < line_buffer.len() {
                    let ilo = buf[i];
                    let ihi = buf[i + 0x1];
                    let mut olo = 0u8;
                    let mut ohi = 0u8;
                    olo |= (ihi & 0x80) >> 0;
                    olo |= (ilo & 0x80) >> 1;
                    olo |= (ihi & 0x40) >> 1;
                    olo |= (ilo & 0x40) >> 2;
                    olo |= (ihi & 0x20) >> 2;
                    olo |= (ilo & 0x20) >> 3;
                    olo |= (ihi & 0x10) >> 3;
                    olo |= (ilo & 0x10) >> 4;
                    ohi |= (ihi & 0x08) << 4;
                    ohi |= (ilo & 0x08) << 3;
                    ohi |= (ihi & 0x04) << 3;
                    ohi |= (ilo & 0x04) << 2;
                    ohi |= (ihi & 0x02) << 2;
                    ohi |= (ilo & 0x02) << 1;
                    ohi |= (ihi & 0x01) << 1;
                    ohi |= (ilo & 0x01) << 0;
                    line_buffer
                        [(((i >> 1) & 0x7) * GB_VIDEO_HORIZONTAL_PIXELS / 4) + ((i >> 3) & !1)] =
                        olo;
                    line_buffer
                        [(((i >> 1) & 0x7) * GB_VIDEO_HORIZONTAL_PIXELS / 4) + ((i >> 3) | 1)] =
                        ohi;
                    i += 2;
                }
                buf[..line_buffer.len()].copy_from_slice(&line_buffer);
            }
        }
        self.buffer = buffer;
        // printer->print(printer, currentIndex * 4 / GB_VIDEO_HORIZONTAL_PIXELS, printer->buffer)
        let height = self.current_index * 4 / GB_VIDEO_HORIZONTAL_PIXELS;
        self.pending_image.clear();
        self.pending_image
            .reserve(GB_VIDEO_HORIZONTAL_PIXELS * height);
        for y in 0..height {
            let mut x = 0;
            while x < GB_VIDEO_HORIZONTAL_PIXELS {
                let byte = self.buffer[(x + y * GB_VIDEO_HORIZONTAL_PIXELS) / 4];
                self.pending_image
                    .push(GB_PRINTER_SHADES[((byte & 0xC0) >> 6) as usize]);
                self.pending_image
                    .push(GB_PRINTER_SHADES[((byte & 0x30) >> 4) as usize]);
                self.pending_image
                    .push(GB_PRINTER_SHADES[((byte & 0x0C) >> 2) as usize]);
                self.pending_image
                    .push(GB_PRINTER_SHADES[(byte & 0x03) as usize]);
                x += 4;
            }
        }
        self.image_ready = true;
    }

    /// GBPrinterWriteSC. `pending_sb` stands for `driver->p->pendingSB`.
    fn write_sc(&mut self, pending_sb: &mut u8, value: u8) -> u8 {
        if (value & 0x81) == 0x81 {
            *pending_sb = 0;
            match self.next {
                GbPrinterPacketByte::Magic0 => {
                    if self.byte == 0x88 {
                        self.next = GbPrinterPacketByte::Magic1;
                    } else {
                        self.next = GbPrinterPacketByte::Magic0;
                    }
                }
                GbPrinterPacketByte::Magic1 => {
                    if self.byte == 0x33 {
                        self.next = GbPrinterPacketByte::Command;
                    } else {
                        self.next = GbPrinterPacketByte::Magic0;
                    }
                }
                GbPrinterPacketByte::Command => {
                    self.checksum = self.byte as u16;
                    self.command = self.byte;
                    self.next = GbPrinterPacketByte::Compression;
                }
                GbPrinterPacketByte::Compression => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    self.compression = self.byte != 0;
                    self.next = GbPrinterPacketByte::Length0;
                }
                GbPrinterPacketByte::Length0 => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    self.remaining_bytes = self.byte as u16;
                    self.next = GbPrinterPacketByte::Length1;
                }
                GbPrinterPacketByte::Length1 => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    self.remaining_bytes |= (self.byte as u16) << 8;
                    if self.remaining_bytes != 0 {
                        self.next = GbPrinterPacketByte::Data;
                    } else {
                        self.next = GbPrinterPacketByte::Checksum0;
                    }
                    match self.command {
                        GB_PRINTER_COMMAND_INIT => {
                            self.current_index = 0;
                            self.status &= !(GB_PRINTER_STATUS_PRINT_REQ | GB_PRINTER_STATUS_READY);
                        }
                        _ => {}
                    }
                }
                GbPrinterPacketByte::Data => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    if !self.compression {
                        self.process_byte();
                    } else {
                        self.next = if self.byte & 0x80 != 0 {
                            GbPrinterPacketByte::CompressedDatum
                        } else {
                            GbPrinterPacketByte::UncompressedData
                        };
                        self.remaining_cmp_bytes = (self.byte & 0x7F) + 1;
                        if self.byte & 0x80 != 0 {
                            self.remaining_cmp_bytes += 1;
                        }
                    }
                    self.remaining_bytes = self.remaining_bytes.wrapping_sub(1);
                    if self.remaining_bytes == 0 {
                        self.next = GbPrinterPacketByte::Checksum0;
                    }
                }
                GbPrinterPacketByte::UncompressedData => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    self.process_byte();
                    self.remaining_cmp_bytes = self.remaining_cmp_bytes.wrapping_sub(1);
                    if self.remaining_cmp_bytes == 0 {
                        self.next = GbPrinterPacketByte::Data;
                    }
                    self.remaining_bytes = self.remaining_bytes.wrapping_sub(1);
                    if self.remaining_bytes == 0 {
                        self.next = GbPrinterPacketByte::Checksum0;
                    }
                }
                GbPrinterPacketByte::CompressedDatum => {
                    self.checksum = self.checksum.wrapping_add(self.byte as u16);
                    while self.remaining_cmp_bytes != 0 {
                        self.process_byte();
                        self.remaining_cmp_bytes = self.remaining_cmp_bytes.wrapping_sub(1);
                    }
                    self.remaining_bytes = self.remaining_bytes.wrapping_sub(1);
                    if self.remaining_bytes == 0 {
                        self.next = GbPrinterPacketByte::Checksum0;
                    } else {
                        self.next = GbPrinterPacketByte::Data;
                    }
                }
                GbPrinterPacketByte::Checksum0 => {
                    self.checksum ^= self.byte as u16;
                    self.next = GbPrinterPacketByte::Checksum1;
                }
                GbPrinterPacketByte::Checksum1 => {
                    self.checksum ^= (self.byte as u16) << 8;
                    self.next = GbPrinterPacketByte::Keepalive;
                }
                GbPrinterPacketByte::Keepalive => {
                    *pending_sb = 0x81;
                    self.next = GbPrinterPacketByte::Status;
                }
                GbPrinterPacketByte::Status => {
                    match self.command {
                        GB_PRINTER_COMMAND_DATA => {
                            if self.current_index >= 0x280
                                && self.status & GB_PRINTER_STATUS_CHECKSUM_ERROR == 0
                            {
                                self.status |= GB_PRINTER_STATUS_READY;
                            }
                        }
                        GB_PRINTER_COMMAND_PRINT => {
                            if self.current_index >= GB_VIDEO_HORIZONTAL_PIXELS * 2 {
                                self.print_wait = 0;
                            }
                        }
                        _ => {}
                    }
                    *pending_sb = self.status;
                    self.next = GbPrinterPacketByte::Magic0;
                }
            }

            if self.print_wait == 0 {
                self.status &= !GB_PRINTER_STATUS_READY;
                self.status |= GB_PRINTER_STATUS_PRINTING | GB_PRINTER_STATUS_PRINT_REQ;
                self.print_image();
                self.print_wait = -1;
                self.current_index = 0;
            } else if self.print_wait > 0 {
                self.print_wait -= 1;
            }

            self.byte = 0;
        }
        value
    }
}

impl Default for GbPrinter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Link-cable lockstep (mgba/src/gb/sio/lockstep.c)
// ---------------------------------------------------------------------------

pub const LOCKSTEP_INCREMENT: i32 = 512;

/// The node event has priority 0x80 in C but no `EventId` slot (see the
/// module comment); schedule under this id.
pub const GB_SIO_LOCKSTEP_EVENT_ID: u32 = u32::MAX;
pub const GB_SIO_LOCKSTEP_EVENT_PRIORITY: u32 = 0x80;

/// enum mLockstepPhase
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockstepPhase {
    Idle,
    Starting,
    Started,
    Finishing,
    Finished,
}

/// struct GBSIOLockstep (and the embedded struct mLockstep). Shared between
/// the linked cores; the glue state (cycles_posted, awake, master_wait_mask,
/// node_next_event/node_event_diff) is the Qt frontend's mLockstep vtable
/// data, kept per live node id exactly like the C resolves `player(id)` and
/// `players[i]` by current node id.
pub struct GbSioLockstep {
    pub attached: usize,
    pub transfer_active: LockstepPhase,
    pub transfer_cycles: i32,

    pending_sb: [u8; MAX_GBS],
    master_claimed: bool,

    // Stable attach-token -> live node id (the C keeps node->id in the node
    // and players[] pointers updated on attach/detach/id-swap).
    id_of_token: [i32; MAX_GBS],
    // Glue state, indexed by live node id:
    cycles_posted: [i32; MAX_GBS],
    awake: [bool; MAX_GBS],
    master_wait_mask: u32,
    node_next_event: [i32; MAX_GBS],
    node_event_diff: [i32; MAX_GBS],
}

impl GbSioLockstep {
    /// GBSIOLockstepInit. The `Rc<RefCell<...>>` is shared between the
    /// linked `Gb`s; each node downgrades it to a Weak handle.
    pub fn new() -> Rc<RefCell<GbSioLockstep>> {
        Rc::new(RefCell::new(GbSioLockstep {
            attached: 0,
            transfer_active: LockstepPhase::Idle,
            transfer_cycles: 0,
            pending_sb: [0xFF; MAX_GBS],
            master_claimed: false,
            id_of_token: [-1; MAX_GBS],
            cycles_posted: [0; MAX_GBS],
            awake: [true; MAX_GBS], // struct Player: int awake = 1
            master_wait_mask: 0,
            node_next_event: [0; MAX_GBS],
            node_event_diff: [0; MAX_GBS],
        }))
    }

    /// GBSIOLockstepAttachNode; the returned node should be installed with
    /// `Gb::sio_set_driver` (mirroring attachGame's order:
    /// GBSIOLockstepAttachNode → GBSIOSetDriver).
    pub fn attach_node(lockstep: &Rc<RefCell<GbSioLockstep>>) -> Option<GbSioLockstepNode> {
        let mut ls = lockstep.borrow_mut();
        if ls.attached == MAX_GBS {
            return None;
        }
        let token = ls
            .id_of_token
            .iter()
            .position(|&id| id < 0)
            .expect("id_of_token slot");
        ls.id_of_token[token] = ls.attached as i32; // node->id = attached
        ls.attached += 1;
        Some(GbSioLockstepNode {
            p: Rc::downgrade(lockstep),
            token,
            transfer_finished: false,
            waiting_on_slaves: false,
        })
    }

    /// GBSIOLockstepDetachNode
    pub fn detach_node(&mut self, token: usize) {
        if self.attached == 0 {
            return;
        }
        if token >= MAX_GBS || self.id_of_token[token] < 0 {
            return;
        }
        let id = self.id_of_token[token] as usize;
        // for (++i; i < lockstep->d.attached; ++i) { players[i-1] = players[i]; players[i-1]->id = i-1; }
        for i in id + 1..self.attached {
            if let Some(owner) = (0..MAX_GBS).find(|&t| self.id_of_token[t] == i as i32) {
                self.id_of_token[owner] = (i - 1) as i32;
            }
        }
        self.attached -= 1;
        self.id_of_token[token] = -1;
    }

    /// Live node id for a token (node->id).
    pub fn node_id(&self, token: usize) -> i32 {
        self.id_of_token[token]
    }

    // --- mLockstep glue (GB branches of the Qt frontend vtable), ----------
    // --- single-threaded cooperative model. -------------------------------

    /// signal(): slave -> master rendezvous notification.
    fn signal(&mut self, mask: u32) -> bool {
        self.master_wait_mask &= !mask;
        let mut woke = false;
        if self.master_wait_mask == 0 && !self.awake[0] {
            // mCoreThreadStopWaiting(master)
            self.awake[0] = true;
            woke = true;
        }
        woke
    }

    /// wait(): parks the master until every node in `mask` signals.
    fn wait(&mut self, mask: u32) -> bool {
        self.master_wait_mask |= mask;
        let mut slept = false;
        if self.awake[0] {
            // mCoreThreadWaitFromThread(master): the parked master resumes
            // via its node event callback once the mask clears.
            self.awake[0] = false;
            slept = true;
        }
        // setSync(true) is a no-op in the single-threaded model
        slept
    }

    /// addCycles(): for id 0 the master grants allowance to the slave;
    /// otherwise the node posts its own allowance.
    fn add_cycles(&mut self, id: usize, cycles: i32) {
        if cycles < 0 {
            // if (cycles < 0) abort();
            panic!("GBSIO lockstep: addCycles called with negative cycles");
        }
        if id == 0 {
            let slave = 1;
            // setSync(false) is a no-op in the single-threaded model
            self.cycles_posted[slave] += cycles;
            if !self.awake[slave] {
                self.node_next_event[slave] += self.cycles_posted[slave];
            }
            // mCoreThreadStopWaiting(slave)
            self.awake[slave] = true;
        } else {
            // setSync(true) is a no-op in the single-threaded model
            self.cycles_posted[id] += cycles;
        }
    }

    /// useCycles()
    fn use_cycles(&mut self, id: usize, cycles: i32) -> i32 {
        self.cycles_posted[id] -= cycles;
        if self.cycles_posted[id] <= 0 {
            // mCoreThreadWaitFromThread(self)
            self.awake[id] = false;
        }
        self.cycles_posted[id]
    }

    /// unusedCycles()
    fn unused_cycles(&self, id: usize) -> i32 {
        self.cycles_posted[id]
    }

    /// unload()
    fn unload(&mut self, id: usize) {
        if id != 0 {
            // setSync(true) is a no-op in the single-threaded model
            self.cycles_posted[id] = 0;

            // release master if it is waiting for this node
            self.master_wait_mask &= !(1u32 << id);
            if self.master_wait_mask == 0 && !self.awake[0] {
                // mCoreThreadStopWaiting(master)
                self.awake[0] = true;
            }
        } else {
            for i in 1..self.attached {
                // setSync(true) is a no-op in the single-threaded model
                self.cycles_posted[i] += self.node_event_diff[0];
                if !self.awake[i] {
                    self.node_next_event[i] += self.cycles_posted[i];
                }
                // mCoreThreadStopWaiting(player[i])
                self.awake[i] = true;
            }
        }
    }
}

/// struct GBSIOLockstepNode, owned by its `Gb` inside `SioDriver::Lockstep`.
/// The weak handle to the shared lockstep stands for the C's `node->p`.
/// NOTE: `nextEvent`/`eventDiff` live in the shared `GbSioLockstep` (indexed
/// by live node id) because the C pokes them cross-core through
/// `players[i]`; `id` likewise (it can be renumbered by attach/detach and
/// the master-claim swap).
pub struct GbSioLockstepNode {
    p: Weak<RefCell<GbSioLockstep>>,
    token: usize,
    transfer_finished: bool,
    /// Port modeling aid: the C blocks the master thread inside
    /// mLockstep::wait; here the wait point resumes from
    /// `master_update`'s re-entry guarded by this flag.
    waiting_on_slaves: bool,
}

/// Schedule/deschedule the node event from outside a timing callback,
/// keeping the same relative-cycles sync as `Gb::schedule`.
fn schedule_lockstep_event(gb: &mut Gb, when: i32) {
    gb.timing.set_relative_cycles(gb.cpu.cycles);
    gb.timing.schedule(
        GB_SIO_LOCKSTEP_EVENT_ID,
        GB_SIO_LOCKSTEP_EVENT_PRIORITY,
        when,
    );
    gb.cpu.next_event = gb.timing.next_event_cycles_owned();
}

fn deschedule_lockstep_event(gb: &mut Gb) {
    gb.timing.deschedule(GB_SIO_LOCKSTEP_EVENT_ID);
}

impl GbSioLockstepNode {
    /// The node's stable attach token (for GbSioLockstep::detach_node).
    pub fn token(&self) -> usize {
        self.token
    }

    /// GBSIOLockstepNodeInit
    fn init(&mut self, gb: &mut Gb) -> bool {
        let Some(rc) = self.p.upgrade() else {
            return false;
        };
        {
            let mut ls = rc.borrow_mut();
            let id = ls.node_id(self.token);
            if id < 0 {
                return false;
            }
            let id = id as usize;
            ls.node_next_event[id] = 0;
            ls.node_event_diff[id] = 0;
        }
        schedule_lockstep_event(gb, 0);
        true
    }

    /// GBSIOLockstepNodeDeinit
    fn deinit(&mut self, gb: &mut Gb) {
        if let Some(rc) = self.p.upgrade() {
            let mut ls = rc.borrow_mut();
            let id = ls.node_id(self.token);
            if id >= 0 {
                ls.unload(id as usize);
            }
        }
        deschedule_lockstep_event(gb);
    }

    /// GBSIOLockstepNodeWriteSB
    fn write_sb(&mut self, value: u8) {
        if let Some(rc) = self.p.upgrade() {
            let mut ls = rc.borrow_mut();
            let id = ls.node_id(self.token);
            if id < 0 {
                return;
            }
            ls.pending_sb[id as usize] = value;
        }
    }

    /// GBSIOLockstepNodeWriteSC
    fn write_sc(&mut self, gb: &mut Gb, value: u8) -> u8 {
        let Some(rc) = self.p.upgrade() else {
            return value;
        };
        let mut ls = rc.borrow_mut();
        if (value & 0x81) == 0x81 && ls.attached > 1 {
            // ATOMIC_CMPXCHG(node->p->masterClaimed, false, true)
            if !ls.master_claimed {
                ls.master_claimed = true;
                let id = ls.node_id(self.token);
                if id < 0 {
                    return value;
                }
                let id = id as usize;
                if id != 0 {
                    // The claiming node becomes player 0; ids, the players
                    // slots and pendingSB all swap. Per-id glue state
                    // follows its node (the C's players[] are pointers).
                    let other = (0..MAX_GBS).find(|&t| t != self.token && ls.id_of_token[t] >= 0);
                    if let Some(other) = other {
                        let ta = self.token.min(other);
                        let tb = self.token.max(other);
                        // id_of_token[tb] is the current id-0 node
                        if ls.id_of_token[self.token] != 0 {
                            ls.id_of_token.swap(ta, tb);
                        }
                    }
                    ls.pending_sb.swap(0, 1);
                    ls.cycles_posted.swap(0, 1);
                    ls.awake.swap(0, 1);
                    ls.node_next_event.swap(0, 1);
                    ls.node_event_diff.swap(0, 1);
                }
                ls.transfer_active = LockstepPhase::Starting; // TRANSFER_STARTING
                ls.transfer_cycles = GB_SIO_CYCLES_PER_TRANSFER[((value >> 1) & 1) as usize];
                drop(ls);
                gb.deschedule(EventId::Sio);
                deschedule_lockstep_event(gb);
                schedule_lockstep_event(gb, 0);
            } else {
                mlog!(
                    Level::Debug,
                    rgba_core::log::GB_SIO,
                    "GBSIOLockstepNodeWriteSC() failed to write to masterClaimed"
                );
            }
        }
        value
    }

    /// _finishTransfer
    fn finish_transfer(&mut self, gb: &mut Gb, ls: &mut GbSioLockstep, timing: &mut Timing) {
        if self.transfer_finished {
            return;
        }
        let id = ls.node_id(self.token) as usize;
        gb.sio.pending_sb = ls.pending_sb[1 - id];
        if gb.memory.io[GB_REG_SC as usize] & GB_REG_SC_ENABLE != 0 {
            gb.sio.remaining_bits = 8;
            timing.deschedule(EventId::Sio.into());
            timing.schedule(EventId::Sio.into(), EventId::Sio.priority(), 0);
        }
        self.transfer_finished = true;
    }

    /// _masterUpdate
    fn master_update(&mut self, gb: &mut Gb, ls: &mut GbSioLockstep, timing: &mut Timing) -> i32 {
        let id = ls.node_id(self.token) as usize;
        // Resume from the parked mLockstep::wait (see `waiting_on_slaves`).
        if self.waiting_on_slaves {
            if ls.master_wait_mask != 0 {
                // Still blocked; keep yielding like the C-threaded master.
                return 0;
            }
            self.waiting_on_slaves = false;
            let diff = ls.node_event_diff[id];
            ls.add_cycles(0, diff);
            // needsToWait was true when we parked
            return 0;
        }
        let mut needs_to_wait = false;
        match ls.transfer_active {
            LockstepPhase::Idle => {
                // If the master hasn't initiated a transfer, it can keep going.
                ls.node_next_event[id] += LOCKSTEP_INCREMENT;
            }
            LockstepPhase::Starting => {
                // Start the transfer, but wait for the other GBs to catch up
                self.transfer_finished = false;
                needs_to_wait = true;
                ls.transfer_active = LockstepPhase::Started;
                ls.node_next_event[id] += 4;
            }
            LockstepPhase::Started => {
                // All the other GBs have caught up and are sleeping, we can all continue now
                ls.node_next_event[id] += 4;
                ls.transfer_active = LockstepPhase::Finishing;
            }
            LockstepPhase::Finishing => {
                // Finish the transfer
                // We need to make sure the other GBs catch up so they don't get behind
                ls.node_next_event[id] += gb.sio.period * (2 - gb.double_speed as i32) - 8; // Split the cycles to avoid waiting too long
                needs_to_wait = true;
                ls.transfer_active = LockstepPhase::Finished;
            }
            LockstepPhase::Finished => {
                // Everything's settled. We're done.
                self.finish_transfer(gb, ls, timing);
                ls.master_claimed = false;
                ls.node_next_event[id] += LOCKSTEP_INCREMENT;
                ls.transfer_active = LockstepPhase::Idle;
            }
        }
        let mut mask: u32 = 0;
        for i in 1..ls.attached {
            mask |= 1u32 << i;
        }
        if mask != 0 {
            if needs_to_wait {
                if !ls.wait(mask) {
                    panic!("GBSIO lockstep: mLockstep wait failed"); // abort() in the C
                }
                // Parked: resume from the top of this function on a later
                // node event once the slaves have signaled.
                self.waiting_on_slaves = true;
                return 0;
            } else {
                ls.signal(mask);
            }
        }
        // Tell the other GBs they can continue up to where we were
        let diff = ls.node_event_diff[id];
        ls.add_cycles(0, diff);
        if needs_to_wait {
            return 0;
        }
        ls.node_next_event[id]
    }

    /// _slaveUpdate
    fn slave_update(
        &mut self,
        gb: &mut Gb,
        ls: &mut GbSioLockstep,
        timing: &mut Timing,
        id: usize,
    ) -> i32 {
        let event_diff = ls.node_event_diff[id];
        let mut signal = false;
        match ls.transfer_active {
            LockstepPhase::Idle => {
                ls.add_cycles(id, LOCKSTEP_INCREMENT);
            }
            LockstepPhase::Starting | LockstepPhase::Finishing => {}
            LockstepPhase::Started => {
                if ls.unused_cycles(id) <= event_diff {
                    self.transfer_finished = false;
                    signal = true;
                }
            }
            LockstepPhase::Finished => {
                if ls.unused_cycles(id) <= event_diff {
                    self.finish_transfer(gb, ls, timing);
                    signal = true;
                }
            }
        }
        if signal {
            ls.signal(1u32 << id);
        }
        0
    }

    /// _GBSIOLockstepNodeProcessEvents (the node's own mTimingEvent callback,
    /// priority 0x80 in C)
    fn process_events(&mut self, gb: &mut Gb, timing: &mut Timing, cycles_late: u32) {
        let Some(rc) = self.p.upgrade() else {
            return;
        };
        let mut ls = rc.borrow_mut();
        if ls.attached < 2 {
            let when = (GB_SIO_CYCLES_PER_TRANSFER[0] >> 1) * (2 - gb.double_speed as i32)
                - cycles_late as i32;
            timing.schedule(
                GB_SIO_LOCKSTEP_EVENT_ID,
                GB_SIO_LOCKSTEP_EVENT_PRIORITY,
                when,
            );
            return;
        }
        let id = ls.node_id(self.token) as usize;
        let mut cycles: i32 = 0;
        ls.node_next_event[id] = ls.node_next_event[id].wrapping_sub(cycles_late as i32);
        if ls.node_next_event[id] <= 0 {
            if id == 0 {
                cycles = self.master_update(gb, &mut ls, timing);
            } else {
                cycles = self.slave_update(gb, &mut ls, timing, id);
                let diff = ls.node_event_diff[id];
                cycles += ls.use_cycles(id, diff);
            }
            ls.node_event_diff[id] = 0;
        } else {
            cycles = ls.node_next_event[id];
        }
        // mLockstepUnlock
        drop(ls);

        if cycles > 0 {
            let mut ls = rc.borrow_mut();
            ls.node_next_event[id] = 0;
            ls.node_event_diff[id] += cycles;
            drop(ls);
            timing.deschedule(GB_SIO_LOCKSTEP_EVENT_ID);
            timing.schedule(
                GB_SIO_LOCKSTEP_EVENT_ID,
                GB_SIO_LOCKSTEP_EVENT_PRIORITY,
                cycles,
            );
        } else {
            // GBInterrupt
            gb.gb_interrupt();
            timing.schedule(
                GB_SIO_LOCKSTEP_EVENT_ID,
                GB_SIO_LOCKSTEP_EVENT_PRIORITY,
                cycles_late as i32 + 1,
            );
        }
    }
}

impl Gb {
    /// Dispatch entry for the lockstep node event. Wire to
    /// `GB_SIO_LOCKSTEP_EVENT_ID` in `Gb::process_event` (gb.rs) when linked
    /// play is integrated.
    pub fn sio_lockstep_event(&mut self, timing: &mut Timing, cycles_late: u32) {
        let driver = std::mem::replace(&mut self.sio.driver, SioDriver::None);
        let SioDriver::Lockstep(mut node) = driver else {
            self.sio.driver = driver;
            return;
        };
        node.process_events(self, timing, cycles_late);
        self.sio.driver = SioDriver::Lockstep(node);
    }

    /// Convenience: GBSIOLockstepAttachNode + GBSIOSetDriver on this core.
    pub fn sio_attach_lockstep(&mut self, lockstep: &Rc<RefCell<GbSioLockstep>>) -> bool {
        let Some(node) = GbSioLockstep::attach_node(lockstep) else {
            return false;
        };
        self.sio_set_driver(Some(SioDriver::Lockstep(node)));
        true
    }
}
