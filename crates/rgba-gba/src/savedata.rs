// Copyright (c) 2013-2015 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/gba/savedata.c and include/mgba/internal/gba/savedata.h.

use rgba_core::{mlog, Level};

use crate::gba::Gba;

pub const GBA_SIZE_SRAM: usize = 0x00008000;
pub const GBA_SIZE_SRAM512: usize = 0x00010000;
pub const GBA_SIZE_FLASH512: usize = 0x00010000;
pub const GBA_SIZE_FLASH1M: usize = 0x00020000;
pub const GBA_SIZE_EEPROM: usize = 0x00002000;
pub const GBA_SIZE_EEPROM512: usize = 0x00000200;

// Erase cycles can vary greatly.
pub const FLASH_ERASE_CYCLES: i32 = 30000;
pub const FLASH_PROGRAM_CYCLES: i32 = 650;
pub const EEPROM_SETTLE_CYCLES: i32 = 115000;

pub const FLASH_BASE_LO: u16 = 0x0;
pub const FLASH_BASE_HI: u16 = 0x5555;
pub const FLASH_BASE_2: u16 = 0x2AAA;

pub const FLASH_MFG_PANASONIC: u16 = 0x1B32;
pub const FLASH_MFG_SANYO: u16 = 0x1362;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SavedataType {
    Autodetect = -1,
    ForceNone = 0,
    Sram = 1,
    Flash512 = 2,
    Flash1M = 3,
    Eeprom = 4,
    Eeprom512 = 5,
    Sram512 = 6,
}

// enum SavedataCommand (EEPROM + Flash)
pub const EEPROM_COMMAND_NULL: u8 = 0;
pub const EEPROM_COMMAND_PENDING: u8 = 1;
pub const EEPROM_COMMAND_WRITE: u8 = 2;
pub const EEPROM_COMMAND_READ_PENDING: u8 = 3;
pub const EEPROM_COMMAND_READ: u8 = 4;

pub const FLASH_COMMAND_START: u8 = 0xAA;
pub const FLASH_COMMAND_CONTINUE: u8 = 0x55;
pub const FLASH_COMMAND_ERASE_CHIP: u8 = 0x10;
pub const FLASH_COMMAND_ERASE_SECTOR: u8 = 0x30;
pub const FLASH_COMMAND_ERASE: u8 = 0x80;
pub const FLASH_COMMAND_ID: u8 = 0x90;
pub const FLASH_COMMAND_PROGRAM: u8 = 0xA0;
pub const FLASH_COMMAND_SWITCH_BANK: u8 = 0xB0;
pub const FLASH_COMMAND_TERMINATE: u8 = 0xF0;
pub const FLASH_COMMAND_NONE: u8 = 0;

pub const FLASH_STATE_RAW: u8 = 0;
pub const FLASH_STATE_START: u8 = 1;
pub const FLASH_STATE_CONTINUE: u8 = 2;

pub struct Savedata {
    pub savedata_type: SavedataType,
    pub data: Vec<u8>,
    pub command: u8,
    pub flash_state: u8,
    pub dirty: i32,
    pub read_bits_remaining: i8,
    pub read_address: u32,
    pub write_address: u32,
    /// Bank selector for 1M flash (0 or 1).
    pub current_bank: usize,
    pub settling: i32,
    /// 2-color sector-settle simulation scheduling state (a `Timing` event on
    /// the GBA side handles the countdown).
    pub settling_pending: i32, // -1 when not scheduled
}

impl Savedata {
    pub fn new() -> Self {
        Savedata {
            savedata_type: SavedataType::Autodetect,
            data: Vec::new(),
            command: EEPROM_COMMAND_NULL,
            flash_state: FLASH_STATE_RAW,
            dirty: 0,
            read_bits_remaining: 0,
            read_address: 0,
            write_address: 0,
            current_bank: 0,
            settling: 0,
            settling_pending: -1,
        }
    }

    pub fn size(&self) -> usize {
        match self.savedata_type {
            SavedataType::Sram => GBA_SIZE_SRAM,
            SavedataType::Sram512 => GBA_SIZE_SRAM512,
            SavedataType::Flash512 => GBA_SIZE_FLASH512,
            SavedataType::Flash1M => GBA_SIZE_FLASH1M,
            SavedataType::Eeprom => GBA_SIZE_EEPROM,
            SavedataType::Eeprom512 => GBA_SIZE_EEPROM512,
            _ => 0,
        }
    }

    pub fn fill_empty(&mut self) {
        // newly-created regions are 0xFF
        for b in self.data.iter_mut() {
            *b = 0xFF;
        }
    }

