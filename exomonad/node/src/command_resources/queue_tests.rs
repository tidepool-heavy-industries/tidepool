use super::*;

fn key(name: &str) -> Key {
    ("run-actor".into(), name.into())
}

#[test]
fn protected_commands_progress_without_bypassing_general_fifo() {
    let mut queue = Queue::new(8, 2, 1);
    queue.push(key("running"), 6);
    assert_eq!(queue.next(), Some((key("running"), 6)));
    queue.push(key("large"), 8);
    queue.push(key("medium"), 2);
    for name in ["small-a", "small-b", "small-c"] {
        queue.push(key(name), 1);
    }
    assert_eq!(queue.next(), Some((key("small-a"), 1)));
    assert_eq!(queue.next(), Some((key("small-b"), 1)));
    assert_eq!(queue.next(), None);
    queue.release(&key("running"));
    assert_eq!(queue.next(), Some((key("large"), 8)));
    assert_eq!(queue.next(), None);
    queue.release(&key("small-a"));
    assert_eq!(queue.next(), Some((key("small-c"), 1)));
    queue.release(&key("large"));
    assert_eq!(queue.next(), Some((key("medium"), 2)));
}

#[test]
fn cancellation_releases_capacity_or_removes_waiting_work_once() {
    let mut queue = Queue::new(8, 0, 1);
    queue.push(key("a"), 8);
    queue.push(key("b"), 8);
    queue.push(key("c"), 8);
    assert_eq!(queue.next(), Some((key("a"), 8)));
    queue.release(&key("b"));
    queue.release(&key("a"));
    queue.release(&key("a"));
    assert_eq!(queue.next(), Some((key("c"), 8)));
    assert_eq!(queue.next(), None);
}

#[test]
fn waiting_reason_tracks_head_follower_protected_bypass_and_release() {
    let mut queue = Queue::new(8, 2, 1);
    queue.push(key("running"), 2);
    assert_eq!(queue.next(), Some((key("running"), 2)));
    queue.push(key("head"), 8);
    queue.push(key("follower"), 4);
    queue.push(key("small"), 1);
    let head = queue.waiting_reason(&key("head")).unwrap();
    assert_eq!(
        (
            head.requested_bytes,
            head.general_total_bytes,
            head.general_used_bytes
        ),
        (8, 8, 2)
    );
    assert_eq!(head.head_requested_bytes, 8);
    assert!(head.head_of_line);
    let follower = queue.waiting_reason(&key("follower")).unwrap();
    assert_eq!(follower.requested_bytes, 4);
    assert!(!follower.head_of_line);
    assert_eq!(follower.head_requested_bytes, 8);
    assert_eq!(queue.next(), Some((key("small"), 1)));
    assert!(queue.waiting_reason(&key("small")).is_none());
    assert_eq!(queue.next(), None);
    queue.release(&key("running"));
    assert_eq!(queue.next(), Some((key("head"), 8)));
    let follower = queue.waiting_reason(&key("follower")).unwrap();
    assert!(follower.head_of_line);
    assert_eq!(follower.head_requested_bytes, 4);
    assert_eq!(follower.general_used_bytes, 8);
    queue.release(&key("head"));
    assert_eq!(queue.next(), Some((key("follower"), 4)));
    assert!(queue.waiting_reason(&key("follower")).is_none());
}
