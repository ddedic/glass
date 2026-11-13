//! Serialized native execution with bounded admission and completion receipts.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        input_queue::{Operation, OwnerClass, Physical},
        media::InputSink,
        text_input::Completion,
    };
    use glass_proto::InputEvent;
    use std::sync::mpsc;

    struct Gated {
        steps: mpsc::Sender<Operation>,
        gate: mpsc::Receiver<Completion>,
    }
    impl InputSink for Gated {
        fn execute_step(&mut self, op: &Operation) -> Completion {
            self.steps.send(op.clone()).unwrap();
            self.gate.recv().unwrap()
        }
        fn handle(&mut self, _: InputEvent) {
            panic!("legacy input bypass")
        }
        fn paste(&mut self) {
            panic!("legacy paste bypass")
        }
        fn release_all(&mut self) {
            let _ = self
                .steps
                .send(Operation::Control(crate::input_queue::Control::Reset));
        }
        fn set_displays(&mut self, _: &[u32]) {
            panic!("legacy display bypass")
        }
        fn move_on_display(&mut self, _: u32, _: f32, _: f32) {
            panic!("legacy move bypass")
        }
        fn type_text(&mut self, _: &str) -> bool {
            panic!("legacy text bypass")
        }
    }
    impl crate::input_protocol::Transport for Gated {
        fn exchange(
            &mut self,
            bytes: &[u8; crate::input_protocol::REQUEST_LEN],
            _: Instant,
        ) -> Result<[u8; crate::input_protocol::ACK_LEN], ()> {
            let request = crate::input_protocol::Request::decode(bytes).ok_or(())?;
            let completion = self.execute_step(&request.operation);
            Ok(crate::input_protocol::Ack {
                epoch: request.epoch,
                sequence: request.sequence,
                completion,
            }
            .encode())
        }
        fn cancel(&mut self) {}
    }
    struct HelperSink {
        client: crate::input_protocol::Client<Gated>,
        steps: mpsc::Sender<Operation>,
    }
    impl InputSink for HelperSink {
        fn execute_step(&mut self, operation: &Operation) -> Completion {
            self.client.execute(operation.clone())
        }
        fn handle(&mut self, _: InputEvent) {
            panic!("legacy helper input")
        }
        fn paste(&mut self) {
            panic!("legacy helper paste")
        }
        fn release_all(&mut self) {
            let _ = self.steps.send(Operation::Control(Control::Reset));
        }
        fn set_displays(&mut self, _: &[u32]) {
            panic!("legacy helper display")
        }
        fn move_on_display(&mut self, _: u32, _: f32, _: f32) {
            panic!("legacy helper move")
        }
    }
    fn setup(
        helper: bool,
    ) -> (
        Dispatch,
        mpsc::Receiver<Operation>,
        mpsc::Sender<Completion>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (ack, gate) = mpsc::channel();
        let sink: Box<dyn InputSink> = if helper {
            Box::new(HelperSink {
                client: crate::input_protocol::Client::new(
                    Gated {
                        steps: tx.clone(),
                        gate,
                    },
                    1,
                ),
                steps: tx,
            })
        } else {
            Box::new(Gated { steps: tx, gate })
        };
        (Dispatch::new(sink), rx, ack)
    }
    fn next(rx: &mpsc::Receiver<Operation>) -> Operation {
        rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap()
    }
    #[tokio::test]
    async fn blocked_scalar_allows_up_admission_and_prioritizes_it() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Viewer).unwrap();
            o.events(vec![InputEvent::KeyDown { keycode: 49 }]).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Key(49)));
            ack.send(Completion::Completed).unwrap();
            let text = o.text("abc".into()).unwrap();
            assert_eq!(next(&rx), Operation::Scalar('a'));
            o.events(vec![InputEvent::KeyUp { keycode: 49 }]).unwrap();
            ack.send(Completion::Completed).unwrap();
            assert_eq!(next(&rx), Operation::Up(Physical::Key(49)));
            ack.send(Completion::Completed).unwrap();
            assert_eq!(next(&rx), Operation::Scalar('b'));
            drop(o);
            ack.send(Completion::Completed).unwrap();
            assert_eq!(text.wait().await.unwrap_err().completed_scalars, 2);
        }
    }
    #[tokio::test]
    async fn local_max_unicode_receipt_reports_failure_and_cancels_suffix() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Local).unwrap();
            let text = o.text("😀".repeat(10_000)).unwrap();
            assert_eq!(next(&rx), Operation::Scalar('😀'));
            ack.send(Completion::Partial).unwrap();
            assert_eq!(text.wait().await.unwrap_err().outcome, Outcome::Partial);
        }
    }
    #[tokio::test]
    async fn foreign_button_up_never_releases_other_owner() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let a = d.open(OwnerClass::Viewer).unwrap();
            let b = d.open(OwnerClass::Viewer).unwrap();
            a.events(vec![InputEvent::MouseDown { button: 0 }]).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Button(0)));
            ack.send(Completion::Completed).unwrap();
            b.events(vec![InputEvent::MouseUp { button: 0 }])
                .unwrap()
                .wait()
                .await
                .unwrap();
            drop(a);
            assert_eq!(next(&rx), Operation::Up(Physical::Button(0)));
            ack.send(Completion::Completed).unwrap();
        }
    }
    #[tokio::test]
    async fn disconnect_inflight_down_cleans_once_without_draining_text() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Viewer).unwrap();
            let down = o.events(vec![InputEvent::KeyDown { keycode: 49 }]).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Key(49)));
            let text = o.text("never".into()).unwrap();
            drop(o);
            ack.send(Completion::Completed).unwrap();
            assert_eq!(next(&rx), Operation::Up(Physical::Key(49)));
            ack.send(Completion::Completed).unwrap();
            assert_eq!(down.wait().await.unwrap_err().outcome, Outcome::Cancelled);
            assert_eq!(text.wait().await.unwrap_err().completed_scalars, 0);
            assert!(rx.try_recv().is_err());
        }
    }
    #[tokio::test]
    async fn full_queue_cancellation_does_not_need_queue_capacity() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Viewer).unwrap();
            let first = o.text("a".repeat(4096)).unwrap();
            assert_eq!(next(&rx), Operation::Scalar('a'));
            for _ in 0..3 {
                o.text("b".repeat(4096)).unwrap();
            }
            assert!(o.text("overflow".into()).is_err());
            drop(o);
            ack.send(Completion::Completed).unwrap();
            assert_eq!(first.wait().await.unwrap_err().outcome, Outcome::Cancelled);
            assert!(rx.try_recv().is_err());
        }
    }
    #[tokio::test]
    async fn unsuccessful_up_invalidates_before_accounting_is_lost() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Viewer).unwrap();
            o.events(vec![InputEvent::KeyDown { keycode: 49 }]).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Key(49)));
            ack.send(Completion::Completed).unwrap();
            let up = o.events(vec![InputEvent::KeyUp { keycode: 49 }]).unwrap();
            assert_eq!(next(&rx), Operation::Up(Physical::Key(49)));
            ack.send(Completion::Failed).unwrap();
            assert_eq!(
                next(&rx),
                Operation::Control(crate::input_queue::Control::Reset)
            );
            assert_eq!(up.wait().await.unwrap_err().outcome, Outcome::Failed);
            assert!(o.text("stale".into()).is_err());
        }
    }
    #[tokio::test]
    async fn local_batch_reports_execution_failure_and_cleans_both_presses() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Local).unwrap();
            let batch = o
                .events(vec![
                    InputEvent::KeyDown { keycode: 0 },
                    InputEvent::KeyDown { keycode: 1 },
                ])
                .unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Key(0)));
            ack.send(Completion::Completed).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Key(1)));
            ack.send(Completion::Failed).unwrap();
            assert_eq!(batch.wait().await.unwrap_err().outcome, Outcome::Failed);
            assert_eq!(next(&rx), Operation::Up(Physical::Key(0)));
            ack.send(Completion::Completed).unwrap();
            assert_eq!(next(&rx), Operation::Up(Physical::Key(1)));
            ack.send(Completion::Completed).unwrap();
        }
    }
    #[tokio::test]
    async fn local_chords_preserve_complete_batch_order() {
        for helper in [false, true] {
            for keys in [vec![59, 8], vec![55, 56, 21]] {
                let (d, rx, ack) = setup(helper);
                let o = d.open(OwnerClass::Local).unwrap();
                let events: Vec<_> = keys.iter().map(|&keycode| InputEvent::KeyDown { keycode })
                    .chain(keys.iter().rev().map(|&keycode| InputEvent::KeyUp { keycode })).collect();
                let batch = o.events(events).unwrap();
                for &key in &keys {
                    assert_eq!(next(&rx), Operation::Down(Physical::Key(key)));
                    ack.send(Completion::Completed).unwrap();
                }
                for &key in keys.iter().rev() {
                    assert_eq!(next(&rx), Operation::Up(Physical::Key(key)));
                    ack.send(Completion::Completed).unwrap();
                }
                batch.wait().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn unsupported_text_keeps_viewer_input_usable() {
        for helper in [false, true] {
            let (d, rx, ack) = setup(helper);
            let o = d.open(OwnerClass::Viewer).unwrap();
            let receipt = o.text("é".into()).unwrap();
            assert_eq!(next(&rx), Operation::Scalar('é'));
            ack.send(Completion::Unsupported).unwrap();
            assert_eq!(receipt.wait().await.unwrap_err().outcome, Outcome::Unsupported);
            let down = o.events(vec![InputEvent::MouseDown { button: 0 }]).unwrap();
            assert_eq!(next(&rx), Operation::Down(Physical::Button(0)));
            ack.send(Completion::Completed).unwrap();
            down.wait().await.unwrap();
            let up = o.events(vec![InputEvent::MouseUp { button: 0 }]).unwrap();
            assert_eq!(next(&rx), Operation::Up(Physical::Button(0)));
            ack.send(Completion::Completed).unwrap();
            up.wait().await.unwrap();
        }
    }

    #[test]
    fn rate_budget_uses_monotonic_supplied_time() {
        let at = Instant::now();
        let mut b = Bucket::new(2, 2, at);
        assert!(b.take(2, at));
        assert!(!b.take(1, at));
        assert!(b.take(1, at + std::time::Duration::from_millis(500)));
        assert!(!b.take(1, at + std::time::Duration::from_millis(500)));
    }
}

use crate::{
    input_queue::{
        Command, Control, Finished, Limits, Operation, Outcome, Owner, OwnerClass, Physical, Press,
        Scheduler,
    },
    media::InputSink,
    text_input::Completion,
};
use glass_proto::InputEvent;
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};
use tokio::sync::oneshot;

#[derive(Debug)]
pub struct Failure {
    pub outcome: Outcome,
    pub completed_scalars: usize,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input {:?} after {} completed scalars",
            self.outcome, self.completed_scalars
        )
    }
}
pub struct Receipt {
    results: Vec<oneshot::Receiver<Finished>>,
}
impl Receipt {
    pub async fn wait(self) -> Result<(), Failure> {
        let mut completed_scalars = 0;
        let mut failure = None;
        for rx in self.results {
            match rx.await {
                Ok(done) => {
                    completed_scalars += done.completed_scalars;
                    if !matches!(done.outcome, Outcome::Completed | Outcome::Retired)
                        && failure.is_none()
                    {
                        failure = Some(done.outcome);
                    }
                }
                Err(_) => {
                    failure.get_or_insert(Outcome::Cancelled);
                }
            }
        }
        failure.map_or(Ok(()), |outcome| {
            Err(Failure {
                outcome,
                completed_scalars,
            })
        })
    }
}
#[derive(Clone, Default)]
struct Ingress {
    generation: u64,
    presses: BTreeMap<Physical, Press>,
}
impl Ingress {
    fn event(&mut self, ev: InputEvent) -> Result<Command, String> {
        let (physical, down) = match ev {
            InputEvent::KeyDown { keycode } => (Physical::Key(keycode), true),
            InputEvent::KeyUp { keycode } => (Physical::Key(keycode), false),
            InputEvent::MouseDown { button } => (Physical::Button(button), true),
            InputEvent::MouseUp { button } => (Physical::Button(button), false),
            InputEvent::Text { .. } => return Err("text must be assembled before admission".into()),
            _ => return Ok(Command::Control(Control::Event(ev))),
        };
        if down {
            if let Some(p) = self.presses.get(&physical) {
                return Ok(Command::Repeat(*p));
            }
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or("press generations exhausted")?;
            let p = Press {
                physical,
                generation: self.generation,
            };
            self.presses.insert(physical, p);
            Ok(Command::Down(p))
        } else {
            // Generation zero can never own a press, even if another owner holds it.
            Ok(Command::Up(self.presses.remove(&physical).unwrap_or(
                Press {
                    physical,
                    generation: 0,
                },
            )))
        }
    }
}
struct Pending {
    result: oneshot::Sender<Finished>,
    actor: crate::grants::Actor,
    target: Option<crate::grants::Target>,
}
struct State {
    queue: Scheduler,
    ingress: BTreeMap<Owner, Ingress>,
    pending: BTreeMap<u64, Pending>,
    next_owner: u64,
    opens: Bucket,
    streams: Bucket,
    shutdown: bool,
    local: Option<Owner>,
    aliases: BTreeMap<Owner, Owner>,
}
impl State {
    fn deliver(&mut self, results: Vec<Finished>) {
        for done in results {
            if let Some(p) = self.pending.remove(&done.receipt.0) {
                let _ = p.result.send(done);
            }
        }
    }
    fn cancel(&mut self, owner: Owner) {
        self.ingress.remove(&owner);
        let current = self.aliases.remove(&owner).unwrap_or(owner);
        let results = self.queue.cancel_owner(current);
        self.deliver(results);
    }
}
struct Core {
    state: Mutex<State>,
    wake: Condvar,
    stream_slots: Arc<tokio::sync::Semaphore>,
}
struct Lifetime {
    core: Arc<Core>,
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        let mut s = self.core.state.lock().unwrap();
        s.shutdown = true;
        let results = s.queue.cancel_all();
        s.deliver(results);
        self.core.wake.notify_one();
    }
}
#[derive(Clone)]
pub struct Dispatch {
    life: Arc<Lifetime>,
}
pub struct OwnerGuard {
    dispatch: Dispatch,
    owner: Owner,
    cancel_on_drop: bool,
}
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        if !self.cancel_on_drop {
            return;
        }
        let c = &self.dispatch.life.core;
        c.state.lock().unwrap().cancel(self.owner);
        c.wake.notify_one();
    }
}
impl Dispatch {
    pub fn new(sink: Box<dyn InputSink>) -> Self {
        let core = Arc::new(Core {
            state: Mutex::new(State {
                queue: Scheduler::new(Limits::default()),
                ingress: BTreeMap::new(),
                pending: BTreeMap::new(),
                next_owner: 0,
                streams: Bucket::new(128, 64, Instant::now()),
                opens: Bucket::new(128, 64, Instant::now()),
                shutdown: false,
                local: None,
                aliases: BTreeMap::new(),
            }),
            wake: Condvar::new(),
            stream_slots: Arc::new(tokio::sync::Semaphore::new(128)),
        });
        let worker = core.clone();
        std::thread::Builder::new()
            .name("glass-input".into())
            .spawn(move || run(worker, sink))
            .expect("input worker");
        Self {
            life: Arc::new(Lifetime { core }),
        }
    }
    pub fn admit_stream(&self) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        let core = &self.life.core;
        if !core.state.lock().unwrap().streams.take(1, Instant::now()) {
            return Err("stream open rate exceeded".into());
        }
        core.stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "too many incoming streams".into())
    }
    pub fn owner_count(&self) -> usize {
        self.life.core.state.lock().unwrap().ingress.len()
    }
    pub fn pending_count(&self) -> usize {
        self.life.core.state.lock().unwrap().pending.len()
    }
    pub fn local(&self) -> Result<OwnerGuard, String> {
        let mut s = self.life.core.state.lock().unwrap();
        if let Some(owner) = s.local.filter(|o| s.ingress.contains_key(o)) {
            return Ok(OwnerGuard {
                dispatch: self.clone(),
                owner,
                cancel_on_drop: false,
            });
        }
        s.next_owner = s.next_owner.checked_add(1).ok_or("owner ids exhausted")?;
        let id = s.next_owner;
        let owner = s
            .queue
            .open_owner(id, OwnerClass::Local)
            .map_err(|e| format!("local input: {e:?}"))?;
        s.ingress.insert(owner, Ingress::default());
        s.local = Some(owner);
        Ok(OwnerGuard {
            dispatch: self.clone(),
            owner,
            cancel_on_drop: false,
        })
    }
    pub fn control(&self, control: Control) -> Result<Receipt, String> {
        self.local()?.controls(vec![control], None)
    }
    pub fn configure(&self, ids: &[u32], window: Option<[f64; 4]>) {
        if let Err(e) = self.control(Control::Configure {
            displays: ids.into(),
            window,
        }) {
            tracing::warn!("{e}");
        }
    }
    pub fn set_displays(&self, ids: &[u32]) {
        if let Err(e) = self.control(Control::Displays(ids.into())) {
            tracing::warn!("{e}");
        }
    }
    pub fn set_window_rect(&self, rect: Option<[f64; 4]>) {
        if let Err(e) = self.control(Control::Window(rect)) {
            tracing::warn!("{e}");
        }
    }
    pub fn paste(&self) {
        if let Err(e) = self.control(Control::Paste) {
            tracing::warn!("{e}");
        }
    }
    pub fn open(&self, class: OwnerClass) -> Result<OwnerGuard, String> {
        let mut s = self.life.core.state.lock().unwrap();
        if s.shutdown || !s.opens.take(1, Instant::now()) {
            return Err("input owner rate exceeded".into());
        }
        s.next_owner = s.next_owner.checked_add(1).ok_or("owner ids exhausted")?;
        let id = s.next_owner;
        let owner = s
            .queue
            .open_owner(id, class)
            .map_err(|e| format!("input owner: {e:?}"))?;
        s.ingress.insert(owner, Ingress::default());
        Ok(OwnerGuard {
            dispatch: self.clone(),
            owner,
            cancel_on_drop: true,
        })
    }
}
impl OwnerGuard {
    pub fn events(&self, events: Vec<InputEvent>) -> Result<Receipt, String> {
        self.submit(Some(events), Vec::new(), None)
    }
    pub fn text(&self, text: String) -> Result<Receipt, String> {
        self.submit(
            None,
            vec![Command::Text(text.into_boxed_str())],
            Some(crate::grants::Target::Keys),
        )
    }
    pub fn controls(
        &self,
        controls: Vec<Control>,
        target: Option<crate::grants::Target>,
    ) -> Result<Receipt, String> {
        self.submit(
            None,
            controls.into_iter().map(Command::Control).collect(),
            target,
        )
    }
    pub fn events_checked(
        &self,
        events: Vec<InputEvent>,
        target: Option<crate::grants::Target>,
    ) -> Result<Receipt, String> {
        self.submit(Some(events), Vec::new(), target)
    }
    fn submit(
        &self,
        events: Option<Vec<InputEvent>>,
        mut commands: Vec<Command>,
        target: Option<crate::grants::Target>,
    ) -> Result<Receipt, String> {
        let c = &self.dispatch.life.core;
        let mut s = c.state.lock().unwrap();
        let mut ingress = s
            .ingress
            .get(&self.owner)
            .cloned()
            .ok_or("input owner cancelled")?;
        if let Some(events) = events {
            if events.len() > 512 {
                s.cancel(self.owner);
                c.wake.notify_one();
                return Err("input batch too large".into());
            }
            for ev in events {
                commands.push(ingress.event(ev)?);
            }
        }
        let current = s.aliases.get(&self.owner).copied().unwrap_or(self.owner);
        let ids = match s.queue.try_submit_batch(current, commands) {
            Ok(ids) => ids,
            Err(rejected) => {
                s.deliver(rejected.cancelled);
                s.ingress.remove(&self.owner);
                c.wake.notify_one();
                return Err(format!("input admission: {:?}", rejected.error));
            }
        };
        s.ingress.insert(self.owner, ingress);
        let actor = crate::grants::current();
        let results = ids
            .into_iter()
            .map(|id| {
                let (tx, rx) = oneshot::channel();
                s.pending.insert(
                    id.0,
                    Pending {
                        result: tx,
                        actor: actor.clone(),
                        target,
                    },
                );
                rx
            })
            .collect();
        c.wake.notify_one();
        Ok(Receipt { results })
    }
}
fn run(core: Arc<Core>, mut sink: Box<dyn InputSink>) {
    loop {
        let (step, context) = {
            let mut s = core.state.lock().unwrap();
            loop {
                if let Some(step) = s.queue.next_step() {
                    let context = s
                        .pending
                        .get(&step.receipt.0)
                        .map(|p| (p.actor.clone(), p.target));
                    break (step, context);
                }
                if s.shutdown {
                    drop(s);
                    sink.release_all();
                    return;
                }
                s = core.wake.wait(s).unwrap();
            }
        };
        // No scheduler lock, pipe I/O, native call or pacing under the admission mutex.
        let permitted = matches!(step.operation, Operation::Up(_) | Operation::Ownership)
            || context.is_none_or(|(actor, target)| {
                crate::grants::ACTOR.sync_scope(actor, || {
                    target.is_none_or(|target| crate::grants::check_input(target).is_ok())
                })
            });
        let reset = matches!(
            step.operation,
            Operation::Control(
                Control::Displays(_) | Control::Display(_) | Control::Configure { .. }
            )
        );
        let reset_completion = if reset {
            sink.reset_state()
        } else {
            Completion::Completed
        };
        let completion = if reset_completion != Completion::Completed {
            Completion::Unknown
        } else if !permitted {
            Completion::Failed
        } else if step.operation == Operation::Ownership {
            Completion::Completed
        } else {
            sink.execute_step(&step.operation)
        };
        let invalidate = completion == Completion::Unknown
            || (matches!(step.operation, Operation::Up(_)) && completion != Completion::Completed);
        // Cleanup BEFORE complete forgets an unsuccessful Up. Exactly one attempt;
        // uncertainty retires the route ledger instead of repeatedly retrying releases.
        if invalidate {
            // A failed native write may have emitted Down without updating the backend's
            // held set. Release that exact physical input once before retiring the ledger.
            sink.recover_step(&step.operation);
        }
        let outcome = match completion {
            Completion::Completed => Outcome::Completed,
            Completion::Unsupported => Outcome::Unsupported,
            Completion::Partial => Outcome::Partial,
            Completion::Failed => Outcome::Failed,
            Completion::Unknown => Outcome::Unknown,
        };
        let mut s = core.state.lock().unwrap();
        let results = s
            .queue
            .complete(step.token, outcome)
            .expect("single worker token");
        s.deliver(results);
        if outcome != Outcome::Completed {
            let cancelled: Vec<_> = s
                .ingress
                .keys()
                .copied()
                .filter(|o| !s.queue.is_live(s.aliases.get(o).copied().unwrap_or(*o)))
                .collect();
            for owner in cancelled {
                s.ingress.remove(&owner);
                s.aliases.remove(&owner);
            }
        }
        if invalidate || reset {
            let results = s.queue.cancel_all();
            s.deliver(results);
            s.queue.forget_cancelled_holds();
            if reset && !invalidate {
                // Keep live stream identities, but renew scheduler epochs and generations.
                // No queued chord/text survives the display reset.
                let owners: Vec<_> = s.ingress.keys().copied().collect();
                s.aliases.clear();
                for original in owners {
                    let class = if s.local == Some(original) {
                        OwnerClass::Local
                    } else {
                        OwnerClass::Viewer
                    };
                    let Some(id) = s.next_owner.checked_add(1) else {
                        s.ingress.clear();
                        break;
                    };
                    s.next_owner = id;
                    match s.queue.open_owner(id, class) {
                        Ok(owner) => {
                            s.aliases.insert(original, owner);
                            s.ingress.insert(original, Ingress::default());
                        }
                        Err(_) => {
                            s.ingress.remove(&original);
                        }
                    }
                }
            } else {
                s.ingress.clear();
                s.aliases.clear();
            }
        }
    }
}
/// Monotonic token bucket; callers supply time so overload tests need no sleep.
struct Bucket {
    capacity: u64,
    rate: u64,
    tokens: u64,
    at: Instant,
}
impl Bucket {
    fn new(capacity: u64, rate: u64, at: Instant) -> Self {
        Self {
            capacity,
            rate,
            tokens: capacity,
            at,
        }
    }
    fn take(&mut self, n: u64, now: Instant) -> bool {
        let added = now
            .saturating_duration_since(self.at)
            .as_nanos()
            .saturating_mul(self.rate as u128)
            / 1_000_000_000;
        if added > 0 {
            self.tokens = self.capacity.min(
                self.tokens
                    .saturating_add(added.min(u64::MAX as u128) as u64),
            );
            self.at = now;
        }
        if n > self.tokens {
            false
        } else {
            self.tokens -= n;
            true
        }
    }
}