    pub fn bank_offset(&self) -> usize {
        self.current_bank * 0x10000
    }
}

impl Default for Savedata {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for SavedataType {
    fn default() -> Self { Self::Autodetect }
}

impl SavedataType {
    pub fn is_eeprom(self) -> bool {
        matches!(self, SavedataType::Eeprom | SavedataType::Eeprom512)
    }
    pub fn is_sram(self) -> bool {
        matches!(self, SavedataType::Sram | SavedataType::Sram512)
    }
    pub fn is_flash(self) -> bool {
        matches!(self, SavedataType::Flash512 | SavedataType::Flash1M)
    }
}

impl Gba {
    /// GBASavedataInitFlash
    pub fn savedata_init_flash(&mut self) {
        if self.savedata.savedata_type == SavedataType::Autodetect {
            self.savedata.savedata_type = SavedataType::Flash512;
        }
        if !self.savedata.savedata_type.is_flash() {
            mlog!(Level::Warn, rgba_core::log::GBA_SAVE, "Can't re-initialize savedata");
            return;
        }
        let flash_size = if self.savedata.savedata_type == SavedataType::Flash1M {
            GBA_SIZE_FLASH1M
        } else {
            GBA_SIZE_FLASH512
        };
        self.savedata.data = vec![0xFF; flash_size];
        self.savedata.current_bank = 0;
    }

    /// GBASavedataInitEEPROM
    pub fn savedata_init_eeprom(&mut self) {
        if self.savedata.savedata_type == SavedataType::Autodetect {
            self.savedata.savedata_type = SavedataType::Eeprom512;
        } else if !self.savedata.savedata_type.is_eeprom() {
            mlog!(Level::Warn, rgba_core::log::GBA_SAVE, "Can't re-initialize savedata");
            return;
        }
        let size = if self.savedata.savedata_type == SavedataType::Eeprom {
            GBA_SIZE_EEPROM
        } else {
            GBA_SIZE_EEPROM512
        };
        self.savedata.data = vec![0xFF; size];
    }

    /// GBASavedataInitSRAM
    pub fn savedata_init_sram(&mut self) {
        if self.savedata.savedata_type == SavedataType::Autodetect {
            self.savedata.savedata_type = SavedataType::Sram;
        } else if self.savedata.savedata_type != SavedataType::Sram {
            mlog!(Level::Warn, rgba_core::log::GBA_SAVE, "Can't re-initialize savedata");
            return;
        }
        self.savedata.data = vec![0xFF; GBA_SIZE_SRAM];
    }

    /// GBASavedataInitSRAM512
    pub fn savedata_init_sram512(&mut self) {
        if self.savedata.savedata_type == SavedataType::Autodetect {
            self.savedata.savedata_type = SavedataType::Sram512;
        } else if self.savedata.savedata_type != SavedataType::Sram512 {
            mlog!(Level::Warn, rgba_core::log::GBA_SAVE, "Can't re-initialize savedata");
            return;
        }
        self.savedata.data = vec![0xFF; GBA_SIZE_SRAM512];
    }

    /// GBASavedataForceType
    pub fn savedata_force_type(&mut self, t: SavedataType) {
        if self.savedata.savedata_type == t {
            return;
        }
        self.savedata.savedata_type = t;
        match t {
            SavedataType::Flash512 | SavedataType::Flash1M => self.savedata_init_flash(),
            SavedataType::Eeprom | SavedataType::Eeprom512 => self.savedata_init_eeprom(),
            SavedataType::Sram => self.savedata_init_sram(),
            SavedataType::Sram512 => self.savedata_init_sram512(),
            _ => {}
        }
    }

    /// GBASavedataReset
    pub fn savedata_reset(&mut self) {
        self.savedata.command = EEPROM_COMMAND_NULL;
        self.savedata.flash_state = FLASH_STATE_RAW;
    }

    fn savedata_flash_switch_bank(&mut self, bank: usize) {
        mlog!(Level::Debug, rgba_core::log::GBA_SAVE, "Performing flash bank switch to bank {}", bank);
        if bank > 0 && self.savedata.savedata_type == SavedataType::Flash512 {
            mlog!(Level::Info, rgba_core::log::GBA_SAVE, "Updating flash chip from 512kb to 1Mb");
            self.savedata.savedata_type = SavedataType::Flash1M;
            self.savedata.data.resize(GBA_SIZE_FLASH1M, 0xFF);
        }
        self.savedata.current_bank = bank;
    }

