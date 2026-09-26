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
