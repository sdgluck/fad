//! What the tools view will show, without the view. `cargo run --example toolprobe`
use fad::format::human;

fn main() {
    let t = std::time::Instant::now();
    let report = fad::tools::Report::probe();
    println!("probed in {:?}\n", t.elapsed());
    for s in &report.sources {
        println!("== {} [{:?}]", s.source.label(), s.status);
        for n in s.backing.notes() {
            println!("   ! {n}");
        }
        for (k, size, recl) in &s.totals {
            println!("   {:<14} {:>8} total {:>8} reclaimable", k.label(), human(*size), human(*recl));
        }
        for r in &s.items {
            println!(
                "     {:>8} {:<34} {}{}",
                if r.sized() { human(r.bytes) } else { "?".into() },
                r.name,
                if r.shared() > 0 { format!("(+{} shared) ", human(r.shared())) } else { String::new() },
                r.blocked.clone().map(|b| format!("BLOCKED: {b}")).unwrap_or_default()
            );
        }
    }
    println!("\ncandidates:");
    for k in report.candidates(0) {
        println!("   {}", fad::tools::remove_line(&k));
    }
}
