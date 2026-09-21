//! Resident Language Service host.
//!
//! LSP and MCP adapters connect to this process when `MNLS_SERVICE_SOCKET` is
//! set.  The host owns exactly one `LanguageService` and exposes its bounded
//! JSON-line request/event-cursor surface over a Unix-domain socket.

use std::path::PathBuf;
use std::sync::Arc;

fn main() -> std::process::ExitCode {
    let root = std::env::var_os("MNLS_WORKSPACE_ROOT").map(PathBuf::from);
    let socket = std::env::var_os("MNLS_SERVICE_SOCKET")
        .map(PathBuf::from)
        .or_else(|| {
            root.as_ref()
                .map(|path| path.join(".mncs/mnls-language-service.sock"))
        })
        .unwrap_or_else(|| PathBuf::from(".mncs/mnls-language-service.sock"));
    let service = Arc::new(mncs_service_core::LanguageService::new(root.clone()));
    if let Some(root) = root {
        if let Err(error) = service.configure_root(Some(root)) {
            eprintln!("mnls-language-service-host: workspace unavailable: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }
    eprintln!(
        "mnls-language-service-host: resident service listening at {}",
        socket.display()
    );
    if let Err(error) = mncs_service_core::serve_unix(&socket, service) {
        eprintln!("mnls-language-service-host: fatal: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
