// Copyright (c) 2013-2016 Jeffrey Pfau (mGBA), MPL-2.0.
// Ported from mgba/src/core/timing.c and include/mgba/core/timing.h.
//
// mGBA's mTiming is a priority event scheduler over a 32-bit master-cycle
// counter. Events form an intrusive singly-linked list sorted by
// (when, priority); events with identical (when, priority) run in FIFO order.
// Callbacks can reschedule from inside mTimingTick; mTimingInterrupt() moves
// the pending list aside ("reroot") so a caller can drain the queue.
//
// In the C, an event is a heap object with a function pointer and a `when`.
// Here an event is identified by a small integer id (the console maps ids to
// its internal event slots) and this struct only tracks (id, when, priority).
// The console passes itself to tick() as `ctx` and dispatches ids manually.

/// A scheduled event slot. `id` < 0 style empty slots are not used; the Vec
/// always contains only live events.
#[derive(Clone, Copy, Debug)]
struct EventSlot {
    id: u32,
    when: u32,
    priority: u32,
}

#[derive(Default)]
pub struct Timing {
    root: Vec<EventSlot>,
    reroot: Vec<EventSlot>,
    pub global_cycles: u64,
    pub master_cycles: u32,
    /// Owned copy of the C's `int32_t* relativeCycles`.
    relative_cycles: i32,
    /// Owned copy of the C's `int32_t* nextEvent`.
    next_event: i32,
}

/// Result of popping the head of the event queue in `Timing::pop_due_event`.
pub enum PopDue {
    /// The head event was due; it has been removed. (id, cycles_late)
    Event(u32, i32),
    /// Head event not yet due: cycles until it fires (mTimingTick's early
    /// `return nextWhen`).
    Pending(i32),
    /// Queue empty.
    Empty,
}

fn insert_sorted(list: &mut Vec<EventSlot>, ev: EventSlot, master_cycles: u32) {
    // Match mTimingSchedule's insertion: before the first event that fires
    // strictly later, or that fires at the same time with higher priority.
    let next_event = ev.when.wrapping_sub(master_cycles) as i32;
    let pos = list
        .iter()
        .position(|next| {
            let next_when = next.when.wrapping_sub(master_cycles) as i32;
            next_when > next_event || (next_when == next_event && next.priority > ev.priority)
        })
        .unwrap_or(list.len());
    list.insert(pos, ev);
}

impl Timing {
    pub fn new() -> Self {
        Timing {
            root: Vec::new(),
            reroot: Vec::new(),
            global_cycles: 0,
            master_cycles: 0,
            relative_cycles: 0,
            next_event: i32::MAX,
        }
    }

    pub fn deinit(&mut self) {}

    pub fn clear(&mut self) {
        self.root.clear();
        self.reroot.clear();
        self.global_cycles = 0;
        self.master_cycles = 0;
    }

    pub fn interrupt(&mut self) {
        if self.root.is_empty() {
            return;
        }
        std::mem::swap(&mut self.root, &mut self.reroot);
    }

    /// mGBA exposes relativeCycles/nextEvent as pointers shared with the CPU;
    /// we keep owned copies here instead. The console updates relative_cycles
    /// as the CPU advances.
    pub fn set_relative_cycles(&mut self, cycles: i32) {
        self.relative_cycles = cycles;
    }

    pub fn relative_cycles(&self) -> i32 {
        self.relative_cycles
    }

    pub fn set_next_event(&mut self, next: i32) {
        self.next_event = next;
    }

    /// The internal nextEvent copy — mirrors the C's shared `*nextEvent`
    /// pointer after schedules done outside mTimingTick.
    pub fn next_event_cycles_owned(&self) -> i32 {
        self.next_event
    }

