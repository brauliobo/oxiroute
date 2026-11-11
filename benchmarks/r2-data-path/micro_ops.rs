use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let n: u64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let a = Arc::new(AtomicU64::new(0));
    let m = Arc::new(Mutex::new(0u64));
    let c = Cell::new(0u64);
    let r = Rc::new(5u64);
    let ar = Arc::new(5u64);
    match mode.as_str() {
        "empty" => for i in 0..n { black_box(i); },
        "fetch_add" => for _ in 0..n { black_box(&*a).fetch_add(1, Relaxed); },
        "load_store" => for _ in 0..n { let a = black_box(&*a); a.store(a.load(Relaxed) + 1, Relaxed); },
        "cell" => for _ in 0..n { let c = black_box(&c); c.set(c.get() + 1); },
        "arc_clone_drop" => for _ in 0..n { drop(black_box(&ar).clone()); },
        "rc_clone_drop" => for _ in 0..n { drop(black_box(&r).clone()); },
        "mutex" => for _ in 0..n { *black_box(&*m).lock().unwrap() += 1; },
        "instant" => for _ in 0..n { black_box(Instant::now()); },
        "box648" => for _ in 0..n { let b = Box::new(black_box([7u8; 648])); black_box(&*b); },
        "box_arcstr" => for _ in 0..n { let s: Arc<str> = black_box("backend-1").into(); black_box(&s); },
        _ => panic!(),
    }
}
