//! The host's drive (package 1c-09, DESIGN.md 6.12), gate 3: `Game::drive` stops right for
//! every mix of seats (driven, the host's, hybrid), including a reply awaited from the host's
//! seat (rule T3), or from a driven seat whose driver left the chat to the host (a hybrid seat's
//! model), in and out of that seat's turn, and a limit on seats; it puts the negotiations that
//! wait on a driven seat to its driver, whoever's turn it is, with the game refusing to end a
//! turn inside the answer; and
//! a driver's memory, written in a turn or in an answer, is digested and survives a save and a
//! load, as does a stop inside a turn.

use citar_engine::api::testops;
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::diplomacy::actions::{OpenNegotiation, RespondNegotiation};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    Action, DebugOptions, DriveOptions, DriverOutcome, Drivers, ErrCode, Game, SeatDriver, Stop,
};
use citar_engine::rules::Ruleset;
use citar_engine::save::chain::DigestChain;
use citar_engine::state::Phase;
use citar_engine::state::diplo::NegStatus;
use citar_engine::state::players::{Controller, DriverMemory};
use citar_testkit::agents::RandomAgent;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

/// A bare arena game: `seats` as the lobby sends them, no city-states, barbarians or ruins, and
/// a warrior for each civilization, so none is eliminated as a round ends.
fn arena(seats: &Value, extra: &Value) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let mut cfg = json!({
        "seed": 5,
        "map": doc,
        "players": seats,
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg[k] = v.clone();
    }
    let r = Ruleset::shared();
    let setup = config_from_value(r, cfg).expect("settings");
    let (mut g, _) = Game::new(r, &setup).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    let spots = [(5, 5), (18, 10), (18, 4), (5, 11), (11, 13)];
    let n = g.majors(true).count();
    let ops: Vec<Value> = spots
        .iter()
        .take(n)
        .enumerate()
        .map(|(p, &(x, y))| json!({"op": "add_unit", "player": p, "unit": "Warrior", "x": x, "y": y}))
        .collect();
    g.apply_ops(&Value::Array(ops)).expect("warriors");
    g
}

/// Every check clean.
fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// A driver that plays nothing, answers what waits on it with `answer` (or leaves it, or with
/// `defer` leaves it to the host, as a hybrid seat's bot leaves it to its model), counts its
/// turns and answers in the seat's memory (turns, then answers), and checks as it answers that
/// the game will not end a turn inside it.
struct Tester {
    answer: Option<&'static str>,
    defer: bool,
    turns: u32,
    answers: u32,
    /// A negotiation to open, with this player, on its next turn.
    open_with: Option<PlayerId>,
}

impl Tester {
    fn new(answer: Option<&'static str>) -> Self {
        Self { answer, defer: false, turns: 0, answers: 0, open_with: None }
    }
}

fn bump(mem: &mut DriverMemory, at: usize) {
    let mut b = mem.bytes().to_vec();
    b.resize(2, 0);
    b[at] += 1;
    *mem = DriverMemory::new(9, 1, b).expect("two bytes");
}

impl SeatDriver for Tester {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        self.turns += 1;
        bump(mem, 0);
        if let Some(to) = self.open_with.take() {
            let a = OpenNegotiation {
                to: i64::from(to.0),
                message: json!("Shall we talk?"),
                give: None,
                receive: None,
            };
            g.act(pid, Action::OpenNegotiation(a)).expect("it opens");
        }
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        assert_eq!(g.negotiation(nid).and_then(|n| n.awaiting), Some(pid), "it waits on it");
        // Inside an answer, as inside a turn, nobody ends a turn but the drive.
        let e = g.end_turn(g.current()).expect_err("driving");
        assert_eq!(e.code, ErrCode::Rule);
        self.answers += 1;
        bump(mem, 1);
        if self.defer {
            return DriverOutcome::Deferred;
        }
        if let Some(action) = self.answer {
            let a = RespondNegotiation {
                negotiation_id: i64::from(nid.get()),
                action: json!(action),
                message: Some(json!("From the tester.")),
                give: None,
                receive: None,
            };
            let _refused = g.act(pid, Action::RespondNegotiation(a));
        }
        DriverOutcome::Done
    }
}

fn memory(g: &Game, p: PlayerId) -> Vec<u8> {
    g.player(p).and_then(|x| x.seat().driver()).map(|m| m.bytes().to_vec()).unwrap_or_default()
}

