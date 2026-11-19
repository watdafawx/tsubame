//! tsubame-bench: fixture generator and directory-enumeration benchmark for
//! Tsubame (see docs/PLAN.md section 2 for the budgets this measures).
//!
//! Subcommands:
//!   gen       create folders of 1k / 100k / 1M mixed-name files
//!   gate-fixture  create the leak gate's 200-folder tree
//!   enum      time first-batch and complete listing per enumeration method
//!   verify    cross-check that all four methods return identical entries
//!   coldtest  report whether the standby-list purge is available
//!   purge     flush caches before a cold run (needs elevation)
//!   app       turn one benchmark run's raw reports into a bench-log section

mod appbench;
mod bench;
mod cli;
mod clock;
mod cold;
mod dirhandle;
mod disk;
mod enum_find;
mod enum_gfhie;
mod enum_nt;
mod fixture;
mod gatefixture;
mod load;
mod ntinfo;
mod stats;
mod store;
mod util;
mod winerr;

fn main() {
    std::process::exit(cli::run());
}
