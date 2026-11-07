//! Native check of `aros_startup::minimum_stack!`, run from a Shell with its default stack.
//!
//!   stack-probe size           prints the stack size; status 0 if at least the declared 1 MiB
//!   stack-probe return         uses ~700 KB of stack in a recursion, returns 0
//!   stack-probe exit           calls std::process::exit(7) ~700 KB deep
//!   stack-probe panic          panics ~700 KB deep (status 101 with unwinding, an abort otherwise)
//!   stack-probe thread         runs the recursion on a thread with an 8 MiB stack, joins it, returns 0
//!
//! A Shell's `Echo $RC` after each shows the status; the Shell must still work afterwards
//! (the program's stack is freed and the task's bounds restored).

aros_startup::minimum_stack!(1 << 20);

fn deep(level: u32, action: &str) -> u64 {
    let buf = std::hint::black_box([level as u8; 1024]);
    if level == 0 {
        match action {
            "exit" => std::process::exit(7),
            "panic" => panic!("stack-probe: panic at depth"),
            _ => return buf[0] as u64,
        }
    }
    deep(level - 1, action) + buf[1] as u64
}

fn main() {
    let action = std::env::args().nth(1).unwrap_or_default();
    let size = aros_startup::stack_size().unwrap_or(0);
    println!("stack {size} bytes");
    match action.as_str() {
        "size" => std::process::exit(if size >= 1 << 20 { 0 } else { 10 }),
        "return" | "exit" | "panic" => {
            let r = deep(700, &action);
            println!("returned {r}");
        }
        "thread" => {
            let r = std::thread::Builder::new()
                .name("stack-probe".into())
                .stack_size(8 << 20)
                .spawn(|| deep(700, "return"))
                .expect("spawn")
                .join()
                .expect("join");
            println!("thread returned {r}");
        }
        _ => {
            eprintln!("usage: stack-probe size|return|exit|panic|thread");
            std::process::exit(20);
        }
    }
}