#[test]
fn every_seat_driven_plays_to_the_end_and_a_host_seat_stops_the_drive_at_its_turn() {
    let mut g = arena(&json!([{}, {}, {}]), &json!({"turn_limit": 3}));
    let (mut a, mut b, mut c) = (Tester::new(None), Tester::new(None), Tester::new(None));
    // Seat 1 is the host's: the drive stops at its turn each round.
    let mut d = Drivers::none(3).with(PlayerId(0), &mut a).with(PlayerId(2), &mut c);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    assert_eq!((g.turn(), g.current()), (1, PlayerId(1)));
    g.end_turn(PlayerId(1)).expect("the host ends it");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    assert_eq!((g.turn(), g.current()), (2, PlayerId(1)));
    // Every seat driven: to the turn limit.
    let mut d = Drivers::none(3)
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b)
        .with(PlayerId(2), &mut c);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::GameOver);
    assert_eq!(g.phase(), Phase::Over);
    assert_eq!([a.turns, b.turns, c.turns], [3, 2, 3]);
    clean(&mut g);
    // No driver at all: the first seat is the host's.
    let mut g = arena(&json!([{}, {}]), &json!({}));
    let (stop, batch) = g.drive(&mut Drivers::none(2), DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(0)));
    assert!(batch.is_empty());
}

#[test]
fn a_seat_limit_returns_between_driven_seats_and_the_next_drive_goes_on() {
    let run = |limit: u32| {
        let mut g = arena(&json!([{}, {}, {}]), &json!({"turn_limit": 4}));
        g.set_chain(Some(DigestChain::new(b"limit")));
        let (mut a, mut b, mut c) = (Tester::new(None), Tester::new(None), Tester::new(None));
        let mut d = Drivers::none(3)
            .with(PlayerId(0), &mut a)
            .with(PlayerId(1), &mut b)
            .with(PlayerId(2), &mut c);
        let opts = DriveOptions::default().with_seat_limit(limit);
        let mut calls = 0;
        loop {
            calls += 1;
            let before = g.current();
            let (stop, _) = g.drive(&mut d, opts).expect("live");
            match stop {
                Stop::SeatLimit => {
                    assert!(limit > 0);
                    assert_ne!(g.current(), before, "it ended at least one turn");
                }
                Stop::GameOver => break,
                other => panic!("{other:?}"),
            }
        }
        (calls, g.chain().copied(), g.digest().ok())
    };
    let (calls, chain, digest) = run(0);
    assert_eq!(calls, 1, "no limit: one call");
    // Twelve turns: a call per seat, and one to find the game over.
    let (one, chain1, digest1) = run(1);
    assert_eq!(one, 12);
    let (two, chain2, digest2) = run(2);
    assert_eq!(two, 6);
    assert_eq!((chain1, digest1), (chain, digest), "the same game however often it returns");
    assert_eq!((chain2, digest2), (chain, digest));
}

#[test]
fn a_hybrid_seat_stops_for_its_diplomat_once_a_turn() {
    let mut g = arena(&json!([{"controller": "hybrid"}, {"controller": "bot"}]), &json!({}));
    let (mut a, mut b) = (Tester::new(None), Tester::new(None));
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    assert_eq!((g.turn(), g.current()), (1, PlayerId(0)));
    // The diplomat acts on the seat's turn, as a model does through the tools.
    let noted = json!({"tool": "write_notes", "text": "From the diplomat."});
    let action: Action = serde_json::from_value(noted).expect("an action");
    g.act(PlayerId(0), action).expect("the diplomat writes");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    assert_eq!((g.turn(), g.current()), (2, PlayerId(0)));
    drop(d);
    assert_eq!([a.turns, b.turns], [2, 1], "the hybrid seat's driver played each turn once");
    // A seat that is no longer hybrid is driven through.
    g.set_controller(PlayerId(0), Controller::Bot, None, Default::default()).expect("bot");
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default().with_seat_limit(3)).expect("live");
    assert_eq!(stop, Stop::SeatLimit);
    assert_eq!((g.turn(), g.current()), (3, PlayerId(1)));
    drop(d);
    // Its second turn ended, the bot's second and its third were played and ended.
    assert_eq!([a.turns, b.turns], [3, 2]);
    clean(&mut g);
}

