//! omt-daemon binary entry (thin): the daemon core lives in the library
//! (`src/server.rs`) so the `omt` CLI binary and integration suites share it.

fn main() {
    omt_runtime::server::run();
}