    fn flash_erase(&mut self) {
        mlog!(Level::Debug, rgba_core::log::GBA_SAVE, "Performing flash chip erase");
        self.savedata.dirty |= 1;
        for b in self.savedata.data.iter_mut() {
            *b = 0xFF;
        }
    }

    fn flash_erase_sector(&mut self, sector_start: u16) {
        mlog!(
            Level::Debug,
            rgba_core::log::GBA_SAVE,
            "Performing flash sector erase at 0x{:04x}",
            sector_start
        );
        self.savedata.dirty |= 1;
        let size = 0x1000usize;
        if self.savedata.savedata_type == SavedataType::Flash1M {
            mlog!(Level::Debug, rgba_core::log::GBA_SAVE, "Performing unknown sector-size erase");
        }
        self.savedata.settling = (sector_start as i32) >> 12;
        self.savedata.settling_pending = FLASH_ERASE_CYCLES;
        let bank = self.savedata.bank_offset();
        let base = bank + (sector_start as usize & !(size - 1));
        for b in self.savedata.data[base..base + size].iter_mut() {
            *b = 0xFF;
        }
    }

    /// GBASavedataReadFlash
    pub fn savedata_read_flash(&self, address: u16) -> u8 {
        if self.savedata.command == FLASH_COMMAND_ID {
            if self.savedata.savedata_type == SavedataType::Flash512 {
                if address < 2 {
                    return (FLASH_MFG_PANASONIC >> (address * 8)) as u8;
                }
            } else if self.savedata.savedata_type == SavedataType::Flash1M {
                if address < 2 {
                    return (FLASH_MFG_SANYO >> (address * 8)) as u8;
                }
            }
        }
        if self.savedata.settling_pending > 0 && (address as i32 >> 12) == self.savedata.settling {
            // Data# polling
            return (self.savedata.data[self.savedata.bank_offset() + address as usize] ^ 0x80) & 0x80;
        }
        self.savedata
            .data
            .get(self.savedata.bank_offset() + address as usize)
            .copied()
            .unwrap_or(0xFF)
    }

