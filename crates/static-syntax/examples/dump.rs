fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = if args.len() > 1 {
        std::fs::read_to_string(&args[1]).expect("file")
    } else {
        std::fs::read_to_string("tests/fixtures/bounded-iteration.mncs").expect("fixture")
    };
    let g = mncs_static_syntax::load_grammar().expect("grammar");
    let lines = mncs_static_syntax::tokenize_document(&g, &text).expect("tok");
    let src_lines: Vec<&str> = text.split('\n').collect();
    for (i, toks) in lines.iter().enumerate() {
        println!("== line {i}: {:?}", src_lines.get(i));
        for t in toks {
            println!(
                "   @{}..{} {:?} {}",
                t.start_byte,
                t.start_byte + t.length,
                src_lines.get(i).map_or("", |l| &l
                    [t.start_byte..(t.start_byte + t.length).min(l.len())]),
                t.scopes.last().map(String::as_str).unwrap_or("<unscoped>")
            );
        }
    }
}
