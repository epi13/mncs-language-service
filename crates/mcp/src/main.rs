fn main() -> std::process::ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let service: std::sync::Arc<dyn mncs_service_core::LanguageServiceClient> =
        if let Some(path) = std::env::var_os("MNLS_SERVICE_SOCKET") {
            std::sync::Arc::new(mncs_service_core::RemoteLanguageService::connect_path(path))
        } else {
            std::sync::Arc::new(mncs_service_core::LanguageService::new(None))
        };
    if let Err(error) = runtime.block_on(mncs_mcp::serve_stdio(
        mncs_mcp::MncsSemanticServer::new_client(service),
    )) {
        eprintln!("mncs-mcp: fatal: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
