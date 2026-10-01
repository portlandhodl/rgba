// Copyright (c) 2013-2018 Jeffrey Pfau (mGBA), MPL-2.0.
// Tests for the GBA BattleChip Gate (mgba/src/gba/extra/battlechip.c port).
//
// No ROM is used: the gate is an SIO driver, so the games' side of the
// protocol is driven through the sio_write_* register entry points and the
// transfer-complete event (EventId::Sio), exactly like the C paths. Reply
// words are observed in SIOMULTI1, where GBASIOMultiplayerFinishTransfer
// drops data[1].

use rgba_gba::cart::battlechip::{
    BattleChipGate, BattlechipFlavor, BATTLECHIP_CONTINUE, BATTLECHIP_OK, BEAST_LINK_GATE_OK,
    BEAST_LINK_GATE_US_OK, PROGRESS_GATE_OK,
};
use rgba_gba::gba::Gba;
use rgba_gba::io::*;
use rgba_gba::sio::SioDriver;

fn step(gba: &mut Gba, cycles: i32) {
    gba.cpu.cycles += cycles;
    gba.process_events();
}

fn attach(flavor: BattlechipFlavor) -> Box<Gba> {
    let mut gba = Gba::new();
    let mut gate = BattleChipGate::new();
    gate.flavor = flavor;
    gba.attach_battlechip_gate(gate);
    gba
}

/// MULTI mode select, like a game's RCNT=0; SIOCNT=0x2xxx dance.
const SIOCNT_MULTI: u16 = 0x2000;

fn enter_multi(gba: &mut Gba) {
    gba.sio_write_rcnt(0);
    gba.sio_write_siocnt(SIOCNT_MULTI);
}

/// One MULTI exchange: load SIOMLT_SEND, raise START, run the transfer out,
/// and return the gate's reply from SIOMULTI1.
///
/// MULTI at baud 0 with one connected device takes 63427 cycles
/// (GBA_SIO_CYCLES_PER_TRANSFER[0][1]); leave slack.
const MULTI_TRANSFER_CYCLES: i32 = 63427 + 4096;

fn exchange(gba: &mut Gba, cmd: u16) -> u16 {
    gba.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = cmd;
    gba.sio_write_siocnt(SIOCNT_MULTI | 0x80);
    step(gba, MULTI_TRANSFER_CYCLES);
    assert_eq!(gba.sio.siocnt & 0x80, 0, "busy bit cleared after transfer");
    gba.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize]
}

fn data32(gba: &Gba) -> (u16, u16) {
    (
        gba.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize],
        gba.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize],
    )
}

/// The full handshake + command + reply trace, matching the C state machine
/// phase by phase (SYNC → COMMAND → … → END). Returns the gate's replies.
fn full_sequence(gba: &mut Gba, chip_cmd: u16) -> [u16; 10] {
    [
        exchange(gba, 0x8FFF),    // handshake out of SYNC
        exchange(gba, chip_cmd),  // COMMAND
        exchange(gba, 0),         // UNK_0
        exchange(gba, 0),         // UNK_1
        exchange(gba, 0),         // DATA_0
        exchange(gba, 0),         // DATA_1
        exchange(gba, 0),         // ID
        exchange(gba, 0),         // UNK_2
        exchange(gba, 0),         // UNK_3
        exchange(gba, 0),         // END
    ]
}

#[test]
fn battlechip_sync_waits_for_handshake() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);
    enter_multi(&mut gba);

    // Junk commands in SYNC get the greeting without leaving SYNC...
    assert_eq!(exchange(&mut gba, 0x1234), BATTLECHIP_OK);
    assert_eq!(exchange(&mut gba, 0xDEAD), BATTLECHIP_OK);
    // ...and so do the EXE resync commands while already synced.
    assert_eq!(exchange(&mut gba, 0xA380), BATTLECHIP_OK);
    assert_eq!(exchange(&mut gba, 0xA6C0), BATTLECHIP_OK);
}

#[test]
fn battlechip_full_command_sequence() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);
    enter_multi(&mut gba);

    let replies = full_sequence(&mut gba, 0xA390);
    assert_eq!(
        replies,
        [
            BATTLECHIP_OK,       // handshake
            BATTLECHIP_OK,       // command accepted
            BATTLECHIP_CONTINUE, // UNK_0
            BATTLECHIP_CONTINUE, // UNK_1
            0x00FE,              // DATA_0 (seeded at init)
            0xFFFE,              // DATA_1 (seeded at init)
            0x0000,              // no chip inserted
            0x0000,              // UNK_2
            0x0000,              // UNK_3
            BATTLECHIP_OK,       // END
        ]
    );

    // END lands on COMMAND +1 = COMMAND-ish (state 0 → see the C's END arm
    // followed by `++gate->state`), so a back-to-back sequence consumes the
    // handshake word *as* the next command: the whole trace shifts by one
    // phase, and the DATA counters have advanced by 3.
    let replies2 = full_sequence(&mut gba, 0x0001);
    assert_eq!(
        replies2,
        [
            BATTLECHIP_OK,       // state 0 (handshake word eaten as command)
            BATTLECHIP_CONTINUE, // UNK_0
            BATTLECHIP_CONTINUE, // UNK_1
            0x0001,              // DATA_0 wraps mod 0x100: (0xFE+3)&0xFF
            0xFFFB,              // DATA_1 counts down: 0xFFFE-3
            0x0000,              // no chip inserted
            0x0000,              // UNK_2
            0x0000,              // UNK_3
            BATTLECHIP_OK,       // END
            BATTLECHIP_OK,       // state 0 again
        ]
    );
}