#[test]
fn a_driven_seat_waits_for_the_host_seats_answer_and_its_driver_answers_back() {
    let mut g = arena(&json!([{"controller": "bot"}, {}]), &json!({}));
    g.meet(PlayerId(0), PlayerId(1)).expect("they meet");
    let mut a = Tester::new(Some("reply"));
    a.open_with = Some(PlayerId(1));
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    let nid = NegotiationId::new(1).expect("the first");
    assert!(
        matches!(&stop, Stop::AwaitingReply { pid: PlayerId(0), nids } if nids.as_slice() == [nid]),
        "{stop:?}"
    );
    assert_eq!(g.current(), PlayerId(0), "its turn waits");
    // Nothing moved: it waits still, and its driver does not play again.
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert!(matches!(stop, Stop::AwaitingReply { pid: PlayerId(0), .. }));
    assert!(batch.is_empty());
    // The host's seat answers out of turn: the driver answers back, and it waits again.
    let reply = RespondNegotiation {
        negotiation_id: 1,
        action: json!("reply"),
        message: Some(json!("Of what?")),
        give: None,
        receive: None,
    };
    g.act(PlayerId(1), Action::RespondNegotiation(reply.clone())).expect("an answer");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert!(matches!(stop, Stop::AwaitingReply { pid: PlayerId(0), .. }));
    assert_eq!(g.negotiation(nid).map(|n| n.history.len()), Some(3));
    // The host's wait runs out: it closes the negotiation, and the turn ends.
    g.close_negotiation(nid, NegStatus::Expired, "No answer in time.", None).expect("closed");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    drop(d);
    assert_eq!((a.turns, a.answers), (1, 1));
    assert_eq!(memory(&g, PlayerId(0)), [1, 1], "a turn and an answer, in the seat's memory");
    clean(&mut g);
}

/// The one stop that names `nid` as awaited by driven seat `pid`, which it waits on as `on`.
fn awaits(g: &Game, stop: &Stop, pid: PlayerId, nid: NegotiationId, on: PlayerId) {
    assert!(
        matches!(stop, Stop::AwaitingReply { pid: p, nids } if *p == pid && nids.as_slice() == [nid]),
        "{stop:?}"
    );
    assert_eq!(g.current(), pid, "its turn waits");
    let n = g.negotiation(nid).expect("it exists");
    assert_eq!((n.status, n.awaiting), (NegStatus::Open, Some(on)));
}

fn answer_as(g: &mut Game, p: PlayerId, nid: NegotiationId, action: &str) {
    let a = RespondNegotiation {
        negotiation_id: i64::from(nid.get()),
        action: json!(action),
        message: Some(json!("From the model.")),
        give: None,
        receive: None,
    };
    g.act(p, Action::RespondNegotiation(a)).expect("an answer");
}

#[test]
fn a_chat_a_driver_leaves_to_the_host_holds_the_turn_in_and_out_of_the_hybrid_seats_turn() {
    // Seat 0 is hybrid: its bot leaves every chat to the seat's model (the host). Seat 1 is a
    // bot that replies to whatever waits on it.
    let mut g = arena(&json!([{"controller": "hybrid"}, {"controller": "bot"}]), &json!({}));
    g.meet(PlayerId(0), PlayerId(1)).expect("they meet");
    let mut a = Tester::new(None);
    a.defer = true;
    let mut b = Tester::new(Some("reply"));
    b.open_with = Some(PlayerId(0));
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    // Out of the hybrid seat's turn: the bot opens a chat with it, its driver leaves the chat to
    // the model, and the bot's turn waits for the answer instead of ending (and the chat
    // expiring) before the model has seen it.
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    let first = NegotiationId::new(1).expect("the first");
    awaits(&g, &stop, PlayerId(1), first, PlayerId(0));
    assert_eq!(g.turn(), 1);
    // Nothing moved: the next drive asks the hybrid seat's driver again, which leaves it again.
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    awaits(&g, &stop, PlayerId(1), first, PlayerId(0));
    // The model answers; the bot replies to it, and the chat is the model's again.
    answer_as(&mut g, PlayerId(0), first, "reply");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    awaits(&g, &stop, PlayerId(1), first, PlayerId(0));
    assert_eq!(g.negotiation(first).map(|n| n.history.len()), Some(3));
    // The model ends it: the bot's turn ends, and the hybrid seat's next turn is played.
    answer_as(&mut g, PlayerId(0), first, "reject");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    assert_eq!((g.turn(), g.current()), (2, PlayerId(0)));
    // In its own turn: the model opens a chat with the bot, which replies; the hybrid seat's
    // driver leaves the reply to the model, and the turn waits on it rather than ending.
    let open = OpenNegotiation { to: 1, message: json!("Trade?"), give: None, receive: None };
    g.act(PlayerId(0), Action::OpenNegotiation(open)).expect("the model opens");
    let second = NegotiationId::new(2).expect("the second");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    awaits(&g, &stop, PlayerId(0), second, PlayerId(0));
    assert_eq!(g.turn(), 2);
    drop(d);
    // The model's wait runs out: the host has the bot decide, driving with a driver that
    // answers this time, and the turn ends.
    a.defer = false;
    a.answer = Some("reject");
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default().with_seat_limit(1)).expect("live");
    assert_eq!(stop, Stop::SeatLimit);
    assert_eq!((g.turn(), g.current()), (2, PlayerId(1)));
    assert_eq!(g.negotiation(second).map(|n| n.status), Some(NegStatus::Rejected));
    drop(d);
    assert_eq!((a.turns, b.turns), (2, 1), "no seat was played twice");
    // A driver that leaves a chat without deferring it has answered: the turn ends, and the chat
    // expires with it.
    a.answer = None;
    b.open_with = Some(PlayerId(0));
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    assert_eq!(g.turn(), 3);
    let third = NegotiationId::new(3).expect("the third");
    assert_eq!(g.negotiation(third).map(|n| n.status), Some(NegStatus::Expired));
    drop(d);
    clean(&mut g);
}

