//! `omt-runtime` library surface (U5b/U5c): shared by the `omt-daemon` and
//! `omt` binaries and by integration suites. Every module here belongs to
//! the daemon composition; the two binaries are thin entries over
//! [`server::run`] and [`cli::run`].

pub mod auth;
pub mod bootstrap;
pub mod cli;
pub mod config;
pub mod descriptor;
pub mod dispatch;
pub mod events;
pub mod homes;
pub mod ipc;
pub mod jsonrpc;
pub mod limits;
pub mod logging;
pub mod ownership;
pub mod paths;
pub mod problem;
pub mod server;
pub mod signal;
pub mod views;
