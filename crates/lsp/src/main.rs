fn main() -> std::process::ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(mncs_lsp::run_stdio());
    std::process::ExitCode::SUCCESS
}