    /// Schedule `id` `when` cycles from `current_time()`.
    pub fn schedule(&mut self, id: u32, priority: u32, when: i32) {
        let next_event = when + self.relative_cycles;
        let ev = EventSlot {
            id,
            when: (next_event as u32).wrapping_add(self.master_cycles),
            priority,
        };
        if next_event < self.next_event {
            self.next_event = next_event;
        }
        if !self.reroot.is_empty() {
            let master = self.master_cycles;
            insert_sorted(&mut self.reroot, ev, master);
        } else {
            let master = self.master_cycles;
            insert_sorted(&mut self.root, ev, master);
        }
    }

    pub fn schedule_absolute(&mut self, id: u32, priority: u32, when: i32) {
        self.schedule(id, priority, when - self.current_time());
    }

    pub fn deschedule(&mut self, id: u32) {
        if !self.reroot.is_empty() {
            if let Some(pos) = self.reroot.iter().position(|e| e.id == id) {
                self.reroot.remove(pos);
                return;
            }
        } else if let Some(pos) = self.root.iter().position(|e| e.id == id) {
            self.root.remove(pos);
        }
    }

    pub fn is_scheduled(&self, id: u32) -> bool {
        let list = if !self.root.is_empty() { &self.root } else { &self.reroot };
        list.iter().any(|e| e.id == id)
    }

    /// The clock-advance half of mTimingTick (`*masterCycles += cycles`).
    /// Pair with `pop_due_event` when the console dispatches events itself
    /// (so event handlers run with the console's timing queue in place,
    /// exactly like the C where `&gba->timing` is shared everywhere).
    pub fn advance_clock(&mut self, cycles: i32) {
        self.master_cycles = self.master_cycles.wrapping_add(cycles as u32);
        self.global_cycles += cycles as u64;
    }

    /// The dispatch step of mTimingTick's loop: if the head event is due,
    /// remove it and return `PopDue::Event(id, cycles_late)`; if not due,
    /// `PopDue::Pending(cycles_until)`; if the queue is empty, `PopDue::Empty`.
    pub fn pop_due_event(&mut self) -> PopDue {
        let Some(next) = self.root.first().copied() else {
            return PopDue::Empty;
        };
        let next_when = next.when.wrapping_sub(self.master_cycles) as i32;
        if next_when > 0 {
            return PopDue::Pending(next_when);
        }
        self.root.remove(0);
        PopDue::Event(next.id, -next_when)
    }

    /// mTimingTick's tail: after the root queue empties, adopt the rerooted
    /// list (`mTimingInterrupt` moves the pending queue aside so a console
    /// can drain it after the frame). Returns true when an adoption happened
    /// (the C then does `*nextEvent = mTimingNextEvent(timing)`).
    pub fn adopt_reroot(&mut self) -> bool {
        if self.reroot.is_empty() {
            return false;
        }
        std::mem::swap(&mut self.root, &mut self.reroot);
        true
    }

    /// Run the clock forward `cycles` master cycles, dispatching due events to
    /// `callback(ctx, timing, id, cycles_late)`. Returns the number of cycles
    /// until the next pending event (matching mTimingTick's return value:
    /// `*nextEvent`, i.e. relative to current time).
    pub fn tick<C>(
        &mut self,
        cycles: i32,
        ctx: &mut C,
        callback: &mut dyn FnMut(&mut C, &mut Timing, u32, i32),
    ) -> i32 {
        self.master_cycles = self.master_cycles.wrapping_add(cycles as u32);
        self.global_cycles += cycles as u64;
        let master_cycles = self.master_cycles;
        while let Some(next) = self.root.first().copied() {
            let next_when = next.when.wrapping_sub(master_cycles) as i32;
            if next_when > 0 {
                return next_when;
            }
            self.root.remove(0);
            callback(ctx, self, next.id, -next_when);
        }
        if !self.reroot.is_empty() {
            std::mem::swap(&mut self.root, &mut self.reroot);
            self.next_event = self.next_event_cycles_raw();
        }
        self.next_event
    }

    pub fn current_time(&self) -> i32 {
        self.master_cycles as i32 + self.relative_cycles
    }

    pub fn global_time(&self) -> u64 {
        self.global_cycles + self.relative_cycles as u64
    }

