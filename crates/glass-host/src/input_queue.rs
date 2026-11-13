//! Pure, completion-gated input scheduling. Not wired to a native executor yet.
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Owner {
    id: u64,
    epoch: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Physical {
    Key(u16),
    Button(u8),
}
/// Ingress assigns strictly increasing generations across all presses of one owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Press {
    pub physical: Physical,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug)]
pub enum OwnerClass {
    Viewer,
    Local,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Paste,
    Reset,
    MoveAbsolute {
        x: i32,
        y: i32,
    },
    Configure {
        displays: Box<[u32]>,
        window: Option<[f64; 4]>,
    },
    Display(u32),
    Displays(Box<[u32]>),
    Window(Option<[f64; 4]>),
    Move {
        display: u32,
        x: f32,
        y: f32,
    },
    Event(glass_proto::InputEvent),
}
#[derive(Debug)]
pub enum Command {
    Down(Press),
    Repeat(Press),
    Up(Press),
    Text(Box<str>),
    Control(Control),
}
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    Down(Physical),
    Repeat(Physical),
    Up(Physical),
    Scalar(char),
    Control(Control),
    Ownership,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt(pub(crate) u64);
#[derive(Debug)]
pub struct Step {
    pub token: Token,
    pub receipt: Receipt,
    pub operation: Operation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Unsupported,
    Partial,
    Failed,
    Unknown,
    Cancelled,
    Retired,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub receipt: Receipt,
    pub outcome: Outcome,
    pub completed_scalars: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    StaleOwner,
    OwnerBusy,
    OwnerLimit,
    Overload,
    Invalid,
    Exhausted,
}
#[derive(Debug)]
pub struct Rejected {
    pub error: Error,
    pub cancelled: Vec<Finished>,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub viewer_text: usize,
    pub local_text: usize,
    pub global_text: usize,
    pub owner_commands: usize,
    pub global_commands: usize,
    pub owners: usize,
    pub generations: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            viewer_text: 16 * 1024,
            local_text: 64 * 1024,
            global_text: 256 * 1024,
            owner_commands: 256,
            global_commands: 1024,
            owners: 64,
            generations: 256,
        }
    }
}
#[derive(Clone)]
struct Hold {
    active: bool,
    releasing: bool,
}
struct OwnerState {
    class: OwnerClass,
    cancelled: bool,
    last_generation: u64,
    presses: BTreeMap<Press, Hold>,
}
struct Entry {
    owner: Owner,
    receipt: Receipt,
    command: Command,
    cursor: usize,
    scalars: usize,
    reserved: bool,
    // Local atomic batches preserve their own chord order, including releases.
    batch: Option<Receipt>,
}
impl Entry {
    fn bytes(&self) -> usize {
        match &self.command {
            Command::Text(t) => t.len(),
            _ => 0,
        }
    }
    fn finished(&self, outcome: Outcome) -> Finished {
        Finished {
            receipt: self.receipt,
            outcome,
            completed_scalars: self.scalars,
        }
    }
}
struct Flight {
    token: Token,
    entry: Entry,
    cleanup: bool,
}

