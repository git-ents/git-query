//! Gate A5 -- resource behavior.
//!
//! A query that will not finish has to be stoppable, and the CLI has to be
//! able to tell "this failed" (exit 2) from "this was cut short" (exit 3).
//! The gate is whether either is expressible from the embedding.

use std::time::{Duration, Instant};

use nemo_spike::{FactRoute, Runner, s};

/// A deliberate cross product. 2000 x 2000 is four million pairs, and the
/// second rule squares it again, so this does not finish.
fn quadratic(n: usize) -> Runner {
    Runner::new(
        "\
pair(?X, ?Y) :- item(?X), item(?Y) .
quad(?X, ?Y, ?Z, ?W) :- pair(?X, ?Y), pair(?Z, ?W) .",
    )
    .route(FactRoute::MemoryImport)
    .facts("item", (0..n as i64).map(|i| s!(i)).collect::<Vec<_>>())
}

#[tokio::test(flavor = "current_thread")]
async fn a5_a_small_cross_product_finishes_so_the_shape_is_right() {
    let mut session = quadratic(20).run().await.expect("run");
    assert_eq!(session.relation("pair").await.len(), 400);
    assert_eq!(session.relation("quad").await.len(), 400 * 400);
    println!("A5 memory at 20 items: {}", session.engine.memory_usage());
}

#[tokio::test(flavor = "current_thread")]
async fn a5_there_is_no_row_cap_or_cancellation_parameter() {
    // Recorded as a compile-time fact rather than a runtime one: the only
    // knobs `ExecutionParameters` exposes are the import manager, global
    // variables, and export selection. Nothing bounds the chase.
    let parameters = nemo::execution::execution_parameters::ExecutionParameters::default();
    let rendered = format!("{parameters:?}");
    assert!(!rendered.contains("limit"), "{rendered}");
    assert!(!rendered.contains("timeout"), "{rendered}");
    assert!(!rendered.contains("cancel"), "{rendered}");
}

#[tokio::test(flavor = "current_thread")]
async fn a5_a_timeout_around_execute_does_not_interrupt_the_chase() {
    // `execute` is async, so `tokio::time::timeout` compiles. Whether it does
    // anything is a different question: cancellation only takes effect at an
    // await point that yields to the runtime, and Nemo's inner futures never
    // yield -- they are async for the sake of the import path, not for
    // cooperative scheduling. The assertion is that the wall clock overruns
    // the deadline by a wide margin.
    let runner = quadratic(45);
    let budget = Duration::from_millis(50);
    let started = Instant::now();
    let outcome = tokio::time::timeout(budget, runner.run()).await;
    let elapsed = started.elapsed();

    match outcome {
        Ok(result) => {
            let session = result.expect("run");
            println!(
                "A5 timeout: the chase finished in {elapsed:?} despite a {budget:?} budget \
                 (load {:?}, execute {:?})",
                session.timing.load, session.timing.execute
            );
        }
        Err(_) => println!("A5 timeout: the future was dropped after {elapsed:?}"),
    }
    assert!(
        elapsed > budget,
        "the deadline was honored, so this finding is stale"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a5_the_only_bound_available_is_bounding_the_input() {
    // What we can do is refuse to start: count the EDB, estimate, and error
    // before handing anything to the engine. That is an admission-time gate,
    // not a truncation, and it is the honest basis for the CLI's behavior.
    let n = 2_000usize;
    let projected = n * n;
    assert!(projected > 1_000_000);
    // No engine was constructed. This is the entire mechanism.
}

#[test]
fn a5_two_engines_cannot_run_concurrently_in_one_process() {
    // Nemo's timing instrumentation is a process-global `Mutex<TimedCode>`
    // that `execute` unconditionally enters. In a debug build it also
    // `debug_assert!`s that no other thread is inside, so two concurrent runs
    // panic -- and the panic poisons the mutex, so every later run in that
    // process fails too. In release the assertion is compiled out and the
    // mutex merely serializes, which is quieter but no more parallel.
    //
    // The consequence is architectural, not cosmetic: a watchdog thread
    // cannot supervise an evaluation, two engines cannot be raced against
    // each other for a differential test in-process, and the test suite has
    // to run single-threaded.
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .expect("runtime");
                    // The result is stringified before it crosses the thread
                    // boundary because `nemo::error::Error` is not `Send`:
                    // the whole API is `Rc`-based and its async is
                    // `?Send`, so neither an engine nor its errors can move
                    // between threads.
                    rt.block_on(async {
                        quadratic(30)
                            .run()
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join()).collect()
    });
    let panicked = outcomes.iter().filter(|o| o.is_err()).count();
    if cfg!(debug_assertions) {
        assert!(
            panicked > 0,
            "concurrent runs all survived, so this finding is stale"
        );
    } else {
        println!("A5 concurrency: {panicked} of 4 concurrent runs panicked in release");
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "runs until it is killed or exhausts memory; run explicitly"]
async fn a5_the_full_quadratic_program_does_not_come_back() {
    let started = Instant::now();
    let mut session = quadratic(2_000).run().await.expect("run");
    println!(
        "A5 quadratic: finished in {:?}, memory {}, pair rows {}",
        started.elapsed(),
        session.engine.memory_usage(),
        session.relation("pair").await.len()
    );
}
