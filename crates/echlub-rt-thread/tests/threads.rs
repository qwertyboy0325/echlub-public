//! What this machine grants, without opening an audio device: a spawned
//! thread takes the time-constraint policy, a work interval of our own is
//! created, joined and run through a period, and the performance cores are
//! counted. On other platforms everything reports "not available".

use echlub_rt_thread::{make_realtime, perf_cores, WorkInterval};

/// How long a test waits for a message that must come (a loaded machine
/// can delay it by seconds; the waits that must time out stay short).
const POSITIVE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn a_thread_takes_the_real_time_policy_and_a_work_interval_runs() {
    let period_ns = 256 * 1_000_000_000 / 48_000;
    let (realtime, joined) = std::thread::spawn(move || {
        let rt = make_realtime(period_ns);
        let wi = WorkInterval::new("echlub-rt-thread-test", period_ns);
        let joined = wi.as_ref().is_some_and(|w| {
            w.start();
            w.finish();
            w.joined()
        });
        (rt, joined)
    })
    .join()
    .unwrap();
    if cfg!(target_os = "macos") {
        assert!(realtime, "the time-constraint policy was refused");
        assert!(joined, "no work interval (macOS 12 or later expected)");
        assert!(perf_cores().is_some_and(|n| n > 0));
    } else {
        assert!(!realtime && !joined && perf_cores().is_none());
    }
}

#[test]
fn the_policy_numbers_are_read_back_and_a_bad_request_is_refused() {
    use echlub_rt_thread::{set_time_constraint, time_constraint};
    let (before, set, after, bad) = std::thread::spawn(|| {
        let before = time_constraint();
        let set = set_time_constraint(1_333_333, 333_333, 666_667);
        let after = time_constraint();
        // A computation longer than the constraint is not a policy.
        let bad = set_time_constraint(1_333_333, 900_000, 600_000);
        (before, set, after, bad)
    })
    .join()
    .unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(before, None, "a new thread has the default policy");
        assert_eq!(set, Ok(()));
        let (p, c, k) = after.expect("the policy is read back");
        // Round trips through Mach absolute time: within a microsecond.
        for (got, want) in [(p, 1_333_333u64), (c, 333_333), (k, 666_667)] {
            assert!(got.abs_diff(want) < 1_000, "{got} vs {want}");
        }
        assert!(bad.is_err(), "computation > constraint was accepted");
    } else {
        assert!(before.is_none() && set.is_err() && after.is_none() && bad.is_err());
    }
}

/// The hand-over of `echlub_rt_thread::port` inside one process (the
/// cross-process path is exercised by an out-of-process plugin host that is
/// not part of this repository): a name is
/// checked in, an inbox connects, a hello from another pid is not accepted,
/// a work interval's port is sent, rebuilt and joined on another thread.
#[test]
fn a_workgroup_port_is_handed_over_and_joined() {
    use echlub_rt_thread::port::{send_note, send_port, Inbox, Letter, Rendezvous};
    use echlub_rt_thread::Workgroup;
    use std::time::Duration;
    let service = format!("com.echlub.rt-thread-test.{}", std::process::id());
    let r = Rendezvous::check_in(&service);
    if !cfg!(target_os = "macos") {
        assert!(r.is_err());
        return;
    }
    let r = r.expect("bootstrap_check_in");
    // The same name twice is refused (BOOTSTRAP_NAME_IN_USE or similar).
    assert!(Rendezvous::check_in(&service).is_err());
    let inbox = Inbox::connect(&service).expect("connect");
    // Our own hello, but we say we expect another pid: not accepted.
    let wrong = r.accept(std::process::id() as i32 + 1, Duration::from_millis(100));
    assert_eq!(wrong.unwrap_err().call, "accept-timeout");
    let inbox2 = Inbox::connect(&service).expect("connect again");
    let link = r.accept(std::process::id() as i32, POSITIVE_WAIT).expect("accept");
    drop(r);
    // The name is dead once the rendezvous is gone. launchd drops it
    // asynchronously, so a look-up right after the drop can still succeed
    // under load: wait up to 2 s for it to go.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let gone = loop {
        let call = Inbox::connect(&service).err().map(|e| e.call);
        if call.is_some() || std::time::Instant::now() >= deadline {
            break call;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(gone, Some("bootstrap_look_up"));
    let wi = WorkInterval::new("echlub-rt-thread-port-test", 1_000_000).expect("work interval");
    let port = wi.group.copy_port().expect("os_workgroup_copy_port");
    send_port(&link, &port).expect("send port");
    send_note(&link, "device-lost").expect("send note");
    drop(port);
    // `link` reaches `inbox2` (the first inbox's hello was destroyed).
    assert!(inbox.recv(Duration::from_millis(50)).expect("recv").is_none());
    let got = inbox2.recv(POSITIVE_WAIT).expect("recv");
    let Some(Letter::Port(p)) = got else { panic!("{got:?}") };
    let joined = std::thread::spawn(move || {
        let wg = Workgroup::from_port("echlub-rt-thread-port-test", &p).expect("os_workgroup_create_with_port");
        drop(p);
        let mut t = wg.try_join().expect("os_workgroup_join");
        // Joining a second, unrelated work interval while in the first
        // (printed, not asserted: what nests is the system's rule).
        let other = WorkInterval::new("echlub-rt-thread-port-test-2", 1_000_000);
        wg.leave(&mut t);
        other.map(|o| o.joined())
    })
    .join()
    .unwrap();
    println!("second work interval joined while in the first: {joined:?}");
    match inbox2.recv(POSITIVE_WAIT).expect("recv") {
        Some(Letter::Note(n)) => assert_eq!(n, "device-lost"),
        other => panic!("{other:?}"),
    }
}