/// All methods are synchronous bounded state transitions; the caller owns synchronization.
/// Execute the returned operation OUTSIDE the lock, then acknowledge its exact token once.
/// Ownership is a no-native-work step, but still requires Completed acknowledgement.
/// Scalar is one Unicode scalar (including newline): the adapter must implement transient
/// Enter/modifier reconciliation and native partial-prefix cleanup within that single step.
/// Down/Up describe aggregate physical state; adapters must preserve it across scalar work.
/// Control cannot conceal a text backlog. Event rejects key/button/text bypasses. A future
/// display/route-changing adapter must invalidate affected owners before resetting native
/// state; blindly calling the current release-all display setter violates this contract.
///
/// Resource bounds include the in-flight entry's entire text allocation. Selection scans
/// at most global_commands + owners * generations entries; cancellation and admission
/// likewise inspect only bounded retained state. Validation clones at most one owner's
/// generation table and visits a bounded batch (ordinary command limit + generation limit).
/// No wire data is parsed here. Ingress MUST bound frame sizes, text assembly, batch
/// construction, owner-open attempts and events/bytes per time interval BEFORE this core.
/// Queue bounds cannot limit CPU from an unlimited stream of foreign ups or open/cancel.
/// Returned receipts/results belong to the worker: deliver/drop promptly instead of
/// accumulating an unbounded completion mailbox. No completion backlog lives here.
/// Unknown/Partial/Failed cancel the affected owner; route-wide invalidation and cleanup
/// of native scalar internals remain the executor/worker's responsibility. No replay.
pub struct Scheduler {
    limits: Limits,
    owners: BTreeMap<Owner, OwnerState>,
    queue: VecDeque<Entry>,
    flight: Option<Flight>,
    epoch: u64,
    receipt: u64,
    token: u64,
}
impl Scheduler {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            owners: BTreeMap::new(),
            queue: VecDeque::new(),
            flight: None,
            epoch: 0,
            receipt: 0,
            token: 0,
        }
    }
    pub fn open_owner(&mut self, id: u64, class: OwnerClass) -> Result<Owner, Error> {
        if self.owners.keys().any(|o| o.id == id) {
            return Err(Error::OwnerBusy);
        }
        if self.owners.len() >= self.limits.owners {
            return Err(Error::OwnerLimit);
        }
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Exhausted)?;
        let owner = Owner {
            id,
            epoch: self.epoch,
        };
        self.owners.insert(
            owner,
            OwnerState {
                class,
                cancelled: false,
                last_generation: 0,
                presses: BTreeMap::new(),
            },
        );
        Ok(owner)
    }
    pub fn try_submit(&mut self, owner: Owner, command: Command) -> Result<Receipt, Rejected> {
        self.try_submit_batch(owner, vec![command]).map(|r| r[0])
    }
    pub fn try_submit_batch(
        &mut self,
        owner: Owner,
        commands: Vec<Command>,
    ) -> Result<Vec<Receipt>, Rejected> {
        if let Err(error) = self.validate(owner, &commands) {
            let cancelled = if matches!(error, Error::Overload | Error::Invalid | Error::Exhausted)
            {
                self.cancel_owner(owner)
            } else {
                Vec::new()
            };
            return Err(Rejected { error, cancelled });
        }
        let batch = matches!(self.owners[&owner].class, OwnerClass::Local)
            .then_some(Receipt(self.receipt + 1));
        let mut receipts = Vec::with_capacity(commands.len());
        for command in commands {
            let state = self.owners.get_mut(&owner).unwrap();
            let reserved = match command {
                Command::Down(p) => {
                    state.last_generation = p.generation;
                    state.presses.insert(
                        p,
                        Hold {
                            active: false,
                            releasing: false,
                        },
                    );
                    false
                }
                Command::Up(p) => {
                    if let Some(h) = state.presses.get_mut(&p) {
                        if !h.releasing {
                            h.releasing = true;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                _ => false,
            };
            self.receipt += 1;
            let receipt = Receipt(self.receipt);
            receipts.push(receipt);
            self.queue.push_back(Entry {
                owner,
                receipt,
                command,
                cursor: 0,
                scalars: 0,
                reserved,
                batch,
            });
        }
        Ok(receipts)
    }
    /// Cancel without executing a backlog. Native cleanup remains the worker's job.
    pub fn is_live(&self, owner: Owner) -> bool {
        self.owners.get(&owner).is_some_and(|s| !s.cancelled)
    }
    pub fn cancel_all(&mut self) -> Vec<Finished> {
        let owners: Vec<_> = self.owners.keys().copied().collect();
        owners
            .into_iter()
            .flat_map(|o| self.cancel_owner(o))
            .collect()
    }
    /// After best-effort global native cleanup, retire uncertain accounting once.
    /// Called only with no outstanding operation; never retry failed cleanup forever.
    pub fn forget_cancelled_holds(&mut self) {
        assert!(self.flight.is_none());
        for state in self.owners.values_mut().filter(|s| s.cancelled) {
            state.presses.clear();
        }
        self.reap();
    }
    fn validate(&self, owner: Owner, commands: &[Command]) -> Result<(), Error> {
        let state = self
            .owners
            .get(&owner)
            .filter(|s| !s.cancelled)
            .ok_or(Error::StaleOwner)?;
        if commands.len()
            > self
                .limits
                .owner_commands
                .saturating_add(self.limits.generations)
        {
            return Err(Error::Overload);
        }
        self.receipt
            .checked_add(commands.len() as u64)
            .ok_or(Error::Exhausted)?;
        let (mut local_bytes, mut global_bytes, mut local_count, mut global_count) =
            (0usize, 0usize, 0usize, 0usize);
        for e in self
            .queue
            .iter()
            .chain(self.flight.iter().filter(|f| !f.cleanup).map(|f| &f.entry))
        {
            global_bytes = global_bytes.saturating_add(e.bytes());
            global_count += usize::from(!e.reserved);
            if e.owner == owner {
                local_bytes = local_bytes.saturating_add(e.bytes());
                local_count += usize::from(!e.reserved);
            }
        }
        let mut presses = state.presses.clone();
        let mut last = state.last_generation;
        for c in commands {
            let reserved = match c {
                Command::Down(p) => {
                    if p.generation <= last
                        || presses
                            .iter()
                            .any(|(old, h)| old.physical == p.physical && !h.releasing)
                    {
                        return Err(Error::Invalid);
                    }
                    if presses.len() >= self.limits.generations {
                        return Err(Error::Overload);
                    }
                    last = p.generation;
                    presses.insert(
                        *p,
                        Hold {
                            active: false,
                            releasing: false,
                        },
                    );
                    false
                }
                Command::Up(p) => match presses.get_mut(p) {
                    Some(h) if !h.releasing => {
                        h.releasing = true;
                        true
                    }
                    _ => false,
                },
                Command::Repeat(p) => {
                    if !presses.contains_key(p) {
                        return Err(Error::Invalid);
                    }
                    false
                }
                Command::Text(t) => {
                    local_bytes = local_bytes.saturating_add(t.len());
                    global_bytes = global_bytes.saturating_add(t.len());
                    false
                }
                Command::Control(
                    Control::Displays(ids) | Control::Configure { displays: ids, .. },
                ) if ids.len() > glass_proto::MAX_STREAMS => return Err(Error::Invalid),
                Command::Control(Control::Event(e)) => {
                    use glass_proto::InputEvent::*;
                    if matches!(
                        e,
                        KeyDown { .. }
                            | KeyUp { .. }
                            | MouseDown { .. }
                            | MouseUp { .. }
                            | Text { .. }
                    ) {
                        return Err(Error::Invalid);
                    }
                    false
                }
                _ => false,
            };
            local_count = local_count.saturating_add(usize::from(!reserved));
            global_count = global_count.saturating_add(usize::from(!reserved));
        }
        let text_limit = match state.class {
            OwnerClass::Viewer => self.limits.viewer_text,
            OwnerClass::Local => self.limits.local_text,
        };
        if local_bytes > text_limit
            || global_bytes > self.limits.global_text
            || local_count > self.limits.owner_commands
            || global_count > self.limits.global_commands
        {
            return Err(Error::Overload);
        }
        Ok(())
    }
    fn active(&self, owner: Owner, p: Press) -> bool {
        self.owners
            .get(&owner)
            .and_then(|s| s.presses.get(&p))
            .is_some_and(|h| h.active)
    }
    fn other_active(&self, owner: Owner, physical: Physical) -> bool {
        self.owners.iter().any(|(o, s)| {
            *o != owner
                && s.presses
                    .iter()
                    .any(|(p, h)| p.physical == physical && h.active)
        })
    }
    fn reap(&mut self) {
        let flight_owner = self.flight.as_ref().map(|f| f.entry.owner);
        self.owners
            .retain(|o, s| !s.cancelled || !s.presses.is_empty() || Some(*o) == flight_owner);
    }
    pub fn cancel_owner(&mut self, owner: Owner) -> Vec<Finished> {
        let Some(state) = self.owners.get_mut(&owner) else {
            return Vec::new();
        };
        state.cancelled = true;
        let mut finished = Vec::new();
        self.queue.retain(|e| {
            if e.owner == owner {
                finished.push(e.finished(Outcome::Cancelled));
                false
            } else {
                true
            }
        });
        let pending_down = self
            .flight
            .as_ref()
            .filter(|f| f.entry.owner == owner)
            .and_then(|f| match f.entry.command {
                Command::Down(p) => Some(p),
                _ => None,
            });
        let remove: Vec<_> = self.owners[&owner]
            .presses
            .iter()
            .filter_map(|(p, h)| {
                if Some(*p) == pending_down {
                    None
                } else if !h.active || self.other_active(owner, p.physical) {
                    Some(*p)
                } else {
                    None
                }
            })
            .collect();
        for p in remove {
            self.owners.get_mut(&owner).unwrap().presses.remove(&p);
        }
        self.reap();
        finished
    }
    pub fn next_step(&mut self) -> Option<Step> {
        if self.flight.is_some() {
            return None;
        }
        // Tokens never wrap: exhaustion freezes selection rather than accepting an old ACK.
        let next_token = self.token.checked_add(1)?;
        let cleanup = self.owners.iter().find_map(|(o, s)| {
            if s.cancelled {
                s.presses
                    .iter()
                    .find(|(_, h)| h.active)
                    .map(|(p, _)| (*o, *p))
            } else {
                None
            }
        });
        let (entry, cleanup) = if let Some((owner, p)) = cleanup {
            (
                Entry {
                    owner,
                    receipt: Receipt(0),
                    command: Command::Up(p),
                    cursor: 0,
                    scalars: 0,
                    reserved: true,
                    batch: None,
                },
                true,
            )
        } else {
            let mut seen_batches = std::collections::BTreeSet::new();
            let urgent = self.queue.iter().enumerate().find_map(|(index, e)| {
                let earlier_in_batch = e.batch.is_some_and(|batch| !seen_batches.insert(batch.0));
                let Command::Up(p) = e.command else { return None; };
                (e.reserved && self.active(e.owner, p) && !earlier_in_batch).then_some(index)
            });
            (self.queue.remove(urgent.unwrap_or(0))?, false)
        };
        let operation = match &entry.command {
            Command::Down(p) => {
                if self.other_active(entry.owner, p.physical) {
                    Operation::Ownership
                } else {
                    Operation::Down(p.physical)
                }
            }
            Command::Repeat(p) => {
                if self.active(entry.owner, *p) {
                    Operation::Repeat(p.physical)
                } else {
                    Operation::Ownership
                }
            }
            Command::Up(p) => {
                if self.active(entry.owner, *p) && !self.other_active(entry.owner, p.physical) {
                    Operation::Up(p.physical)
                } else {
                    Operation::Ownership
                }
            }
            Command::Text(t) => t[entry.cursor..]
                .chars()
                .next()
                .map_or(Operation::Ownership, Operation::Scalar),
            Command::Control(c) => Operation::Control(c.clone()),
        };
        self.token = next_token;
        let token = Token(next_token);
        let receipt = entry.receipt;
        self.flight = Some(Flight {
            token,
            entry,
            cleanup,
        });
        Some(Step {
            token,
            receipt,
            operation,
        })
    }
    /// None means stale/invalid acknowledgement; the actual outstanding step is untouched.
    /// Partial/Unknown do not count the uncertain scalar as completed. Adapters report
    /// only Completed/Partial/Failed/Unknown; Cancelled/Retired are scheduler results.
    pub fn complete(&mut self, token: Token, outcome: Outcome) -> Option<Vec<Finished>> {
        if !matches!(
            outcome,
            Outcome::Completed
                | Outcome::Unsupported
                | Outcome::Partial
                | Outcome::Failed
                | Outcome::Unknown
        ) || self.flight.as_ref().is_none_or(|f| f.token != token)
        {
            return None;
        }
        let Flight {
            mut entry, cleanup, ..
        } = self.flight.take().unwrap();
        let owner = entry.owner;
        let cancelled = self.owners.get(&owner).is_none_or(|s| s.cancelled);
        let mut finished = Vec::new();
        match &entry.command {
            Command::Down(p) => {
                // An uncertain down may have reached the OS: reserve release-only cleanup.
                if let Some(h) = self
                    .owners
                    .get_mut(&owner)
                    .and_then(|s| s.presses.get_mut(p))
                {
                    h.active = true;
                }
            }
            Command::Up(p) => {
                if let Some(s) = self.owners.get_mut(&owner) {
                    s.presses.remove(p);
                }
                self.queue.retain(|e| {
                    if e.owner == owner && matches!(e.command,Command::Repeat(r) if r==*p) {
                        finished.push(e.finished(Outcome::Retired));
                        false
                    } else {
                        true
                    }
                });
            }
            Command::Text(t) if outcome == Outcome::Completed => {
                if let Some(ch) = t[entry.cursor..].chars().next() {
                    entry.cursor += ch.len_utf8();
                    entry.scalars += 1;
                }
            }
            _ => {}
        }
        // Unsupported text means no native attempt. End this text receipt, but keep
        // the input owner usable (notably Linux IME commits outside the US fallback).
        let unsupported_text = outcome == Outcome::Unsupported
            && matches!(entry.command, Command::Text(_));
        if outcome != Outcome::Completed && !unsupported_text {
            finished.extend(self.cancel_owner(owner));
        }
        if outcome == Outcome::Completed
            && !cancelled
            && matches!(&entry.command,Command::Text(t) if entry.cursor<t.len())
        {
            self.queue.push_front(entry);
        } else if !cleanup {
            finished.push(entry.finished(if outcome != Outcome::Completed {
                outcome
            } else if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Completed
            }));
        }
        // Late successful down must be detached from shared ownership or released.
        if cancelled {
            finished.extend(self.cancel_owner(owner));
        }
        self.reap();
        Some(finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SPACE: Physical = Physical::Key(49);
    const SHIFT: Physical = Physical::Key(56);
    const A: Physical = Physical::Key(0);
    fn p(physical: Physical, generation: u64) -> Press {
        Press {
            physical,
            generation,
        }
    }
    fn text(s: &str) -> Command {
        Command::Text(s.into())
    }
    fn owner(q: &mut Scheduler, id: u64) -> Owner {
        q.open_owner(id, OwnerClass::Viewer).unwrap()
    }
    fn submit(q: &mut Scheduler, o: Owner, c: Command) -> Receipt {
        q.try_submit(o, c).unwrap()
    }
    // Controllable executor: selection and acknowledgement are intentionally separate.
    fn take(q: &mut Scheduler, expected: Operation) -> Token {
        let step = q.next_step().expect("expected execution step");
        assert_eq!(step.operation, expected);
        assert!(q.next_step().is_none(), "only one outstanding step");
        step.token
    }
    fn run(q: &mut Scheduler, expected: Operation) -> Vec<Finished> {
        let token = take(q, expected);
        q.complete(token, Outcome::Completed).unwrap()
    }
    fn setup() -> (Scheduler, Owner) {
        let mut q = Scheduler::new(Limits::default());
        let o = owner(&mut q, 1);
        (q, o)
    }
    #[test]
    fn active_up_waits_for_scalar_then_overtakes_text() {
        let (mut q, o) = setup();
        let g = p(SPACE, 1);
        submit(&mut q, o, Command::Down(g));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, o, text("abc"));
        let t = take(&mut q, Operation::Scalar('a'));
        submit(&mut q, o, Command::Up(g));
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(SPACE));
        run(&mut q, Operation::Scalar('b'));
        run(&mut q, Operation::Scalar('c'));
        assert!(q.next_step().is_none());
    }
    #[test]
    fn pending_down_up_cannot_overtake_text_or_inflight_down() {
        let (mut q, o) = setup();
        let g = p(SPACE, 1);
        submit(&mut q, o, text("xy"));
        submit(&mut q, o, Command::Down(g));
        submit(&mut q, o, Command::Up(g));
        run(&mut q, Operation::Scalar('x'));
        run(&mut q, Operation::Scalar('y'));
        let t = take(&mut q, Operation::Down(SPACE));
        assert!(q.next_step().is_none());
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(SPACE));
    }
    #[test]
    fn promoted_up_retires_only_old_generation_repeats() {
        let (mut q, o) = setup();
        let g = p(SPACE, 1);
        let h = p(SPACE, 2);
        submit(&mut q, o, Command::Down(g));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, o, text("x"));
        let repeat = submit(&mut q, o, Command::Repeat(g));
        submit(&mut q, o, Command::Up(g));
        submit(&mut q, o, Command::Down(h));
        let done = run(&mut q, Operation::Up(SPACE));
        assert!(
            done.iter()
                .any(|f| f.receipt == repeat && f.outcome == Outcome::Retired)
        );
        run(&mut q, Operation::Scalar('x'));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, o, Command::Repeat(h));
        run(&mut q, Operation::Repeat(SPACE));
    }
    #[test]
    fn shared_keys_buttons_and_foreign_up_preserve_final_owner() {
        let (mut q, a) = setup();
        let b = owner(&mut q, 2);
        let c = owner(&mut q, 3);
        for (i, key) in [SPACE, Physical::Button(1)].into_iter().enumerate() {
            let g = p(key, i as u64 + 1);
            submit(&mut q, a, Command::Down(g));
            run(&mut q, Operation::Down(key));
            submit(&mut q, b, Command::Down(g));
            run(&mut q, Operation::Ownership);
            submit(&mut q, c, Command::Up(g));
            run(&mut q, Operation::Ownership);
        }
        q.cancel_owner(a);
        assert!(q.next_step().is_none());
        q.cancel_owner(b);
        run(&mut q, Operation::Up(SPACE));
        run(&mut q, Operation::Up(Physical::Button(1)));
        assert!(q.next_step().is_none());
    }
    #[test]
    fn cancelled_inflight_down_gets_cleanup_without_epoch_resurrection() {
        let (mut q, o) = setup();
        submit(&mut q, o, Command::Down(p(SPACE, 1)));
        let t = take(&mut q, Operation::Down(SPACE));
        q.cancel_owner(o);
        assert_eq!(q.open_owner(1, OwnerClass::Viewer), Err(Error::OwnerBusy));
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(SPACE));
        let new = owner(&mut q, 1);
        assert_ne!(new, o);
        assert_eq!(
            q.try_submit(o, text("old")).unwrap_err().error,
            Error::StaleOwner
        );
        submit(&mut q, new, text("n"));
        let n = take(&mut q, Operation::Scalar('n'));
        assert!(q.complete(t, Outcome::Completed).is_none());
        assert!(q.next_step().is_none());
        q.complete(n, Outcome::Completed).unwrap();
        assert!(q.next_step().is_none());
    }
    #[test]
    fn shift_release_changes_state_seen_by_queued_chord() {
        let (mut q, o) = setup();
        let shift = p(SHIFT, 1);
        submit(&mut q, o, Command::Down(shift));
        run(&mut q, Operation::Down(SHIFT));
        submit(&mut q, o, text("xy"));
        let t = take(&mut q, Operation::Scalar('x'));
        submit(&mut q, o, Command::Down(p(A, 2)));
        submit(&mut q, o, Command::Up(shift));
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(SHIFT));
        run(&mut q, Operation::Scalar('y'));
        run(&mut q, Operation::Down(A));
    }
    #[test]
    fn cancellation_stops_owner_text_but_other_owner_continues() {
        let (mut q, o) = setup();
        let b = owner(&mut q, 2);
        let r = submit(&mut q, o, text("abc"));
        let t = take(&mut q, Operation::Scalar('a'));
        submit(&mut q, b, text("z"));
        q.cancel_owner(o);
        let finished = q.complete(t, Outcome::Completed).unwrap();
        assert_eq!(
            finished,
            vec![Finished {
                receipt: r,
                outcome: Outcome::Cancelled,
                completed_scalars: 1
            }]
        );
        run(&mut q, Operation::Scalar('z'));
        assert!(q.next_step().is_none());
    }
    #[test]
    fn uncertain_text_never_replays_or_executes_suffix() {
        for outcome in [Outcome::Partial, Outcome::Failed, Outcome::Unknown] {
            let (mut q, o) = setup();
            let r = submit(&mut q, o, text("a😀z"));
            run(&mut q, Operation::Scalar('a'));
            let t = take(&mut q, Operation::Scalar('😀'));
            submit(&mut q, o, text("suffix"));
            let f = q.complete(t, outcome).unwrap();
            assert!(
                f.iter()
                    .any(|f| f.receipt == r && f.outcome == outcome && f.completed_scalars == 1)
            );
            assert!(q.next_step().is_none());
        }
    }
    #[test]
    fn unicode_scalar_cursor_and_ordinary_control_fifo() {
        let (mut q, o) = setup();
        submit(&mut q, o, text("č😀\n"));
        submit(&mut q, o, Command::Control(Control::Paste));
        submit(&mut q, o, text("z"));
        for ch in ['č', '😀', '\n'] {
            run(&mut q, Operation::Scalar(ch));
        }
        run(&mut q, Operation::Control(Control::Paste));
        run(&mut q, Operation::Scalar('z'));
    }
    #[test]
    fn inflight_allocations_count_and_overload_atomically_cancels_batch() {
        let mut q = Scheduler::new(Limits {
            viewer_text: 4,
            global_text: 4,
            ..Limits::default()
        });
        let o = owner(&mut q, 1);
        submit(&mut q, o, text("abcd"));
        let t = take(&mut q, Operation::Scalar('a'));
        let e = q
            .try_submit_batch(o, vec![text(""), text("x")])
            .unwrap_err();
        assert_eq!(e.error, Error::Overload);
        q.complete(t, Outcome::Completed).unwrap();
        assert!(q.next_step().is_none());
        let n = owner(&mut q, 1);
        submit(&mut q, n, text("z"));
        run(&mut q, Operation::Scalar('z'));
    }
    #[test]
    fn release_and_cancel_have_reserved_capacity() {
        let mut q = Scheduler::new(Limits {
            owner_commands: 1,
            global_commands: 1,
            ..Limits::default()
        });
        let o = owner(&mut q, 1);
        let g = p(SPACE, 1);
        submit(&mut q, o, Command::Down(g));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, o, text("abc"));
        submit(&mut q, o, Command::Up(g));
        run(&mut q, Operation::Up(SPACE));
        assert_eq!(
            q.try_submit(o, text("extra")).unwrap_err().error,
            Error::Overload
        );
        assert!(q.next_step().is_none());
        let n = owner(&mut q, 1);
        submit(&mut q, n, Command::Down(p(SPACE, 1)));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, n, text("abc"));
        q.cancel_owner(n);
        run(&mut q, Operation::Up(SPACE));
    }
    #[test]
    fn owner_and_generation_limits_reclaim_without_underflow() {
        let mut q = Scheduler::new(Limits {
            owners: 2,
            generations: 1,
            ..Limits::default()
        });
        let a = owner(&mut q, 1);
        let b = owner(&mut q, 2);
        assert_eq!(q.open_owner(3, OwnerClass::Viewer), Err(Error::OwnerLimit));
        submit(&mut q, a, Command::Down(p(SPACE, 1)));
        run(&mut q, Operation::Down(SPACE));
        assert_eq!(
            q.try_submit(a, Command::Down(p(A, 2))).unwrap_err().error,
            Error::Overload
        );
        run(&mut q, Operation::Up(SPACE));
        q.cancel_owner(a);
        q.cancel_owner(b);
        for _ in 0..100 {
            let o = owner(&mut q, 3);
            q.cancel_owner(o);
            q.cancel_owner(o);
        }
    }
    #[test]
    fn up_admitted_during_down_flight_activates_only_after_matching_completion() {
        let (mut q, o) = setup();
        let g = p(SPACE, 1);
        submit(&mut q, o, Command::Down(g));
        let down = take(&mut q, Operation::Down(SPACE));
        submit(&mut q, o, text("ab"));
        submit(&mut q, o, Command::Up(g));
        assert!(q.next_step().is_none());
        assert!(q.complete(Token(down.0 + 1), Outcome::Completed).is_none());
        assert!(q.next_step().is_none());
        q.complete(down, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(SPACE));
        run(&mut q, Operation::Scalar('a'));
        run(&mut q, Operation::Scalar('b'));
    }
    #[test]
    fn shared_release_retires_own_repeat_but_preserves_other_owner_repeat() {
        let (mut q, a) = setup();
        let b = owner(&mut q, 2);
        let g = p(SPACE, 1);
        submit(&mut q, a, Command::Down(g));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, b, Command::Down(g));
        run(&mut q, Operation::Ownership);
        submit(&mut q, a, text("x"));
        let own = submit(&mut q, a, Command::Repeat(g));
        let other = submit(&mut q, b, Command::Repeat(g));
        submit(&mut q, a, Command::Up(g));
        let done = run(&mut q, Operation::Ownership);
        assert!(
            done.iter()
                .any(|f| f.receipt == own && f.outcome == Outcome::Retired)
        );
        assert!(!done.iter().any(|f| f.receipt == other));
        run(&mut q, Operation::Scalar('x'));
        assert_eq!(run(&mut q, Operation::Repeat(SPACE))[0].receipt, other);
        q.cancel_owner(a);
        assert!(q.next_step().is_none());
        q.cancel_owner(b);
        run(&mut q, Operation::Up(SPACE));
    }
    #[test]
    fn shared_down_transition_survives_original_owner_disconnect_during_ack() {
        let (mut q, a) = setup();
        let b = owner(&mut q, 2);
        let g = p(SPACE, 1);
        submit(&mut q, a, Command::Down(g));
        run(&mut q, Operation::Down(SPACE));
        submit(&mut q, b, Command::Down(g));
        let t = take(&mut q, Operation::Ownership);
        q.cancel_owner(a);
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Ownership);
        submit(&mut q, b, Command::Repeat(g));
        run(&mut q, Operation::Repeat(SPACE));
        q.cancel_owner(b);
        run(&mut q, Operation::Up(SPACE));
    }
    #[test]
    fn local_text_budget_and_global_command_limit_are_independent() {
        let mut q = Scheduler::new(Limits {
            viewer_text: 2,
            local_text: 8,
            global_text: 8,
            global_commands: 2,
            ..Limits::default()
        });
        let local = q.open_owner(1, OwnerClass::Local).unwrap();
        let viewer = owner(&mut q, 2);
        submit(&mut q, local, text("😀xy"));
        submit(&mut q, viewer, text("ab"));
        assert_eq!(
            q.try_submit(viewer, Command::Control(Control::Paste))
                .unwrap_err()
                .error,
            Error::Overload
        );
        for c in ['😀', 'x', 'y'] {
            run(&mut q, Operation::Scalar(c));
        }
        assert!(q.next_step().is_none());
        let viewer = owner(&mut q, 2);
        assert_eq!(
            q.try_submit(viewer, text("abc")).unwrap_err().error,
            Error::Overload
        );
    }
    #[test]
    fn many_owners_and_continuous_admission_cannot_starve_active_release() {
        let mut q = Scheduler::new(Limits::default());
        let mut owners = Vec::new();
        for id in 0..64 {
            owners.push(owner(&mut q, id));
        }
        let g = p(Physical::Button(2), 1);
        submit(&mut q, owners[0], Command::Down(g));
        run(&mut q, Operation::Down(g.physical));
        submit(&mut q, owners[1], text("xyz"));
        let t = take(&mut q, Operation::Scalar('x'));
        for &o in &owners[2..] {
            submit(&mut q, o, Command::Control(Control::Paste));
        }
        submit(&mut q, owners[0], Command::Up(g));
        q.complete(t, Outcome::Completed).unwrap();
        run(&mut q, Operation::Up(g.physical));
        run(&mut q, Operation::Scalar('y'));
        run(&mut q, Operation::Scalar('z'));
        for &o in &owners[2..] {
            run(&mut q, Operation::Control(Control::Paste));
            submit(&mut q, o, Command::Control(Control::Display(7)));
        }
        for _ in &owners[2..] {
            run(&mut q, Operation::Control(Control::Display(7)));
        }
        for o in owners {
            q.cancel_owner(o);
        }
        assert!(q.next_step().is_none());
    }
    #[test]
    fn batch_overload_has_no_admitted_prefix_and_native_event_bypasses_are_rejected() {
        let mut q = Scheduler::new(Limits {
            owner_commands: 1,
            ..Limits::default()
        });
        let o = owner(&mut q, 1);
        assert_eq!(
            q.try_submit_batch(o, vec![Command::Down(p(SPACE, 1)), text("x")])
                .unwrap_err()
                .error,
            Error::Overload
        );
        assert!(q.next_step().is_none());
        let o = owner(&mut q, 1);
        assert_eq!(
            q.try_submit(
                o,
                Command::Control(Control::Event(glass_proto::InputEvent::KeyDown {
                    keycode: 49
                }))
            )
            .unwrap_err()
            .error,
            Error::Invalid
        );
        assert!(q.next_step().is_none());
    }
}