    /// Cycles from `current_time()` until the next event at the head of the
    /// active list; i32::MAX if empty.
    pub fn next_event_cycles(&self) -> i32 {
        match self.root.first() {
            None => i32::MAX,
            Some(next) => {
                (next.when.wrapping_sub(self.master_cycles) as i32) - self.relative_cycles
            }
        }
    }

    /// Same, relative to master_cycles only (used by tick() after reroot
    /// adoption, matching mTimingNextEvent being called with
    /// *relativeCycles == 0 in that context... actually mTimingNextEvent
    /// subtracts *relativeCycles; tick adopts reroot while relativeCycles is
    /// whatever the console left. We reproduce the C exactly by sharing the
    /// same code path.)
    fn next_event_cycles_raw(&self) -> i32 {
        match self.root.first() {
            None => i32::MAX,
            Some(next) => {
                (next.when.wrapping_sub(self.master_cycles) as i32) - self.relative_cycles
            }
        }
    }

    /// Cycles from `current_time()` until the given event fires.
    pub fn until(&self, id: u32) -> i32 {
        for e in self.root.iter().chain(self.reroot.iter()) {
            if e.id == id {
                return (e.when.wrapping_sub(self.master_cycles) as i32) - self.relative_cycles;
            }
        }
        i32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_order() {
        let mut t = Timing::new();
        t.schedule(1, 0, 100);
        t.schedule(2, 0, 50);
        t.schedule(3, 0, 100); // same when+priority as 1: FIFO after 1
        t.schedule(4, 5, 100); // higher priority number: after 1,3
        t.schedule(5, 2, 60); // fires at 60, before 1/3
        let mut fired = Vec::new();
        let _ = t.tick(200, &mut fired, &mut |out: &mut Vec<u32>, _t, id, _late| out.push(id));
        assert_eq!(fired, vec![2, 5, 1, 3, 4]);
    }

    #[test]
    fn tick_returns_time_to_next() {
        // Mirrors GBProcessEvents: caller resets nextEvent to INT_MAX before
        // each batch of ticks.
        let mut t = Timing::new();
        t.schedule(1, 0, 100);
        t.set_next_event(i32::MAX);
        let r = t.tick(40, &mut (), &mut |_, _, _, _| panic!());
        assert_eq!(r, 60);
        t.set_next_event(i32::MAX);
        let r = t.tick(60, &mut (), &mut |_, _, _, _| {}); // fires 0 cycles late
        assert_eq!(r, i32::MAX);
    }

    #[test]
    fn reschedule_inside_callback() {
        // Event scheduled at master 100, but we tick 300: it fires 200 late.
        let mut t = Timing::new();
        t.schedule(1, 0, 100);
        let mut fired = 0;
        struct Ctx<'a> {
            fired: &'a mut i32,
        }
        let mut cb = |ctx: &mut Ctx, timing: &mut Timing, id: u32, late: i32| {
            *ctx.fired += 1;
            if id == 1 {
                assert_eq!(late, 200);
                timing.schedule(2, 0, 50); // lands at master 350 (> now: not re-fired this tick)
            } else {
                assert_eq!(id, 2);
                assert_eq!(late, 0);
            }
        };
        t.set_next_event(i32::MAX);
        let r = {
            let mut ctx = Ctx { fired: &mut fired };
            t.tick(300, &mut ctx, &mut cb)
        };
        assert_eq!(r, 50);
        t.set_next_event(i32::MAX);
        let r = {
            let mut ctx = Ctx { fired: &mut fired };
            t.tick(50, &mut ctx, &mut cb)
        };
        assert_eq!(r, i32::MAX);
        assert_eq!(fired, 2);
    }

    #[test]
    fn current_time_accounts_for_relative() {
        let mut t = Timing::new();
        t.set_relative_cycles(5);
        t.schedule(1, 0, 10);
        // Event fires at master 15.
        let r = t.tick(14, &mut (), &mut |_, _, _, _| panic!());
        assert_eq!(r, 1);
    }
}