    /// GBASavedataWriteFlash
    pub fn savedata_write_flash(&mut self, address: u16, value: u8) {
        match self.savedata.flash_state {
            FLASH_STATE_RAW => match self.savedata.command {
                FLASH_COMMAND_PROGRAM => {
                    self.savedata.dirty |= 1;
                    let off = self.savedata.bank_offset() + address as usize;
                    if off < self.savedata.data.len() {
                        self.savedata.data[off] = value;
                    }
                    self.savedata.command = FLASH_COMMAND_NONE;
                    self.savedata.settling_pending = FLASH_PROGRAM_CYCLES;
                }
                FLASH_COMMAND_SWITCH_BANK => {
                    if address == 0 && value < 2 {
                        self.savedata_flash_switch_bank(value as usize);
                    } else {
                        mlog!(Level::GameError, rgba_core::log::GBA_SAVE, "Bad flash bank switch");
                    }
                    self.savedata.command = FLASH_COMMAND_NONE;
                }
                _ => {
                    if address == 0x5555 && value == FLASH_COMMAND_START {
                        self.savedata.flash_state = FLASH_STATE_START;
                    } else {
                        mlog!(
                            Level::GameError,
                            rgba_core::log::GBA_SAVE,
                            "Bad flash write: {:#04x} = {:#02x}",
                            address,
                            value
                        );
                    }
                }
            },
            FLASH_STATE_START => {
                if address == 0x2AAA && value == FLASH_COMMAND_CONTINUE {
                    self.savedata.flash_state = FLASH_STATE_CONTINUE;
                } else {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GBA_SAVE,
                        "Bad flash write: {:#04x} = {:#02x}",
                        address,
                        value
                    );
                    self.savedata.flash_state = FLASH_STATE_RAW;
                }
            }
            FLASH_STATE_CONTINUE => {
                self.savedata.flash_state = FLASH_STATE_RAW;
                if address == 0x5555 {
                    match self.savedata.command {
                        FLASH_COMMAND_NONE => {
                            match value {
                                FLASH_COMMAND_ERASE
                                | FLASH_COMMAND_ID
                                | FLASH_COMMAND_PROGRAM
                                | FLASH_COMMAND_SWITCH_BANK => {
                                    self.savedata.command = value;
                                }
                                _ => {
                                    mlog!(
                                        Level::GameError,
                                        rgba_core::log::GBA_SAVE,
                                        "Unsupported flash operation: {:#02x}",
                                        value
                                    );
                                }
                            }
                        }
                        FLASH_COMMAND_ERASE => {
                            if value == FLASH_COMMAND_ERASE_CHIP {
                                self.flash_erase();
                            } else {
                                mlog!(
                                    Level::GameError,
                                    rgba_core::log::GBA_SAVE,
                                    "Unsupported flash erase operation: {:#02x}",
                                    value
                                );
                            }
                            self.savedata.command = FLASH_COMMAND_NONE;
                        }
                        FLASH_COMMAND_ID => {
                            if value == FLASH_COMMAND_TERMINATE {
                                self.savedata.command = FLASH_COMMAND_NONE;
                            }
                        }
                        _ => {
                            mlog!(
                                Level::Error,
                                rgba_core::log::GBA_SAVE,
                                "Flash entered bad state: {:#02x}",
                                self.savedata.command
                            );
                            self.savedata.command = FLASH_COMMAND_NONE;
                        }
                    }
                } else if self.savedata.command == FLASH_COMMAND_ERASE {
                    if value == FLASH_COMMAND_ERASE_SECTOR {
                        self.flash_erase_sector(address);
                        self.savedata.command = FLASH_COMMAND_NONE;
                    } else {
                        mlog!(
                            Level::GameError,
                            rgba_core::log::GBA_SAVE,
                            "Unsupported flash erase operation: {:#02x}",
                            value
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn ensure_eeprom(&mut self, size: u32) {
        if size < GBA_SIZE_EEPROM512 as u32 {
            return;
        }
        if self.savedata.savedata_type == SavedataType::Eeprom {
            return;
        }
        self.savedata.savedata_type = SavedataType::Eeprom;
        if self.savedata.data.len() < GBA_SIZE_EEPROM {
            self.savedata.data.resize(GBA_SIZE_EEPROM, 0xFF);
        }
    }

    /// GBASavedataWriteEEPROM
    pub fn savedata_write_eeprom(&mut self, value: u16, write_size: u32) {
        match self.savedata.command {
            EEPROM_COMMAND_NULL => {
                self.savedata.command = (value & 1) as u8;
            }
            EEPROM_COMMAND_PENDING => {
                self.savedata.command <<= 1;
                self.savedata.command |= (value & 1) as u8;
                if self.savedata.command == EEPROM_COMMAND_WRITE {
                    self.savedata.write_address = 0;
                } else {
                    self.savedata.read_address = 0;
                }
            }
            EEPROM_COMMAND_WRITE => {
                // Write
                if write_size > 65 {
                    self.savedata.write_address <<= 1;
                    self.savedata.write_address |= ((value & 1) as u32) << 6;
                } else if write_size == 1 {
                    self.savedata.command = EEPROM_COMMAND_NULL;
                } else if (self.savedata.write_address >> 3) < GBA_SIZE_EEPROM as u32 {
                    self.ensure_eeprom(self.savedata.write_address >> 3);
                    let addr = (self.savedata.write_address >> 3) as usize;
                    let mut current = self.savedata.data.get(addr).copied().unwrap_or(0xFF);
                    current &= !(1u8 << (0x7 - (self.savedata.write_address & 0x7)));
                    current |= ((value & 1) << (0x7 - (self.savedata.write_address & 0x7))) as u8;
                    self.savedata.dirty |= 1;
                    self.savedata.data[addr] = current;
                    self.savedata.settling_pending = EEPROM_SETTLE_CYCLES;
                    self.savedata.write_address += 1;
                } else {
                    mlog!(
                        Level::GameError,
                        rgba_core::log::GBA_SAVE,
                        "Writing beyond end of EEPROM: {:08X}",
                        self.savedata.write_address >> 3
                    );
                }
            }
            EEPROM_COMMAND_READ_PENDING => {
                // Read
                if write_size > 1 {
                    self.savedata.read_address <<= 1;
                    if value & 1 != 0 {
                        self.savedata.read_address |= 0x40;
                    }
                } else {
                    self.savedata.read_bits_remaining = 68;
                    self.savedata.command = EEPROM_COMMAND_READ;
                }
            }
            _ => {}
        }
    }

    /// GBASavedataReadEEPROM
    pub fn savedata_read_eeprom(&mut self) -> u16 {
        if self.savedata.command != EEPROM_COMMAND_READ {
            if self.savedata.settling_pending <= 0 {
                return 1;
            } else {
                return 0;
            }
        }
        self.savedata.read_bits_remaining -= 1;
        if self.savedata.read_bits_remaining < 64 {
            let step = 63 - self.savedata.read_bits_remaining;
            let address = (self.savedata.read_address + step as u32) >> 3;
            self.ensure_eeprom(address);
            if address >= GBA_SIZE_EEPROM as u32 {
                mlog!(
                    Level::GameError,
                    rgba_core::log::GBA_SAVE,
                    "Reading beyond end of EEPROM: {:08X}",
                    address
                );
                return 0xFF;
            }
            let v = self.savedata.data[address as usize];
            let data = v as u8 >> (0x7 - (step & 0x7));
            if self.savedata.read_bits_remaining == 0 {
                self.savedata.command = EEPROM_COMMAND_NULL;
            }
            return (data & 0x1) as u16;
        }
        0
    }

    /// SRAM read/write through the generic address path (memory.c's 0xE..
    /// delegation).  Ports of the C's behavior in the GBA_REGION_SRAM branch.
    pub fn savedata_sram_read8(&mut self, _address: u32) -> u32 {
        match self.savedata.savedata_type {
            SavedataType::Autodetect => {
                mlog!(Level::Info, rgba_core::log::GBA_MEM, "Detected SRAM savegame");
                self.savedata_init_sram();
            }
            _ => {}
        }
        if self.performing_dma == 1 {
            return 0;
        }
        if self.hw_has_ereader() && (_address & 0xE00FF80) >= 0xE00FF80 {
            return self.ereader_read_flash(_address) as u32;
        }
        match self.savedata.savedata_type {
            SavedataType::Sram => {
                self.savedata.data[(_address as usize) & (GBA_SIZE_SRAM - 1)] as u32
            }
            SavedataType::Flash512 | SavedataType::Flash1M => {
                self.savedata_read_flash((_address as u16)) as u32
            }
            SavedataType::Sram512 => {
                self.savedata.data[(_address as usize) & (GBA_SIZE_SRAM512 - 1)] as u32
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Reading from non-existent SRAM: {:08X}", _address);
                0xFF
            }
        }
    }

    pub fn savedata_sram_write8(&mut self, address: u32, value: u8) {
        if self.savedata.savedata_type == SavedataType::Autodetect {
            if address >= SAVEDATA_FLASH_BASE {
                mlog!(Level::Info, rgba_core::log::GBA_MEM, "Detected Flash savegame");
                self.savedata_init_flash();
            } else {
                mlog!(Level::Info, rgba_core::log::GBA_MEM, "Detected SRAM savegame");
                self.savedata_init_sram();
            }
        }
        if self.hw_has_ereader() && (address & 0xE00FF80) >= 0xE00FF80 {
            self.ereader_write_flash(address, value);
            return;
        }
        match self.savedata.savedata_type {
            SavedataType::Flash512 | SavedataType::Flash1M => {
                self.savedata_write_flash((address as u16), value);
            }
            SavedataType::Sram => {
                if self.unl_cart_is_present() {
                    // GBAUnlCartWriteSRAM (cart/unlicensed.rs)
                    self.unl_cart_write_sram(address & 0xFFFF, value);
                } else {
                    let off = (address as usize) & (GBA_SIZE_SRAM - 1);
                    if !self.savedata.data.is_empty() {
                        self.savedata.data[off] = value;
                    }
                }
                self.savedata.dirty |= 1;
            }
            SavedataType::Sram512 => {
                let off = (address as usize) & (GBA_SIZE_SRAM512 - 1);
                if !self.savedata.data.is_empty() {
                    self.savedata.data[off] = value;
                    self.savedata.dirty |= 1;
                }
            }
            _ => {
                mlog!(Level::GameError, rgba_core::log::GBA_MEM, "Writing to non-existent SRAM: {:08X}", address);
            }
        }
    }
}

pub const SAVEDATA_FLASH_BASE: u32 = 0x0E000000;
const SAVEDATA_FLASH_BASE_LO_ADDR: u32 = 0x0E005555 & 0xFFFF;
const SAVEDATA_FLASH_BASE_LO_ADDR_2: u32 = 0x0E002AAA & 0xFFFF;

// Forwarders used by memory.rs in place of the C's direct calls.
impl Gba {
    pub fn savedata_type_is_autodetect(&self) -> bool {
        self.savedata.savedata_type == SavedataType::Autodetect
    }
    pub fn savedata_type_is_eeprom(&self) -> bool {
        self.savedata.savedata_type.is_eeprom()
    }
    pub fn sram_region_read8(&mut self, address: u32) -> u32 {
        self.savedata_sram_read8(address)
    }
    pub fn sram_region_write8(&mut self, address: u32, value: u8) {
        self.savedata_sram_write8(address, value)
    }
}