/// Bounded wire assembly. A malformed/oversized piece terminates the owner stream;
/// it can never silently disappear and allow subsequent commands to execute.
pub struct WireInput {
    text: Vec<u8>,
    assembling: bool,
    events: Bucket,
    bytes: Bucket,
}
impl WireInput {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            text: Vec::new(),
            assembling: false,
            events: Bucket::new(8192, 4096, now),
            bytes: Bucket::new(128 * 1024, 64 * 1024, now),
        }
    }
    pub fn charge(&mut self, now: Instant) -> bool {
        self.events.take(1, now) && self.bytes.take(glass_proto::INPUT_EVENT_LEN as u64, now)
    }
    pub fn submit(
        &mut self,
        owner: &OwnerGuard,
        ev: InputEvent,
        now: Instant,
    ) -> Result<(), String> {
        if !self.charge(now) {
            return Err("input rate exceeded".into());
        }
        match ev {
            InputEvent::Text { len, last, bytes } => {
                let len = len as usize;
                if len > bytes.len() || self.text.len() + len > glass_proto::MAX_TEXT_BYTES {
                    return Err("input text limit".into());
                }
                self.assembling = !last;
                self.text.extend_from_slice(&bytes[..len]);
                if last {
                    let text = String::from_utf8(std::mem::take(&mut self.text))
                        .map_err(|_| "invalid input UTF-8")?;
                    owner.text(text)?;
                }
            }
            _ if self.assembling => return Err("interrupted input text".into()),
            _ => {
                owner.events(vec![ev])?;
            }
        }
        Ok(())
    }
}
