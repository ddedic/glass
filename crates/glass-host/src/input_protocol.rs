//! Fixed-size, versioned single-credit native-step protocol (no semantic text queue).
use crate::{
    input_queue::{Control, Operation, Physical},
    text_input::Completion,
};
pub const REQUEST_LEN: usize = 80;
pub const ACK_LEN: usize = 18;
pub const VERSION: u8 = 1;
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub epoch: u64,
    pub sequence: u64,
    pub operation: Operation,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ack {
    pub epoch: u64,
    pub sequence: u64,
    pub completion: Completion,
}
impl Request {
    pub fn encode(&self) -> Option<[u8; REQUEST_LEN]> {
        let mut b = [0; REQUEST_LEN];
        b[0] = VERSION;
        b[1..9].copy_from_slice(&self.epoch.to_le_bytes());
        b[9..17].copy_from_slice(&self.sequence.to_le_bytes());
        let d = &mut b[18..];
        let tag = match &self.operation {
            Operation::Ownership => 0,
            Operation::Down(p) | Operation::Repeat(p) | Operation::Up(p) => {
                let base = match self.operation {
                    Operation::Down(_) => 1,
                    Operation::Repeat(_) => 3,
                    _ => 5,
                };
                match p {
                    Physical::Key(k) => {
                        d[..2].copy_from_slice(&k.to_le_bytes());
                        base
                    }
                    Physical::Button(v) => {
                        d[0] = *v;
                        base + 1
                    }
                }
            }
            Operation::Scalar(c) => {
                d[..4].copy_from_slice(&(*c as u32).to_le_bytes());
                7
            }
            Operation::Control(Control::Event(ev)) => {
                if matches!(
                    ev,
                    glass_proto::InputEvent::KeyDown { .. }
                        | glass_proto::InputEvent::KeyUp { .. }
                        | glass_proto::InputEvent::MouseDown { .. }
                        | glass_proto::InputEvent::MouseUp { .. }
                        | glass_proto::InputEvent::Text { .. }
                ) {
                    return None;
                }
                d[..glass_proto::INPUT_EVENT_LEN].copy_from_slice(&ev.write());
                8
            }
            Operation::Control(Control::MoveAbsolute { x, y }) => {
                d[..4].copy_from_slice(&x.to_le_bytes());
                d[4..8].copy_from_slice(&y.to_le_bytes());
                16
            }
            Operation::Control(Control::Reset) => 14,
            Operation::Control(Control::Configure { displays, window }) => {
                if displays.len() > glass_proto::MAX_STREAMS {
                    return None;
                }
                d[0] = displays.len() as u8;
                for (i, id) in displays.iter().enumerate() {
                    d[1 + i * 4..5 + i * 4].copy_from_slice(&id.to_le_bytes());
                }
                if let Some(rect) = window {
                    d[17] = 1;
                    for (i, v) in rect.iter().enumerate() {
                        if !v.is_finite() {
                            return None;
                        }
                        d[18 + i * 8..26 + i * 8].copy_from_slice(&v.to_le_bytes());
                    }
                }
                15
            }
            Operation::Control(Control::Paste) => 9,
            Operation::Control(Control::Display(id)) => {
                d[..4].copy_from_slice(&id.to_le_bytes());
                10
            }
            Operation::Control(Control::Displays(ids)) => {
                if ids.len() > glass_proto::MAX_STREAMS {
                    return None;
                }
                d[0] = ids.len() as u8;
                for (i, id) in ids.iter().enumerate() {
                    d[1 + i * 4..5 + i * 4].copy_from_slice(&id.to_le_bytes());
                }
                11
            }
            Operation::Control(Control::Window(rect)) => {
                if let Some(rect) = rect {
                    d[0] = 1;
                    for (i, v) in rect.iter().enumerate() {
                        if !v.is_finite() {
                            return None;
                        }
                        d[1 + i * 8..9 + i * 8].copy_from_slice(&v.to_le_bytes());
                    }
                }
                12
            }
            Operation::Control(Control::Move { display, x, y }) => {
                if !x.is_finite() || !y.is_finite() {
                    return None;
                }
                d[..4].copy_from_slice(&display.to_le_bytes());
                d[4..8].copy_from_slice(&x.to_le_bytes());
                d[8..12].copy_from_slice(&y.to_le_bytes());
                13
            }
        };
        b[17] = tag;
        Some(b)
    }
    pub fn decode(b: &[u8; REQUEST_LEN]) -> Option<Self> {
        if b[0] != VERSION {
            return None;
        }
        let epoch = u64::from_le_bytes(b[1..9].try_into().ok()?);
        let sequence = u64::from_le_bytes(b[9..17].try_into().ok()?);
        if epoch == 0 || sequence == 0 {
            return None;
        }
        let d = &b[18..];
        let operation = match b[17] {
            0 => Operation::Ownership,
            tag @ 1..=6 => {
                let p = if tag % 2 == 1 {
                    Physical::Key(u16::from_le_bytes(d[..2].try_into().ok()?))
                } else {
                    if d[0] > 2 {
                        return None;
                    }
                    Physical::Button(d[0])
                };
                match tag {
                    1 | 2 => Operation::Down(p),
                    3 | 4 => Operation::Repeat(p),
                    _ => Operation::Up(p),
                }
            }
            7 => Operation::Scalar(char::from_u32(u32::from_le_bytes(d[..4].try_into().ok()?))?),
            8 => Operation::Control(Control::Event(glass_proto::InputEvent::read(
                d[..glass_proto::INPUT_EVENT_LEN].try_into().ok()?,
            )?)),
            16 => Operation::Control(Control::MoveAbsolute {
                x: i32::from_le_bytes(d[..4].try_into().ok()?),
                y: i32::from_le_bytes(d[4..8].try_into().ok()?),
            }),
            14 => Operation::Control(Control::Reset),
            15 => {
                let n = d[0] as usize;
                if n > glass_proto::MAX_STREAMS || d[17] > 1 {
                    return None;
                }
                Operation::Control(Control::Configure {
                    displays: (0..n)
                        .map(|i| u32::from_le_bytes(d[1 + i * 4..5 + i * 4].try_into().unwrap()))
                        .collect(),
                    window: (d[17] == 1).then(|| {
                        std::array::from_fn(|i| {
                            f64::from_le_bytes(d[18 + i * 8..26 + i * 8].try_into().unwrap())
                        })
                    }),
                })
            }
            9 => Operation::Control(Control::Paste),
            10 => Operation::Control(Control::Display(u32::from_le_bytes(
                d[..4].try_into().ok()?,
            ))),
            11 => {
                let n = d[0] as usize;
                if n > glass_proto::MAX_STREAMS {
                    return None;
                }
                Operation::Control(Control::Displays(
                    (0..n)
                        .map(|i| u32::from_le_bytes(d[1 + i * 4..5 + i * 4].try_into().unwrap()))
                        .collect(),
                ))
            }
            12 => {
                if d[0] > 1 {
                    return None;
                }
                Operation::Control(Control::Window((d[0] == 1).then(|| {
                    std::array::from_fn(|i| {
                        f64::from_le_bytes(d[1 + i * 8..9 + i * 8].try_into().unwrap())
                    })
                })))
            }
            13 => Operation::Control(Control::Move {
                display: u32::from_le_bytes(d[..4].try_into().ok()?),
                x: f32::from_le_bytes(d[4..8].try_into().ok()?),
                y: f32::from_le_bytes(d[8..12].try_into().ok()?),
            }),
            _ => return None,
        };
        let result = Self {
            epoch,
            sequence,
            operation,
        };
        // Canonical re-encoding checks bypass events, non-finite values and padding.
        (result.encode()? == *b).then_some(result)
    }
}
impl Ack {
    pub fn encode(self) -> [u8; ACK_LEN] {
        let mut b = [0; ACK_LEN];
        b[0] = VERSION;
        b[1..9].copy_from_slice(&self.epoch.to_le_bytes());
        b[9..17].copy_from_slice(&self.sequence.to_le_bytes());
        b[17] = match self.completion {
            Completion::Unsupported => 0,
            Completion::Completed => 1,
            Completion::Partial => 2,
            Completion::Failed => 3,
            Completion::Unknown => 4,
        };
        b
    }
    pub fn decode(b: &[u8; ACK_LEN]) -> Option<Self> {
        if b[0] != VERSION {
            return None;
        }
        Some(Self {
            epoch: u64::from_le_bytes(b[1..9].try_into().ok()?),
            sequence: u64::from_le_bytes(b[9..17].try_into().ok()?),
            completion: match b[17] {
                0 => Completion::Unsupported,
                1 => Completion::Completed,
                2 => Completion::Partial,
                3 => Completion::Failed,
                4 => Completion::Unknown,
                _ => return None,
            },
        })
    }
}
#[derive(Default)]
pub struct Credit {
    epoch: u64,
    sequence: u64,
    outstanding: bool,
    invalid: bool,
}
impl Credit {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            ..Self::default()
        }
    }
    pub fn begin(&mut self, operation: Operation) -> Option<Request> {
        if self.invalid || self.outstanding {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        self.outstanding = true;
        Some(Request {
            epoch: self.epoch,
            sequence: self.sequence,
            operation,
        })
    }
    pub fn complete(&mut self, ack: Ack) -> Completion {
        if self.invalid
            || !self.outstanding
            || ack.epoch != self.epoch
            || ack.sequence != self.sequence
        {
            self.invalidate();
            return Completion::Unknown;
        }
        self.outstanding = false;
        if ack.completion == Completion::Unknown {
            self.invalidate();
        }
        ack.completion
    }
    pub fn invalidate(&mut self) {
        self.invalid = true;
        self.outstanding = false;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_credit_late_ack_timeout_and_route_epoch() {
        let mut c = Credit::new(7);
        let request = c.begin(Operation::Scalar('😀')).unwrap();
        assert!(c.begin(Operation::Scalar('b')).is_none());
        c.invalidate(); // timeout / disconnect / route change
        assert_eq!(
            c.complete(Ack {
                epoch: 7,
                sequence: request.sequence,
                completion: Completion::Completed
            }),
            Completion::Unknown
        );
        assert!(c.begin(Operation::Scalar('c')).is_none());
        let mut replacement = Credit::new(8);
        replacement.begin(Operation::Scalar('d')).unwrap();
        assert_eq!(
            replacement.complete(Ack {
                epoch: 7,
                sequence: request.sequence,
                completion: Completion::Completed
            }),
            Completion::Unknown
        );
    }
    #[test]
    fn bounded_roundtrip_scalar_control_and_completion() {
        for operation in [
            Operation::Scalar('😀'),
            Operation::Up(Physical::Key(56)),
            Operation::Control(Control::Displays(vec![1, 2, 3, 4].into_boxed_slice())),
            Operation::Control(Control::Window(Some([1., 2., 3., 4.]))),
        ] {
            let request = Request {
                epoch: 17,
                sequence: 42,
                operation,
            };
            assert_eq!(Request::decode(&request.encode().unwrap()), Some(request));
        }
        for completion in [
            Completion::Completed,
            Completion::Unsupported,
            Completion::Partial,
            Completion::Failed,
            Completion::Unknown,
        ] {
            let ack = Ack {
                epoch: 1,
                sequence: 2,
                completion,
            };
            assert_eq!(Ack::decode(&ack.encode()), Some(ack));
        }
    }
    #[test]
    fn rejects_text_events_and_unbounded_display_payloads() {
        let request = Request {
            epoch: 1,
            sequence: 1,
            operation: Operation::Control(Control::Displays(vec![0; 5].into_boxed_slice())),
        };
        assert!(request.encode().is_none());
        let mut malformed = [0; REQUEST_LEN];
        malformed[0] = 99;
        assert!(Request::decode(&malformed).is_none());
    }
}

/// Execution-only connection: capture traffic never shares this transport.
pub trait Transport {
    fn exchange(
        &mut self,
        request: &[u8; REQUEST_LEN],
        deadline: std::time::Instant,
    ) -> Result<[u8; ACK_LEN], ()>;
    fn cancel(&mut self);
}
pub struct Client<T> {
    transport: T,
    credit: Credit,
}
impl<T: Transport> Client<T> {
    pub fn new(transport: T, epoch: u64) -> Self {
        Self {
            transport,
            credit: Credit::new(epoch),
        }
    }
    pub fn execute(&mut self, operation: Operation) -> Completion {
        let Some(request) = self.credit.begin(operation) else {
            return Completion::Unknown;
        };
        let Some(bytes) = request.encode() else {
            self.credit.invalidate();
            self.transport.cancel();
            return Completion::Unsupported;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let result = self
            .transport
            .exchange(&bytes, deadline)
            .ok()
            .and_then(|bytes| Ack::decode(&bytes));
        let completion = match result {
            Some(ack) => self.credit.complete(ack),
            None => {
                self.credit.invalidate();
                Completion::Unknown
            }
        };
        if completion == Completion::Unknown {
            self.transport.cancel();
        }
        completion
    }
}
#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    struct FakeTransport {
        requests: mpsc::SyncSender<Request>,
        replies: mpsc::Receiver<Result<Ack, ()>>,
        cancelled: Arc<AtomicUsize>,
    }
    impl Transport for FakeTransport {
        fn exchange(
            &mut self,
            request: &[u8; REQUEST_LEN],
            _: std::time::Instant,
        ) -> Result<[u8; ACK_LEN], ()> {
            self.requests
                .send(Request::decode(request).unwrap())
                .unwrap();
            self.replies.recv().unwrap().map(Ack::encode)
        }
        fn cancel(&mut self) {
            self.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn delayed_ack_is_single_credit_and_capture_backpressure_is_independent() {
        let (requests, rx) = mpsc::sync_channel(1);
        let (tx, replies) = mpsc::channel();
        let cancelled = Arc::new(AtomicUsize::new(0));
        let capture = std::sync::Mutex::new(());
        let _capture_writer = capture.lock().unwrap();
        let worker = std::thread::spawn(move || {
            let mut c = Client::new(
                FakeTransport {
                    requests,
                    replies,
                    cancelled,
                },
                11,
            );
            assert_eq!(c.execute(Operation::Scalar('a')), Completion::Completed);
            assert_eq!(
                c.execute(Operation::Up(Physical::Key(49))),
                Completion::Completed
            );
        });
        let a = rx.recv().unwrap();
        assert_eq!(a.sequence, 1);
        assert!(rx.try_recv().is_err());
        tx.send(Ok(Ack {
            epoch: a.epoch,
            sequence: a.sequence,
            completion: Completion::Completed,
        }))
        .unwrap();
        let up = rx.recv().unwrap();
        assert_eq!(up.sequence, 2);
        tx.send(Ok(Ack {
            epoch: up.epoch,
            sequence: up.sequence,
            completion: Completion::Completed,
        }))
        .unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn timeout_eof_and_late_ack_cancel_without_replay() {
        for reply in [
            Err(()),
            Ok(Ack {
                epoch: 8,
                sequence: 1,
                completion: Completion::Completed,
            }),
            Ok(Ack {
                epoch: 7,
                sequence: 99,
                completion: Completion::Completed,
            }),
        ] {
            let (requests, rx) = mpsc::sync_channel(1);
            let (tx, replies) = mpsc::channel();
            let cancelled = Arc::new(AtomicUsize::new(0));
            tx.send(reply).unwrap();
            let mut c = Client::new(
                FakeTransport {
                    requests,
                    replies,
                    cancelled: cancelled.clone(),
                },
                7,
            );
            assert_eq!(c.execute(Operation::Scalar('a')), Completion::Unknown);
            assert_eq!(c.execute(Operation::Scalar('b')), Completion::Unknown);
            assert_eq!(rx.try_iter().count(), 1);
            assert_eq!(cancelled.load(Ordering::SeqCst), 1);
        }
    }
}

/// Shared helper credit and out-of-band cancellation. There is no text or command
/// backlog here; after EOF even a previously admitted request cannot start execution.
#[derive(Default)]
pub struct HelperSession {
    closed: std::sync::atomic::AtomicBool,
    outstanding: std::sync::atomic::AtomicBool,
    identity: std::sync::Mutex<(Option<u64>, u64)>,
}
impl HelperSession {
    pub fn admit(&self, request: &Request) -> bool {
        use std::sync::atomic::Ordering;
        let mut identity = self.identity.lock().unwrap();
        if self.closed.load(Ordering::Acquire)
            || self.outstanding.swap(true, Ordering::AcqRel)
            || identity.0.is_some_and(|epoch| epoch != request.epoch)
            || identity.1.checked_add(1) != Some(request.sequence)
        {
            self.cancel();
            return false;
        }
        *identity = (Some(request.epoch), request.sequence);
        true
    }
    pub fn can_execute(&self) -> bool {
        !self.closed.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn completed(&self) {
        self.outstanding
            .store(false, std::sync::atomic::Ordering::Release);
    }
    pub fn cancel(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
#[cfg(test)]
mod helper_tests {
    use super::*;
    #[test]
    fn eof_cancels_admitted_step_and_never_drains_next_scalar() {
        let gate = HelperSession::default();
        let first = Request {
            epoch: 1,
            sequence: 1,
            operation: Operation::Scalar('a'),
        };
        assert!(gate.admit(&first));
        gate.cancel();
        assert!(!gate.can_execute());
        gate.completed(); // late native completion cannot reopen the connection
        assert!(!gate.admit(&Request {
            sequence: 2,
            ..first
        }));
    }
    #[test]
    fn second_outstanding_request_and_route_change_close_helper_session() {
        for outstanding in [true, false] {
            let gate = HelperSession::default();
            let first = Request {
                epoch: 1,
                sequence: 1,
                operation: Operation::Scalar('a'),
            };
            assert!(gate.admit(&first));
            if !outstanding {
                gate.completed();
            }
            assert!(!gate.admit(&Request {
                epoch: if outstanding { 1 } else { 2 },
                sequence: 2,
                ..first
            }));
            assert!(!gate.can_execute());
        }
    }
}
