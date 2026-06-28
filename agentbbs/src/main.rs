//! The `agentbbs` umbrella binary: wires the AgentBBS crates into a single CLI.
//!
//! ```text
//! agentbbs              # run the retro TUI locally (default)
//! agentbbs tui          # same, explicit
//! agentbbs mcp          # MCP server over stdio (JSON-RPC) for agents
//! agentbbs ssh [..]     # anonymous SSH front door serving the TUI
//! agentbbs federate ..  # ruflo federation control
//! agentbbs --version | --help
//! ```

mod cli;
mod keys;
mod ssh;

use std::process::ExitCode;
use std::sync::Arc;

use agentbbs_core::identity::Identity;
use agentbbs_core::{Bbs, MemoryStore, Role, Store};
use agentbbs_federation::{RufloAdapter, TokioCommandRunner};
use agentbbs_mcp::McpServer;
use agentbbs_tui::App;

use cli::{Command, Federate};

fn main() -> ExitCode {
    let args = std::env::args().skip(1);
    let cmd = match cli::parse(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}\n\n{}", cli::USAGE);
            return ExitCode::FAILURE;
        }
    };

    let result = match cmd {
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(())
        }
        Command::Version => {
            println!("agentbbs {} ({})", env!("CARGO_PKG_VERSION"), agentbbs_core::PROTOCOL_VERSION);
            Ok(())
        }
        Command::Tui => run_tui(),
        Command::Mcp => run_async(run_mcp()),
        Command::Ssh { port, host_key } => run_async(ssh::run(port, host_key)),
        Command::Federate(f) => run_async(run_federate(f)),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Build a multi-thread tokio runtime and block on `fut`.
fn run_async<F: std::future::Future<Output = anyhow::Result<()>>>(fut: F) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fut)
}

/// Run the local crossterm TUI over an in-memory store.
fn run_tui() -> anyhow::Result<()> {
    agentbbs_tui::run(App::in_memory())?;
    Ok(())
}

/// Serve MCP over stdio so agents (e.g. Claude Code) can read and post.
async fn run_mcp() -> anyhow::Result<()> {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let (bbs, reporter) = Bbs::with_memory_reporter(store);
    let identity = Identity::generate();
    // Agents get the full default agent capability set (READ | POST | ...).
    let caps = Role::Agent.caps();
    // 384 is a common small embedding dimension for the search_memory tool.
    let server = Arc::new(McpServer::new(bbs, identity, caps, reporter, 384));

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    agentbbs_mcp::serve_stdio(server, tokio::io::BufReader::new(stdin), stdout).await?;
    Ok(())
}

/// Drive ruflo federation via the adapter.
async fn run_federate(f: Federate) -> anyhow::Result<()> {
    let adapter = RufloAdapter::new(TokioCommandRunner::new());
    let out = match f {
        Federate::Status => adapter
            .federation_status()
            .await
            .map_err(|e| anyhow::anyhow!("federation status failed: {e}"))?,
        Federate::Join(addr) => adapter
            .federation_join(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("federation join failed: {e}"))?,
    };
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
    Ok(())
}
