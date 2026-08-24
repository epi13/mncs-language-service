fn main() -> std::process::ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(error) = runtime.block_on(mncs_mcp::serve_stdio(mncs_mcp::MncsSemanticServer::new(
        std::sync::Arc::new(mncs_service_core::LanguageService::new(None)),
    ))) {
        eprintln!("mncs-mcp: fatal: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
