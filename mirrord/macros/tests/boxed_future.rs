//! `#[boxed_future]` must keep an `async fn`'s contract: its return type, and every argument
//! living in the future until it completes.

use std::{
    fmt::Debug,
    pin::pin,
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
