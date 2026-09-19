use std::path::PathBuf;
use std::process::ExitCode;

use mncs_service_core::LanguageService;

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut workspace = None;
    let mut repository = None;
    let mut topic = None;
    let mut symbol = None;
    let mut known_language_identity = None;
    let mut known_architecture_identity = None;
    let mut max_items = 16usize;
    let mut index = 0;
    while index < arguments.len() {
        let name = &arguments[index];
        let value = |index: &mut usize, name: &str| -> Result<String, String> {
            *index += 1;
            arguments
                .get(*index)
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        let result = match name.as_str() {
            "--workspace" => {
                value(&mut index, "--workspace").map(|item| workspace = Some(PathBuf::from(item)))
            }
            "--repository" => value(&mut index, "--repository").map(|item| repository = Some(item)),
            "--topic" => value(&mut index, "--topic").map(|item| topic = Some(item)),
            "--symbol" => value(&mut index, "--symbol").map(|item| symbol = Some(item)),
            "--known-language-identity" => value(&mut index, "--known-language-identity")
                .map(|item| known_language_identity = Some(item)),
            "--known-architecture-identity" => value(&mut index, "--known-architecture-identity")
                .map(|item| known_architecture_identity = Some(item)),
            "--max-items" => value(&mut index, "--max-items")
                .and_then(|item| {
                    item.parse::<usize>()
                        .map_err(|_| "--max-items must be an integer".to_owned())
                })
                .map(|item| max_items = item),
            value if value.starts_with('-') => Err(format!("unknown option {value}")),
            value => Err(format!("unexpected argument {value}")),
        };
        if let Err(error) = result {
            eprintln!("mncs-family-context: {error}");
            return ExitCode::from(2);
        }
        index += 1;
    }
    let service = LanguageService::new(workspace);
    match service.family_agent_context(
        repository.as_deref(),
        topic.as_deref(),
        symbol.as_deref(),
        known_language_identity.as_deref(),
        known_architecture_identity.as_deref(),
        max_items,
    ) {
        Ok(response) => match serde_json::to_string_pretty(&response) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("mncs-family-context: response serialization failed: {error}");
                ExitCode::from(3)
            }
        },
        Err(error) => {
            eprintln!("mncs-family-context: {error}");
            ExitCode::from(4)
        }
    }
}
