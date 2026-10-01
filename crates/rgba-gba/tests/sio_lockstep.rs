// Copyright (c) 2013-2024 Jeffrey Pfau (mGBA), MPL-2.0.
// Tests for the GBA link-cable lockstep (mgba/src/gba/sio/lockstep.c port).
//
// No ROM is used: the two linked cores are stepped by feeding cpu.cycles
// into process_events, and register traffic goes through the sio_write_*
// entry points, mirroring how the C driver paths are exercised.

use rgba_gba::gba::Gba;
use rgba_gba::io::*;
use rgba_gba::sio::lockstep::GbaSioLockstep;

fn step(gba: &mut Gba, cycles: i32) {
    gba.cpu.cycles += cycles;
    gba.process_events();
}

/// Interleave the two cores in small slices so lockstep sync points
/// (mode-set acks, transfer-start acks, hard syncs) converge.
fn run_linked(a: &mut Gba, b: &mut Gba, cycles: i32) {
    let mut remaining = cycles;
    while remaining > 0 {
        let slice = remaining.min(32);
        step(a, slice);
        step(b, slice);
        remaining -= slice;
    }
}

fn linked_pair() -> (
    Box<Gba>,
    Box<Gba>,
    std::rc::Rc<std::cell::RefCell<GbaSioLockstep>>,
) {
    let ls = GbaSioLockstep::new();
    let mut a = Gba::new();
    let mut b = Gba::new();
    ls.borrow(); // coordinator handle kept alive by the caller
    a.sio_attach_lockstep(&ls);
    b.sio_attach_lockstep(&ls);
    (a, b, ls)
}

/// Exactly one of the pair must end up player 0.
fn master_is_a(a: &Gba, b: &Gba) -> bool {
    let ia = a.sio_lockstep_player_id().expect("a attached");
    let ib = b.sio_lockstep_player_id().expect("b attached");
    assert_eq!(ia + ib, 1, "player ids must be 0 and 1, got {ia}/{ib}");
    ia == 0
}

/// Put both into the mode selected by the SIOCNT mode bits (0x1000 =
/// NORMAL32, 0x2000 = MULTI, ...), like a game's RCNT=0; SIOCNT=mode dance.
fn set_modes(a: &mut Gba, b: &mut Gba, siocnt_mode: u16) {
    for g in [&mut *a, &mut *b] {
        g.sio_write_rcnt(0);
        g.sio_write_siocnt(siocnt_mode);
    }
    run_linked(a, b, 8192);
}

fn data32(gba: &Gba) -> (u16, u16) {
    (
        gba.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize],
        gba.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize],
    )
}

#[test]
fn lockstep_normal32_exchange() {
    let (mut a, mut b, ls) = linked_pair();
    assert_eq!(ls.borrow().attached(), 2);
    let roles_swapped = !master_is_a(&a, &b);
    set_modes(&mut a, &mut b, 0x1000);

    let (m, s) = if roles_swapped {
        (&mut *b, &mut *a)
    } else {
        (&mut *a, &mut *b)
    };
    m.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0xBEEF;
    m.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = 0xCAFE;
    s.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0x1234;
    s.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = 0x5678;

    // START + internal shift clock (256-cycle NORMAL32 transfer)
    m.sio_write_siocnt(0x1000 | 0x80 | 0x01);
    run_linked(&mut a, &mut b, 4096);

    // The slave received the master's data; the master reads back
    // 0xFFFFFFFF (as mGBA's finishNormal32 delivers to player 0).
    let (m, s) = if roles_swapped {
        (&*b, &*a)
    } else {
        (&*a, &*b)
    };
    assert_eq!(
        data32(s),
        (0xBEEF, 0xCAFE),
        "slave must receive master data"
    );
    assert_eq!(
        data32(m),
        (0xFFFF, 0xFFFF),
        "master reads open bus in normal32"
    );
    assert_eq!(m.sio.siocnt & 0x80, 0, "master busy cleared");
    assert_eq!(s.sio.siocnt & 0x80, 0, "slave busy cleared");
}

#[test]
fn lockstep_normal8_exchange() {
    let (mut a, mut b, _ls) = linked_pair();
    let roles_swapped = !master_is_a(&a, &b);
    set_modes(&mut a, &mut b, 0x0000); // NORMAL8

    let (m, s) = if roles_swapped {
        (&mut *b, &mut *a)
    } else {
        (&mut *a, &mut *b)
    };
    // SIODATA8 shares its address with SIOMLT_SEND
    m.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = 0x5A;
    s.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = 0xA5;

    m.sio_write_siocnt(0x0000 | 0x80 | 0x01);
    run_linked(&mut a, &mut b, 4096);

    let (m, s) = if roles_swapped {
        (&*b, &*a)
    } else {
        (&*a, &*b)
    };
    assert_eq!(
        s.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] & 0xFF,
        0x5A,
        "slave must receive master byte"
    );
    assert_eq!(m.sio.siocnt & 0x80, 0);
    assert_eq!(s.sio.siocnt & 0x80, 0);
}

