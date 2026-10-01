// End-to-end smoke test: the GDB stub over a real loopback TCP connection
// against a real (ROM-less, HLE) Gba.

use rgba_debugger::debugger::*;
use rgba_debugger::gdb::{GdbStub, WatchpointsBehavior};
use rgba_gba::gba::Gba;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0..4].copy_from_slice(&[0x01, 0x00, 0xA0, 0xE3]); // mov r0, #1
    r[4..8].copy_from_slice(&[0x02, 0x10, 0xA0, 0xE3]); // mov r1, #2
    r[8..12].copy_from_slice(&[0xFE, 0xFF, 0xFF, 0xEA]); // b .
    r
}

fn pkt(payload: &str) -> Vec<u8> {
    let checksum = payload.bytes().fold(0u8, |a, b| a.wrapping_add(b));
    let mut out = b"$".to_vec();
    out.extend_from_slice(payload.as_bytes());
    out.push(b'#');
    out.extend_from_slice(format!("{checksum:02x}").as_bytes());
    out
}

fn recv_all(client: &mut TcpStream) -> Vec<u8> {
    let mut buf = [0u8; 4096];
    let mut out = Vec::new();
    loop {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("recv: {e}"),
        }
    }
    out
}

fn recv_until(client: &mut TcpStream, end: u8) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 4096];
    for _ in 0..100 {
        match client.read(&mut buf) {
            Ok(0) => panic!("eof"),
            Ok(n) => {
                all.extend_from_slice(&buf[..n]);
                if all.contains(&end) {
                    return all;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("recv: {e}"),
        }
    }
    panic!("no reply: {:?}", String::from_utf8_lossy(&all));
}

fn pump(gba: &mut Gba, ms: i32) {
    gba.debugger_run_timeout(ms);
}

fn update(gba: &mut Gba) {
    gba.debugger_update();
}

#[test]
fn gdb_tcp_smoke() {
    let mut gba = Gba::new();
    gba.load_rom(rom());
    gba.arm_reset();
    gba.debugger_attach();

    let mut stub = GdbStub::create();
    stub.listen(0, WatchpointsBehavior::StandardLogic).unwrap();
    let port = stub.local_addr().unwrap().port();

    {
        let mut dbg = gba.debugger.take().unwrap();
        dbg.core.attach_module(&mut *gba, stub);
        gba.debugger = Some(dbg);
    }

    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client.set_nonblocking(true).unwrap();

    // Pump update() -> accept the connection (DEBUGGER_ENTER_ATTACHED).
    update(&mut gba);
    {
        let dbg = gba.debugger.take().unwrap();
        assert_eq!(dbg.core.state, DebuggerState::Paused);
        gba.debugger = Some(dbg);
    }

    // '?' -> ack + S02
    client.write_all(&pkt("?")).unwrap();
    pump(&mut gba, 20);
    let got = recv_until(&mut client, b'#');
    let got = { let mut g2 = got; g2.extend_from_slice(&recv_all(&mut client)); g2 };
    let mut exp = b"+".to_vec();
    exp.extend_from_slice(&pkt("S02"));
    assert_eq!(got, exp, "{}", String::from_utf8_lossy(&got));

    // 'g' -> 17 registers; r0 must be 0 at this point, pc adjusted to 0x08000000
    client.write_all(&pkt("g")).unwrap();
    pump(&mut gba, 20);
    let got = recv_until(&mut client, b'#');
    let got = { let mut g2 = got; g2.extend_from_slice(&recv_all(&mut client)); g2 };
    let text = String::from_utf8_lossy(&got);
    assert!(text.starts_with("+$"), "{text}");
    assert_eq!(text.len(), 2 + 17 * 8 + 3, "{text}");
    // pc slot = 15 * 8 .. +8; reset state: pc reported = 0x08000000 (r15 - 4)
    assert_eq!(&text[2 + 15 * 8..2 + 16 * 8], "00000008", "{text}");

    // 'M' write to EWRAM + verify via console + 'm' read back
    client.write_all(&pkt("M2000000,4:0badf00d")).unwrap();
    pump(&mut gba, 20);
    let got = recv_until(&mut client, b'#');
    let got = { let mut g2 = got; g2.extend_from_slice(&recv_all(&mut client)); g2 };
    let mut exp = b"+".to_vec();
    exp.extend_from_slice(&pkt("OK"));
    assert_eq!(got, exp, "{}", String::from_utf8_lossy(&got));
    assert_eq!(gba.view32(0x02000000), 0x0df0ad0b); // byte sequence 0b ad f0 0d

    client.write_all(&pkt("m2000000,4")).unwrap();
    pump(&mut gba, 20);
    let got = recv_until(&mut client, b'#');
    let got = { let mut g2 = got; g2.extend_from_slice(&recv_all(&mut client)); g2 };
    let mut exp = b"+".to_vec();
    exp.extend_from_slice(&pkt("0badf00d"));
    assert_eq!(got, exp, "{}", String::from_utf8_lossy(&got));

    // 'Z0' breakpoint at the `b .` instruction, then 'c' -> runs into it
    client.write_all(&pkt("Z0,8000008")).unwrap();
    pump(&mut gba, 20);
    let _ = recv_all(&mut client); // +OK
    client.write_all(&pkt("c")).unwrap();
    let mut stop = Vec::new();
    for i in 0..200 {
        pump(&mut gba, 1);
        stop.extend_from_slice(&recv_all(&mut client));
        if stop.windows(4).any(|w| w == b"S05k") {
            break;
        }
    }
    {
        let dbg = gba.debugger.take().unwrap();
        assert_eq!(dbg.core.state, DebuggerState::Paused);
        gba.debugger = Some(dbg);
    }
    let text = String::from_utf8_lossy(&stop);
    assert!(text.contains("S05k"), "expected stop reply, got {text:?}");
    assert_eq!(gba.cpu.gprs[0], 1);
    assert_eq!(gba.cpu.gprs[1], 2);
}
