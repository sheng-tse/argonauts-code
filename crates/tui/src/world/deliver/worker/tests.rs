use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use super::*;

/// Polls until `done` holds, for at most a few seconds.
fn wait_for(done: impl Fn() -> bool) {
    let deadline = argonauts_testkit::pty::deadline::after(Duration::from_secs(5));
    while !done() {
        assert!(Instant::now() < deadline, "still waiting");
        std::thread::yield_now();
    }
}

#[test]
fn a_worker_in_line_draws_each_job_as_it_is_asked() {
    let worker = Worker::in_line();
    worker.draw(1, |_| Some("one"));
    worker.draw(2, |_| None);
    assert!(!worker.busy());
    assert_eq!(worker.take(), [(1, "one")]);
    assert!(worker.take().is_empty());
}

#[test]
fn a_threaded_worker_draws_in_the_order_asked_off_the_caller_thread() {
    let worker = Worker::threaded();
    let caller = std::thread::current().id();
    worker.draw(1, move |_| Some(std::thread::current().id() != caller));
    worker.draw(2, |_| Some(true));
    wait_for(|| !worker.busy());
    assert_eq!(worker.take(), [(1, true), (2, true)]);
}

#[test]
fn asking_again_for_a_drawing_under_way_changes_nothing() {
    let worker = Worker::threaded();
    let runs = Arc::new(AtomicUsize::new(0));
    let (release, held) = mpsc::channel::<()>();
    {
        let runs = Arc::clone(&runs);
        worker.draw(7, move |_| {
            runs.fetch_add(1, Ordering::Relaxed);
            held.recv().ok()?;
            Some(())
        });
    }
    for _ in 0..2 {
        let runs = Arc::clone(&runs);
        worker.draw(7, move |_| {
            runs.fetch_add(1, Ordering::Relaxed);
            Some(())
        });
    }
    assert!(worker.busy());
    release.send(()).unwrap();
    wait_for(|| !worker.busy());
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert_eq!(worker.take(), [(7, ())]);
}

#[test]
fn a_stop_ends_the_drawing_under_way_and_the_ones_waiting_never_start() {
    let worker = Worker::threaded();
    let started = Arc::new(AtomicBool::new(false));
    let gave_up = Arc::new(AtomicBool::new(false));
    let waiting_ran = Arc::new(AtomicBool::new(false));
    {
        let (started, gave_up) = (Arc::clone(&started), Arc::clone(&gave_up));
        worker.draw(1, move |cancel| {
            started.store(true, Ordering::Relaxed);
            while !cancel.cancelled() {
                std::thread::yield_now();
            }
            gave_up.store(true, Ordering::Relaxed);
            Some(1)
        });
    }
    {
        let waiting_ran = Arc::clone(&waiting_ran);
        worker.draw(2, move |_| {
            waiting_ran.store(true, Ordering::Relaxed);
            Some(2)
        });
    }
    wait_for(|| started.load(Ordering::Relaxed));
    worker.stop();
    assert!(!worker.busy());
    wait_for(|| gave_up.load(Ordering::Relaxed));
    // A drawing asked after the stop runs, and runs after the ones stopped.
    worker.draw(3, |_| Some(3));
    wait_for(|| !worker.busy());
    assert_eq!(worker.take(), [(3, 3)]);
    assert!(!waiting_ran.load(Ordering::Relaxed));
}

#[test]
fn a_key_stopped_and_asked_again_is_drawn_again() {
    let worker = Worker::threaded();
    let (release, held) = mpsc::channel::<()>();
    worker.draw(5, move |_| {
        held.recv().ok()?;
        Some("stopped")
    });
    worker.stop();
    worker.draw(5, |_| Some("again"));
    // The stopped drawing may never have started, and so never waits for this.
    let _ = release.send(());
    wait_for(|| !worker.busy());
    assert_eq!(worker.take(), [(5, "again")]);
}

#[test]
fn a_picture_being_packed_asks_every_sixteen_rows_whether_to_go_on() {
    let image = crate::base::mascot::png::Rgba::transparent(1280, 546);
    let asked = AtomicUsize::new(0);
    let packed = crate::base::mascot::png::encode_quickly_unless(&image, || {
        asked.fetch_add(1, Ordering::Relaxed);
        false
    });
    assert_eq!(asked.load(Ordering::Relaxed), 546_usize.div_ceil(16));
    let png = packed.expect("a picture");
    let back = crate::base::mascot::png::decode(&png).expect("it reads back");
    assert_eq!((back.width, back.height), (1280, 546));
    let asked = AtomicUsize::new(0);
    let stopped = crate::base::mascot::png::encode_quickly_unless(&image, || {
        asked.fetch_add(1, Ordering::Relaxed) == 3
    });
    assert_eq!(stopped, None);
    assert_eq!(asked.load(Ordering::Relaxed), 4);
}

/// A job that panics ends its own drawing alone: its key is free again, the worker says so,
/// the panic is known for what it is while it unwinds, and the next drawing goes on.
#[test]
fn a_job_that_panics_ends_its_drawing_and_the_next_goes_on() {
    for worker in [Worker::threaded(), Worker::in_line()] {
        worker.draw(1, |_| -> Option<&str> {
            assert!(panicking_in_a_drawing());
            panic!("a drawing that breaks")
        });
        wait_for(|| !worker.busy());
        assert!(worker.failed());
        assert!(!panicking_in_a_drawing());
        worker.draw(1, |_| Some("again"));
        worker.draw(2, |_| Some("after"));
        wait_for(|| !worker.busy());
        assert_eq!(worker.take(), [(1, "again"), (2, "after")]);
    }
}

/// The screen draws no frame to look for a drawing: the worker's thread wakes it once it has
/// finished all it was asked for, the drawings asked for together done by then.
#[test]
fn a_threaded_worker_wakes_the_screen_once_its_drawings_are_done() {
    let mut arrivals = arrivals();
    let worker = Worker::threaded();
    let (release, held) = mpsc::channel::<()>();
    let held = Mutex::new(held);
    worker.draw(1, move |_| {
        let _ = held.lock().map(|held| held.recv());
        Some("one")
    });
    worker.draw(2, |_| Some("two"));
    worker.draw(3, |_| None);
    let _ = release.send(());
    wait_for(|| !worker.busy());
    wait_for(|| arrivals.has_changed().unwrap_or(false));
    arrivals.borrow_and_update();
    assert_eq!(worker.take(), [(1, "one"), (2, "two")]);
}

#[test]
fn a_worker_off_the_frame_draws_in_line_in_tests_unless_asked_for_threads() {
    assert!(in_line_here());
    let worker = Worker::off_the_frame();
    worker.draw(1, |_| Some("one"));
    assert_eq!(worker.take(), [(1, "one")]);
    threads_for_this_test();
    assert!(!in_line_here());
    let worker = Worker::off_the_frame();
    let caller = std::thread::current().id();
    worker.draw(1, move |_| Some(std::thread::current().id() != caller));
    wait_for(|| !worker.busy());
    assert_eq!(worker.take(), [(1, true)]);
}
