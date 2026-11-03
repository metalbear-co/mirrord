//! `#[boxed_future]` must keep an `async fn`'s contract: its return type, and every argument
//! living in the future until it completes.

use std::{
    fmt::Debug,
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

#[mirrord_macros::boxed_future]
async fn unsize_tail() -> Box<dyn Debug> {
    Box::new(1)
}

#[mirrord_macros::boxed_future]
async fn unsize_return(early: bool) -> Box<dyn Debug> {
    if early {
        return Box::new("early");
    }
    Box::new(2)
}

#[mirrord_macros::boxed_future]
async fn deref_tail(value: &String) -> &str {
    value
}

/// Each of these failed to compile when the future's output was inferred from the body.
#[test]
fn return_values_coerce_to_the_declared_type() {
    assert_eq!(format!("{:?}", block_on(unsize_tail())), "1");
    assert_eq!(format!("{:?}", block_on(unsize_return(true))), "\"early\"");
    assert_eq!(format!("{:?}", block_on(unsize_return(false))), "2");
    assert_eq!(block_on(deref_tail(&"tail".to_owned())), "tail");
}

static DROPPED: AtomicBool = AtomicBool::new(false);

struct DropFlag;

impl Drop for DropFlag {
    fn drop(&mut self) {
        DROPPED.store(true, Ordering::SeqCst);
    }
}

#[mirrord_macros::boxed_future]
async fn ignores_a_field((value, _): (u8, DropFlag)) -> bool {
    let _ = value;
    DROPPED.load(Ordering::SeqCst)
}

/// A pattern that binds only some fields must not drop the rest before the first poll, the way
/// an ignored `drain::Watch` would end a graceful shutdown early.
#[test]
fn ignored_fields_live_until_the_future_runs() {
    DROPPED.store(false, Ordering::SeqCst);
    let future = ignores_a_field((1, DropFlag));
    assert!(
        !DROPPED.load(Ordering::SeqCst),
        "dropped before the first poll"
    );
    assert!(!block_on(future), "dropped before the body ran");
    assert!(DROPPED.load(Ordering::SeqCst));
}