#[test]
fn a_negotiation_the_host_seat_opens_gets_the_drivers_answer_on_the_next_drive() {
    let mut g = arena(&json!([{}, {"controller": "bot"}]), &json!({}));
    g.meet(PlayerId(0), PlayerId(1)).expect("they meet");
    let open = OpenNegotiation {
        to: 1,
        message: json!("Friends?"),
        give: None,
        receive: Some(json!([{"type": "declaration_of_friendship"}])),
    };
    g.act(PlayerId(0), Action::OpenNegotiation(open)).expect("opened");
    let nid = NegotiationId::new(1).expect("the first");
    let mut b = Tester::new(Some("accept"));
    let mut d = Drivers::none(2).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(0)), "the host's seat plays on");
    drop(d);
    assert_eq!(b.answers, 1);
    assert_eq!(g.negotiation(nid).map(|n| n.status), Some(NegStatus::Accepted));
    assert_eq!(memory(&g, PlayerId(1)), [0, 1], "an answer, and no turn yet");
    // A driver that leaves it is not asked again until it moves.
    let open = OpenNegotiation { to: 1, message: json!("Anything?"), give: None, receive: None };
    g.end_turn(PlayerId(0)).expect("its turn ends");
    let mut b = Tester::new(None);
    let mut d = Drivers::none(2).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(0)));
    g.act(PlayerId(0), Action::OpenNegotiation(open)).expect("opened");
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(0)));
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(0)));
    drop(d);
    assert_eq!((b.turns, b.answers), (1, 2), "asked once each drive while it waits");
    clean(&mut g);
}

#[test]
fn a_stop_inside_a_turn_and_the_drivers_memory_survive_a_save_and_are_digested() {
    let mut g = arena(&json!([{"controller": "hybrid"}, {}]), &json!({}));
    let mut a = Tester::new(None);
    let mut d = Drivers::none(2).with(PlayerId(0), &mut a);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    drop(d);
    assert_eq!(memory(&g, PlayerId(0)), [1, 0]);
    let digest = g.digest().expect("a digest");
    let json = g.snapshot().to_json().expect("a save");
    let chunk = g.take_journal_chunk().expect("history").map(|c| c.json).unwrap_or_default();
    let (mut loaded, _) =
        Game::load(g.rules(), &json, &mut std::iter::once(chunk.as_slice())).expect("it loads");
    assert_eq!(loaded.digest().ok(), Some(digest));
    assert_eq!(memory(&loaded, PlayerId(0)), [1, 0], "the memory came back");
    // The stop came back too: the loaded game ends the turn rather than playing it again.
    let mut again = Tester::new(None);
    let mut d = Drivers::none(2).with(PlayerId(0), &mut again);
    let (stop, _) = loaded.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    drop(d);
    assert_eq!(again.turns, 0, "the loaded game's seat was not played twice");
    // A driver's memory moves the digest.
    let mut other = arena(&json!([{"controller": "hybrid"}, {}]), &json!({}));
    let mut quiet = Quiet;
    let mut d = Drivers::none(2).with(PlayerId(0), &mut quiet);
    let (stop, _) = other.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::HybridDiplomat(PlayerId(0)));
    drop(d);
    assert!(memory(&other, PlayerId(0)).is_empty());
    assert_ne!(other.digest().ok(), Some(digest), "the memory is in the digest");
}

