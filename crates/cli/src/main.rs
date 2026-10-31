//! `basidekick-sim-cli`: one building on BACnet/IP from a terminal, for
//! servers, CI and anywhere without a display. `--help` lists the options.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", bask_runner::cli::usage());
        return;
    }
    if let Err(e) = bask_runner::cli::run(&args) {
        eprintln!("basidekick-sim-cli: {e}");
        std::process::exit(1);
    }
}
