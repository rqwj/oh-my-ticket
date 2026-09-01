//! `omt` CLI binary entry (thin): the verb surface lives in the library.

fn main() {
    std::process::exit(omt_runtime::cli::real_run_code());
}