/// A driver that plays nothing and keeps nothing.
struct Quiet;

impl SeatDriver for Quiet {
    fn play_turn(&mut self, _: &mut Game, _: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        DriverOutcome::Done
    }
}

#[test]
fn random_agents_that_leave_chats_to_the_drive_get_their_answers() {
    // Three random agents who have met, with cities, gold and techs to trade, two driven and the
    // third the host's. The drive answers the chats between the driven two before the opener's
    // turn ends; theirs with the host's seat stop it (rule T3) until the host closes them; and
    // those the host's seat opens get the drivers' answers when the host drives before it ends
    // its turn.
    let mut g = arena(&json!([{}, {}, {}]), &json!({"turn_limit": 100}));
    g.apply_ops(&json!([
        {"op": "found_city", "player": 0, "x": 5, "y": 5},
        {"op": "found_city", "player": 1, "x": 18, "y": 10},
        {"op": "found_city", "player": 2, "x": 18, "y": 4},
        {"op": "meet", "a": 0, "b": "all"},
        {"op": "meet", "a": 1, "b": 2},
        {"op": "set_player", "player": [0, 1, 2], "gold": 300},
        {"op": "grant_tech", "player": [0, 1, 2], "techs": ["Civil Service", "Education"]},
        {"op": "set_relation", "a": 0, "b": 1, "embassies": true},
        {"op": "set_relation", "a": 0, "b": 2, "embassies": true},
        {"op": "set_relation", "a": 1, "b": 2, "embassies": true},
    ]))
    .expect("set up");
    let (mut a, mut b, mut c) = (RandomAgent::new(), RandomAgent::new(), RandomAgent::new());
    let mut awaited = 0;
    let mut host_turns = 0;
    loop {
        let mut d = Drivers::none(3).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
        let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
        match stop {
            Stop::GameOver => break,
            Stop::AwaitingReply { pid, nids } => {
                awaited += 1;
                assert!(pid == PlayerId(0) || pid == PlayerId(1));
                for nid in nids {
                    let n = g.negotiation(nid).expect("it exists");
                    assert_eq!((n.status, n.awaiting), (NegStatus::Open, Some(PlayerId(2))));
                    g.close_negotiation(nid, NegStatus::Expired, "No answer.", None)
                        .expect("closed");
                }
            }
            Stop::External(p) => {
                assert_eq!(p, PlayerId(2));
                host_turns += 1;
                // The host plays its seat as it likes, here with an agent of its own; drives,
                // which gets the drivers' answers to what it opened; and ends the turn.
                c.play_turn(&mut g, p, &mut DriverMemory::empty(0, 0));
                let (again, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
                assert!(matches!(again, Stop::External(q) if q == p) || again == Stop::GameOver);
                if again == Stop::GameOver {
                    break;
                }
                g.end_turn(p).expect("the host ends it");
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(host_turns, 100);
    assert!(awaited > 0, "the agents' chats with the host's seat waited for it");
    let answered = |n: &&citar_engine::state::diplo::Negotiation| {
        n.history.iter().any(|e| e.by == Some(n.responder))
    };
    // Between the driven two, every chat was answered before its opener's turn ended, unless a
    // war cancelled it first.
    let between: Vec<_> =
        g.negotiations().iter().filter(|n| n.initiator.0 + n.responder.0 == 1).collect();
    assert!(!between.is_empty(), "the agents opened chats with each other");
    assert!(
        between.iter().all(|n| answered(n) || n.status == NegStatus::Cancelled),
        "every chat between driven agents was answered"
    );
    // Those the host's seat opened were answered by the drivers.
    let hosts: Vec<_> = g.negotiations().iter().filter(|n| n.initiator == PlayerId(2)).collect();
    assert!(!hosts.is_empty(), "the host's seat opened chats");
    assert!(hosts.iter().all(|n| answered(n) || n.status == NegStatus::Cancelled));
    assert!(!g.state().diplo().deals.is_empty(), "deals were struck through the drive");
    assert!(g.negotiations().iter().all(|n| n.status != NegStatus::Open), "none left open");
    clean(&mut g);
}