#[test]
fn battlechip_resync_mid_sequence() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);
    enter_multi(&mut gba);

    assert_eq!(exchange(&mut gba, 0x8FFF), BATTLECHIP_OK); // SYNC → COMMAND
    assert_eq!(exchange(&mut gba, 0xA3B0), BATTLECHIP_OK); // COMMAND → UNK_0
    // Resync commands only act outside COMMAND (EXE 4 code listed here):
    assert_eq!(exchange(&mut gba, 0xA6C0), BATTLECHIP_OK); // UNK_0 → SYNC
    // Back to SYNC: junk no longer advances the protocol.
    assert_eq!(exchange(&mut gba, 0x0000), BATTLECHIP_OK);
    // Re-handshake; the following replies restart from COMMAND.
    assert_eq!(exchange(&mut gba, 0x8FFF), BATTLECHIP_OK);
    assert_eq!(exchange(&mut gba, 0xA3B0), BATTLECHIP_OK);
    assert_eq!(exchange(&mut gba, 0), BATTLECHIP_CONTINUE); // UNK_0 again
    assert_eq!(exchange(&mut gba, 0), BATTLECHIP_CONTINUE); // UNK_1
}

#[test]
fn battlechip_chip_insertion() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);
    enter_multi(&mut gba);

    if let SioDriver::Battlechip(gate) = &mut gba.sio.driver {
        gate.insert_chip(0x0163);
    } else {
        panic!("gate not installed");
    }
    let replies = full_sequence(&mut gba, 0);
    assert_eq!(replies[6], 0x0163, "ID phase must report the chip");

    if let SioDriver::Battlechip(gate) = &mut gba.sio.driver {
        gate.remove_chip();
    }
    let replies = full_sequence(&mut gba, 0);
    assert_eq!(replies[6], 0x0000, "removed chip reads back as id 0");
}

#[test]
fn battlechip_flavor_ok_codes() {
    for (flavor, ok) in [
        (BattlechipFlavor::BattlechipGate, BATTLECHIP_OK),
        (BattlechipFlavor::ProgressGate, PROGRESS_GATE_OK),
        (BattlechipFlavor::BeastLinkGate, BEAST_LINK_GATE_OK),
        (BattlechipFlavor::BeastLinkGateUs, BEAST_LINK_GATE_US_OK),
    ] {
        let mut gba = attach(flavor);
        enter_multi(&mut gba);
        assert_eq!(exchange(&mut gba, 0x8FFF), ok, "handshake for {flavor:?}");
        assert_eq!(exchange(&mut gba, 0xA390), ok, "command ok for {flavor:?}");
        assert_eq!(exchange(&mut gba, 0), BATTLECHIP_CONTINUE);
        assert_eq!(exchange(&mut gba, 0), BATTLECHIP_CONTINUE);
        assert_eq!(exchange(&mut gba, 0), 0x00FE);
        assert_eq!(exchange(&mut gba, 0), 0xFFFE);
        assert_eq!(exchange(&mut gba, 0), 0x0000);
        assert_eq!(exchange(&mut gba, 0), 0x0000);
        assert_eq!(exchange(&mut gba, 0), 0x0000);
        assert_eq!(exchange(&mut gba, 0), ok, "END for {flavor:?}");
    }
}

#[test]
fn battlechip_siocnt_shaping() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);

    // writeSIOCNT clears 0xC and sets 0x8 (the lines the gate drives).
    gba.sio_write_rcnt(0);
    gba.sio_write_siocnt(SIOCNT_MULTI);
    assert_eq!(gba.sio.siocnt & 0xC, 0x8, "ready line forced by the gate");

    // The gate also claims NORMAL_32 (handlesMode), but has no
    // finishNormal32 hook: the transfer completes with a zero reply,
    // exactly like the C's null-hook path.
    gba.sio_write_siocnt(0x1000);
    assert_eq!(gba.sio.siocnt & 0xC, 0x8);
    gba.sio_write_siocnt(0x1000 | 0x80 | 0x01); // START + internal clock
    step(&mut gba, 8192);
    assert_eq!(gba.sio.siocnt & 0x80, 0, "start bit cleared at finish");
    assert_eq!(data32(&gba), (0, 0), "no finishNormal32 hook → zero data");
}

#[test]
fn battlechip_detach_restores_dummy_driver() {
    let mut gba = attach(BattlechipFlavor::BattlechipGate);
    enter_multi(&mut gba);
    assert_eq!(exchange(&mut gba, 0x8FFF), BATTLECHIP_OK);

    gba.detach_battlechip_gate();
    assert!(matches!(gba.sio.driver, SioDriver::None));
    // Dummy path: replies are zeroed (data[] is never filled by a driver).
    assert_eq!(exchange(&mut gba, 0x8FFF), 0);
}
