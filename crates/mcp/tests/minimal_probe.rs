use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct Echo {
    text: String,
}

#[derive(Clone)]
struct S {
    router: rmcp::handler::server::router::tool::ToolRouter<S>,
}

#[tool_router]
impl S {
    fn new() -> Self {
        Self {
            router: Self::tool_router(),
        }
    }
    #[tool(description = "echo")]
    async fn echo(&self, Parameters(Echo { text }): Parameters<Echo>) -> String {
        text
    }
}

#[tool_handler(router = self.router.clone())]
impl ServerHandler for S {}

#[tokio::test(flavor = "multi_thread")]
async fn minimal_duplex_works() {
    let (c, s) = tokio::io::duplex(4096);
    let t = tokio::spawn(async move { S::new().serve(s).await });
    let client = ().serve(c).await.expect("client");
    let tools = client.peer().list_tools(None).await.expect("list");
    assert_eq!(tools.tools.len(), 1);
    drop(t);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_server_duplex_works() {
    let service = std::sync::Arc::new(mncs_service_core::LanguageService::new(None));
    let server = mncs_mcp::MncsSemanticServer::new(service);
    let (c, s) = tokio::io::duplex(4096);
    let t = tokio::spawn(async move { server.serve(s).await });
    let client = ().serve(c).await.expect("client");
    let tools = client.peer().list_tools(None).await.expect("list");
    assert!(!tools.tools.is_empty());
    drop(t);
}