#[test]
fn lockstep_multiplayer_exchange() {
    let (mut a, mut b, _ls) = linked_pair();
    let roles_swapped = !master_is_a(&a, &b);
    set_modes(&mut a, &mut b, 0x2000); // MULTI

    // Once both players agree on MULTI the ready bit (and SD) go up.
    assert_ne!(a.sio.siocnt & 0x8, 0, "player A ready in multi mode");
    assert_ne!(b.sio.siocnt & 0x8, 0, "player B ready in multi mode");
    assert_ne!(a.sio.siocnt & 0x30, b.sio.siocnt & 0x30, "different ids");
    let (m, s) = if roles_swapped {
        (&mut *b, &mut *a)
    } else {
        (&mut *a, &mut *b)
    };
    m.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = 0x1111;
    s.memory.io[(GBA_REG_SIOMLT_SEND >> 1) as usize] = 0x2222;

    m.sio_write_siocnt(0x2080);
    // MULTI at baud 0 with one peer is 63427 cycles; leave sync slack.
    run_linked(&mut a, &mut b, 63427 + 8192);

    let (m, s) = if roles_swapped {
        (&*b, &*a)
    } else {
        (&*a, &*b)
    };
    for g in [m, s] {
        assert_eq!(g.memory.io[(GBA_REG_SIOMULTI0 >> 1) as usize], 0x1111);
        assert_eq!(g.memory.io[(GBA_REG_SIOMULTI1 >> 1) as usize], 0x2222);
        assert_eq!(g.memory.io[(GBA_REG_SIOMULTI2 >> 1) as usize], 0xFFFF);
        assert_eq!(g.memory.io[(GBA_REG_SIOMULTI3 >> 1) as usize], 0xFFFF);
        assert_eq!(g.sio.siocnt & 0x80, 0, "busy cleared");
    }
    assert_eq!((m.sio.siocnt >> 4) & 3, 0, "master id bits");
    assert_eq!((s.sio.siocnt >> 4) & 3, 1, "slave id bits");
}

#[test]
fn lockstep_savestate_roundtrip_mid_transfer() {
    let (mut a, mut b, _ls) = linked_pair();
    let roles_swapped = !master_is_a(&a, &b);
    set_modes(&mut a, &mut b, 0x1000);

    {
        let (m, s) = if roles_swapped {
            (&mut *b, &mut *a)
        } else {
            (&mut *a, &mut *b)
        };
        m.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0xBEEF;
        m.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = 0xCAFE;
        s.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0x1234;
        s.memory.io[(GBA_REG_SIODATA32_HI >> 1) as usize] = 0x5678;
        m.sio_write_siocnt(0x1000 | 0x80 | 0x01);
    }

    // Save main + lockstep extdata states mid-transfer.
    let main_a = rgba_gba::serialize::serialize(&mut a).expect("serialize a");
    let main_b = rgba_gba::serialize::serialize(&mut b).expect("serialize b");
    let ext_a = a.sio_save_extra_state().expect("extstate a");
    let ext_b = b.sio_save_extra_state().expect("extstate b");

    // Run to completion and record the outcome.
    run_linked(&mut a, &mut b, 4096);
    let (m, s) = if roles_swapped {
        (&*b, &*a)
    } else {
        (&*a, &*b)
    };
    let outcome = (data32(m), data32(s));

    // Restore both halves mid-transfer and make sure the re-save is
    // byte-identical (deterministic driver state).
    rgba_gba::serialize::deserialize(&mut a, &main_a).expect("deserialize a");
    rgba_gba::serialize::deserialize(&mut b, &main_b).expect("deserialize b");
    assert!(a.sio_load_extra_state(&ext_a));
    assert!(b.sio_load_extra_state(&ext_b));
    assert_eq!(
        a.sio_save_extra_state().unwrap(),
        ext_a,
        "player A extdata must reload byte-identically"
    );
    assert_eq!(
        b.sio_save_extra_state().unwrap(),
        ext_b,
        "player B extdata must reload byte-identically"
    );
    // (The main 0x61000 state is not byte-compared here: with no ROM loaded,
    // cpu.prefetch refills off the open bus, which previous SIO traffic
    // perturbs — a pre-existing, lockstep-independent artifact.)

    // The restored link completes with the same exchanged data.
    run_linked(&mut a, &mut b, 4096);
    let (m, s) = if roles_swapped {
        (&*b, &*a)
    } else {
        (&*a, &*b)
    };
    assert_eq!(data32(s), (0xBEEF, 0xCAFE));
    assert_eq!((data32(m), data32(s)), outcome);
}

#[test]
fn lockstep_detach_leaves_single_player() {
    let (mut a, mut b, ls) = linked_pair();
    set_modes(&mut a, &mut b, 0x1000);

    // Detaching the secondary must not hang the remaining core.
    let (ia, ib) = (
        a.sio_lockstep_player_id().unwrap(),
        b.sio_lockstep_player_id().unwrap(),
    );
    let (secondary, primary) = if ia == 0 {
        (&mut b, &mut a)
    } else {
        (&mut a, &mut b)
    };
    assert_eq!(ib + ia, 1);
    secondary.sio_detach_lockstep();
    assert_eq!(ls.borrow().attached(), 1);
    assert_eq!(primary.sio_lockstep_player_id(), Some(0));

    // Primary keeps running; a further start attempt finds no secondary
    // (driver->start fails, no complete event is scheduled).
    primary.memory.io[(GBA_REG_SIODATA32_LO >> 1) as usize] = 0xBEEF;
    primary.sio_write_siocnt(0x1000 | 0x80 | 0x01);
    step(primary, 4096);
    step(primary, 4096);
    assert_ne!(primary.sio.siocnt & 0x80, 0, "busy never completes solo");
}
