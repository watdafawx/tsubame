//! tsubame-bench: fixture generator and directory-enumeration benchmark for
//! Tsubame (see docs/PLAN.md section 2 for the budgets this measures).
//!
//! Subcommands:
//!   gen       create folders of 1k / 100k / 1M mixed-name files
//!   enum      time first-batch and complete listing per enumeration method
//!   verify    cross-check that all four methods return identical entries
//!   coldtest  report whether the standby-list purge is available

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
mod load;
mod ntinfo;
mod stats;
mod store;
mod util;
mod winerr;

fn main() {
    std::process::exit(cli::run());
}
