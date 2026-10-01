// Rewind context round-trip test (mCoreRewindAppend/mCoreRewindRestore).
use rgba_core::core::{Core, CoreInfo};
use rgba_core::ring::RingI16;
use rgba_core::rewind::RewindContext;
use rgba_core::Platform;

struct MockCore {
    /// The "machine state": a 64-byte blob we atomically update per frame.
    state: [u8; 64],
}

impl Core for MockCore {
    fn platform(&self) -> rgba_core::Platform { Platform::None }
    fn info(&self) -> CoreInfo {
        CoreInfo { platform: Platform::None, width: 1, height: 1, frequency: 1, frame_cycles: 1 }
    }
    fn is_rom(_rom: &[u8]) -> bool { false }
    fn reset(&mut self) {}
    fn unload_rom(&mut self) {}
    fn run_frame(&mut self) {}
    fn step(&mut self) {}
    fn set_keys(&mut self, _: u32) {}
    fn keys(&self) -> u32 { 0 }
    fn video_buffer(&self) -> &[u32] { &[] }
    fn base_video_size(&self) -> (u32, u32) { (1, 1) }
    fn audio_buffer(&mut self) -> &mut RingI16 { panic!() }
    fn audio_sample_rate(&self) -> i32 { 1 }
    fn set_audio_buffer_size(&mut self, _: usize) {}
    fn frame_counter(&self) -> u32 { 0 }
    fn frame_cycles(&self) -> i32 { 1 }
    fn frequency(&self) -> i32 { 1 }
    fn state_size(&mut self) -> usize { 64 }
    fn save_state(&mut self, out: &mut Vec<u8>) -> Result<(), &'static str> {
        out.extend_from_slice(&self.state);
        Ok(())
    }
    fn load_state(&mut self, state: &[u8]) -> Result<(), &'static str> {
        if state.len() != 64 { return Err("size"); }
        self.state.copy_from_slice(state);
        Ok(())
    }
    fn savedata(&self) -> Option<&[u8]> { None }
    fn savedata_mut(&mut self) -> Option<&mut [u8]> { None }
}

#[test]
fn rewind_restores_previous_states() {
    let mut core = MockCore { state: [0; 64] };
    let mut ctx = RewindContext::new(8);

    // Frame 0..=5, each sets a byte in the state.
    for f in 0..5u8 {
        core.state[0] = f;
        core.state[1] = 0xAA ^ f;
        ctx.append(&mut core);
    }
    assert_eq!(core.state[0], 4);

    // Rewind 3 steps: we should land on frame state 1 (4 - 3).
    assert!(ctx.restore(&mut core, 3));
    assert_eq!(core.state[0], 1);
    assert_eq!(core.state[1], 0xAA ^ 1);

    // Rewind once more: frame 0.
    assert!(ctx.restore(&mut core, 1));
    assert_eq!(core.state[0], 0);

    // History now exhausted: C returns false and leaves the console alone.
    assert!(!ctx.restore(&mut core, 12));
    assert_eq!(core.state[0], 0);
}

#[test]
fn rewind_rolling_window_discards_oldest() {
    let mut core = MockCore { state: [0; 64] };
    let mut ctx = RewindContext::new(4);
    for f in 0..8u8 {
        core.state[0] = f;
        ctx.append(&mut core);
    }
    // Only the last ~4 states retained; rewind to the floor, then one step
    // short of the oldest recorded.
    assert!(ctx.restore(&mut core, 3));
    assert_eq!(core.state[0], 4);
}
