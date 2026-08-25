//! Grammar loading and structural validation.
//!
//! Validation is strict and fail-closed: anything the tokenizer engine or
//! mainstream TextMate engines handle differently is rejected here rather
//! than silently mis-highlighting. The grammar is authored to a portable
//! subset; this module enforces that subset mechanically.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde_json::Value;

/// A loaded, validated grammar.
pub struct Grammar {
    pub scope_name: String,
    pub file_types: Vec<String>,
    pub first_line_match: Option<String>,
    root_patterns: Vec<String>,
    repository: HashMap<String, Rule>,
}

/// One repository rule (a `match` pattern or a `begin`/`end` region).
pub struct Rule {
    pub name: Option<String>,
    pub body: RuleBody,
}

pub enum RuleBody {
    /// A pure grouping of patterns (`patterns` only); flattened away at
    /// load time into every referencing context.
    Group {
        patterns: Vec<String>,
    },
    Match {
        regex: Compiled,
        captures: Captures,
    },
    BeginEnd(Box<BeginEndData>),
}

/// Payload of a `begin`/`end` region rule (boxed to keep `RuleBody`
/// variants within a reasonable size range).
pub struct BeginEndData {
    pub begin: Compiled,
    pub begin_captures: Captures,
    pub end: Compiled,
    pub end_captures: Captures,
    pub content_name: Option<String>,
    /// Rule names after group flattening; may reference the rule itself for
    /// region recursion.
    pub patterns: Vec<String>,
}

pub type Captures = Vec<(usize, Option<String>)>;

/// A regex compiled once at load time.
pub struct Compiled {
    source: String,
    inner: fancy_regex::Regex,
}

impl Compiled {
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Anchored capture-group spans for a match starting exactly at `pos`.
    /// Index 0 is the whole match; `None` entries are non-participating
    /// groups.
    pub fn captures_at(&self, line: &str, pos: usize) -> Option<Vec<Option<(usize, usize)>>> {
        let m = self.inner.captures_from_pos(line, pos).ok().flatten()?;
        if m.get(0)?.start() != pos {
            return None;
        }
        let mut spans = Vec::with_capacity(m.len());
        for index in 0..m.len() {
            spans.push(m.get(index).map(|g| (g.start(), g.end())));
        }
        Some(spans)
    }
}

/// Grammar validation failure. `Display` output is developer-facing detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for GrammarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "grammar error at `{}`: {}", self.path, self.message)
    }
}

impl std::error::Error for GrammarError {}

fn err(path: impl Into<String>, message: impl Into<String>) -> GrammarError {
    GrammarError {
        path: path.into(),
        message: message.into(),
    }
}

/// Keys allowed inside one rule object. Anything else is rejected so new
/// upstream-style constructs must be consciously supported, not ignored.
const RULE_KEYS: &[&str] = &[
    "name",
    "match",
    "begin",
    "end",
    "captures",
    "beginCaptures",
    "endCaptures",
    "contentName",
    "patterns",
    "include",
];

impl Grammar {
    /// Parse and validate a grammar from JSON text.
    ///
    /// # Errors
    /// See [`GrammarError`].
    pub fn from_json(raw: &str) -> Result<Self, GrammarError> {
        let value: Value =
            serde_json::from_str(raw).map_err(|e| err("$", format!("invalid JSON: {e}")))?;
        Self::from_value(&value)
    }

    /// Parse and validate a grammar from parsed JSON.
    ///
    /// # Errors
    /// See [`GrammarError`].
    pub fn from_value(value: &Value) -> Result<Self, GrammarError> {
        let obj = value
            .as_object()
            .ok_or_else(|| err("$", "grammar must be a JSON object"))?;

        let scope_name = obj
            .get("scopeName")
            .and_then(Value::as_str)
            .ok_or_else(|| err("$.scopeName", "missing string"))?
            .to_owned();

        let file_types = obj
            .get("fileTypes")
            .and_then(Value::as_array)
            .ok_or_else(|| err("$.fileTypes", "missing array"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| err("$.fileTypes[]", "must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let first_line_match = match obj.get("firstLineMatch") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or_else(|| err("$.firstLineMatch", "must be a string"))?
                    .to_owned(),
            ),
        };

        let mut repository = HashMap::new();
        let repo_obj = obj
            .get("repository")
            .and_then(Value::as_object)
            .ok_or_else(|| err("$.repository", "missing object"))?;
        for (key, rule_value) in repo_obj {
            let rule = parse_rule(rule_value, &format!("$.repository.{key}"), &mut repository)?;
            repository.insert(key.clone(), rule);
        }

        let root_patterns = parse_pattern_list(
            obj.get("patterns")
                .ok_or_else(|| err("$.patterns", "missing array"))?,
            "$.patterns",
            &mut repository,
        )?;

        // Every include — root and nested — must resolve.
        let mut all_includes: Vec<(String, String)> = root_patterns
            .iter()
            .map(|name| ("$".to_owned(), name.clone()))
            .collect();
        for (key, rule) in &repository {
            collect_rule_includes(key, rule, &mut all_includes);
        }
        for (from, target) in &all_includes {
            if !repository.contains_key(target) {
                return Err(err(
                    from.clone(),
                    format!("include `#{target}` does not resolve to a repository entry"),
                ));
            }
        }

        // Flatten pure `patterns` groupings into every referencing context so
        // the tokenizer only ever sees match/begin-end rules. Expansion also
        // rejects group-only include cycles (they could never terminate).
        let mut expansion_stack: Vec<String> = Vec::new();
        let mut expanded_lists: HashMap<String, Vec<String>> = HashMap::new();
        for (key, rule) in &repository {
            if let RuleBody::BeginEnd(data) = &rule.body {
                let flat = flatten_list(data.patterns.clone(), &repository, &mut expansion_stack)?;
                expanded_lists.insert(key.clone(), flat);
            }
        }
        let root_patterns = flatten_list(root_patterns, &repository, &mut expansion_stack)?;

        let mut flattened = HashMap::new();
        for (key, rule) in repository {
            let body = match rule.body {
                RuleBody::Group { .. } => continue,
                RuleBody::Match { regex, captures } => RuleBody::Match { regex, captures },
                RuleBody::BeginEnd(mut data) => {
                    data.patterns = expanded_lists.remove(&key).unwrap_or_default();
                    RuleBody::BeginEnd(data)
                }
            };
            flattened.insert(
                key,
                Rule {
                    name: rule.name,
                    body,
                },
            );
        }

        Ok(Self {
            scope_name,
            file_types,
            first_line_match,
            root_patterns,
            repository: flattened,
        })
    }

    pub fn root_patterns(&self) -> &[String] {
        &self.root_patterns
    }

    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.repository.get(name)
    }
}

fn collect_rule_includes(from: &str, rule: &Rule, out: &mut Vec<(String, String)>) {
    match &rule.body {
        RuleBody::Match { .. } | RuleBody::Group { .. } => {}
        RuleBody::BeginEnd(data) => {
            for name in &data.patterns {
                out.push((format!("$.repository.{from}"), name.clone()));
            }
        }
    }
}

/// Expand pure groupings within one pattern list, depth-first. Region
/// recursion (`block-body` ⇄ `statements-and-expressions`) survives because
/// only *group* rules are dissolved; `begin`/`end` rules stay as single list
/// entries and re-enter at runtime through the stack.
fn flatten_list(
    names: Vec<String>,
    repository: &HashMap<String, Rule>,
    stack: &mut Vec<String>,
) -> Result<Vec<String>, GrammarError> {
    let mut flat = Vec::with_capacity(names.len());
    for name in names {
        match repository.get(&name).map(|rule| &rule.body) {
            Some(RuleBody::Group { patterns }) => {
                if stack.contains(&name) {
                    return Err(err(
                        format!("$.repository.{name}"),
                        "include cycle contains no begin/end rule; tokenization would not terminate",
                    ));
                }
                stack.push(name.clone());
                flat.extend(flatten_list(patterns.clone(), repository, stack)?);
                stack.pop();
            }
            Some(_) => flat.push(name),
            None => return Err(err(name, "unresolved include during flattening")),
        }
    }
    Ok(flat)
}

fn parse_pattern_list(
    value: &Value,
    path: &str,
    repository: &mut HashMap<String, Rule>,
) -> Result<Vec<String>, GrammarError> {
    let list = value
        .as_array()
        .ok_or_else(|| err(path, "must be an array"))?;
    let mut names = Vec::with_capacity(list.len());
    for (index, entry) in list.iter().enumerate() {
        let entry_path = format!("{path}[{index}]");
        let obj = entry
            .as_object()
            .ok_or_else(|| err(&entry_path, "pattern entries must be objects"))?;
        if let Some(target) = obj.get("include") {
            if obj.len() != 1 {
                return Err(err(
                    &entry_path,
                    "an `include` entry must not carry other keys",
                ));
            }
            let target = target
                .as_str()
                .ok_or_else(|| err(&entry_path, "`include` must be a string"))?;
            let name = target.strip_prefix('#').ok_or_else(|| {
                err(
                    &entry_path,
                    format!("only `#repository` includes are supported, got `{target}`"),
                )
            })?;
            if name == "$self" || name == "$base" {
                return Err(err(&entry_path, "`$self`/`$base` includes are unsupported"));
            }
            names.push(name.to_owned());
        } else {
            // Inline rule: validate and register under a synthetic key so the
            // tokenizer sees a uniform list of named rules.
            let inline_path = format!("{path}#inline[{index}]");
            let key = format!("@inline.{path}.{index}");
            let rule = parse_rule(entry, &inline_path, repository)?;
            repository.insert(key.clone(), rule);
            names.push(key);
        }
    }
    Ok(names)
}

fn parse_rule(
    value: &Value,
    path: &str,
    repository: &mut HashMap<String, Rule>,
) -> Result<Rule, GrammarError> {
    let obj = value
        .as_object()
        .ok_or_else(|| err(path, "rule must be an object"))?;
    for key in obj.keys() {
        if !RULE_KEYS.contains(&key.as_str()) {
            return Err(err(path, format!("unsupported rule key `{key}`")));
        }
    }
    let name = match obj.get("name") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| err(path, "`name` must be a string"))?
                .to_owned(),
        ),
    };
    let has_match = obj.contains_key("match");
    let has_begin = obj.contains_key("begin");
    let has_end = obj.contains_key("end");
    let has_patterns = obj.contains_key("patterns");

    if has_patterns && !has_match && !has_begin {
        // Pure pattern grouping.
        let patterns =
            parse_pattern_list(&obj["patterns"], &format!("{path}.patterns"), repository)?;
        return Ok(Rule {
            name: None,
            body: RuleBody::Group { patterns },
        });
    }
    if has_patterns && !has_begin {
        return Err(err(path, "`match` rules must not carry `patterns`"));
    }
    if has_match == has_begin {
        return Err(err(
            path,
            "rule must have exactly one of `match`, `begin`(+`end`), or `patterns`",
        ));
    }
    if has_begin && !has_end {
        return Err(err(path, "`begin` requires `end`"));
    }
    if obj.contains_key("captures") && !has_match {
        return Err(err(path, "`captures` requires `match`"));
    }
    if (obj.contains_key("beginCaptures") || obj.contains_key("endCaptures")) && !has_begin {
        return Err(err(path, "begin/end captures require `begin`/`end`"));
    }

    if has_match {
        let source = obj["match"]
            .as_str()
            .ok_or_else(|| err(path, "`match` must be a string"))?;
        reject_unsupported_regex(source, path)?;
        let regex = compile(source, path)?;
        let captures = parse_captures(obj.get("captures"), &format!("{path}.captures"))?;
        Ok(Rule {
            name,
            body: RuleBody::Match { regex, captures },
        })
    } else {
        let begin_source = obj["begin"]
            .as_str()
            .ok_or_else(|| err(path, "`begin` must be a string"))?;
        let end_source = obj["end"]
            .as_str()
            .ok_or_else(|| err(path, "`end` must be a string"))?;
        reject_unsupported_regex(begin_source, path)?;
        reject_unsupported_regex(end_source, path)?;
        let begin = compile(begin_source, path)?;
        let end = compile(end_source, path)?;
        let begin_captures =
            parse_captures(obj.get("beginCaptures"), &format!("{path}.beginCaptures"))?;
        let end_captures = parse_captures(obj.get("endCaptures"), &format!("{path}.endCaptures"))?;
        let content_name = match obj.get("contentName") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or_else(|| err(path, "`contentName` must be a string"))?
                    .to_owned(),
            ),
        };
        let patterns = match obj.get("patterns") {
            None => Vec::new(),
            Some(v) => parse_pattern_list(v, &format!("{path}.patterns"), repository)?,
        };
        Ok(Rule {
            name,
            body: RuleBody::BeginEnd(Box::new(BeginEndData {
                begin,
                begin_captures,
                end,
                end_captures,
                content_name,
                patterns,
            })),
        })
    }
}

fn parse_captures(value: Option<&Value>, path: &str) -> Result<Captures, GrammarError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let obj = value
        .as_object()
        .ok_or_else(|| err(path, "captures must be an object keyed by group number"))?;
    let mut captures = Vec::with_capacity(obj.len());
    for (key, group) in obj {
        let index: usize = key
            .parse()
            .map_err(|_| err(path, format!("capture key `{key}` is not a group number")))?;
        let group_obj = group
            .as_object()
            .ok_or_else(|| err(format!("{path}.{key}"), "capture group must be an object"))?;
        for sub_key in group_obj.keys() {
            if sub_key != "name" {
                return Err(err(
                    format!("{path}.{key}.{sub_key}"),
                    "only capture `name` is supported (`patterns` inside captures is rejected)",
                ));
            }
        }
        let cap_name = group_obj.get("name").and_then(Value::as_str);
        if group_obj.contains_key("name") && cap_name.is_none() {
            return Err(err(format!("{path}.{key}.name"), "must be a string"));
        }
        captures.push((index, cap_name.map(str::to_owned)));
    }
    captures.sort_by_key(|(index, _)| *index);
    Ok(captures)
}

/// Reject constructs outside the portable subset before compilation so the
/// failure names the construct instead of surfacing as a runtime difference.
fn reject_unsupported_regex(source: &str, path: &str) -> Result<(), GrammarError> {
    if fancy_regex::Regex::new(source).is_err() {
        // Let `compile` produce the precise error later.
        return Ok(());
    }
    // Backreferences in `end` patterns would need begin-capture substitution,
    // which the engine intentionally does not implement.
    if fancy_regex::Regex::new(r"\\[0-9]")
        .is_ok_and(|backref| backref.is_match(source).unwrap_or(false))
    {
        return Err(err(
            path,
            "numeric backreferences (\\1…) are outside the supported subset",
        ));
    }
    if source.contains("\\G") {
        return Err(err(path, "\\G is outside the supported subset"));
    }
    Ok(())
}

fn compile(source: &str, path: &str) -> Result<Compiled, GrammarError> {
    let inner = fancy_regex::Regex::new(source)
        .map_err(|e| err(path, format!("regex `{source}` failed to compile: {e}")))?;
    Ok(Compiled {
        source: source.to_owned(),
        inner,
    })
}

/// Convenience: the set of all scope names mentioned by the grammar (rule
/// names, capture names, content names).
pub fn all_scope_names(grammar: &Grammar) -> HashSet<String> {
    let mut scopes = HashSet::new();
    let mut push = |scope: &Option<String>| {
        if let Some(scope) = scope {
            scopes.insert(scope.clone());
        }
    };
    for rule in grammar.repository.values() {
        match &rule.body {
            RuleBody::Group { .. } => {}
            RuleBody::Match { captures, .. } => {
                push(&rule.name);
                for (_, name) in captures {
                    push(name);
                }
            }
            RuleBody::BeginEnd(data) => {
                push(&rule.name);
                push(&data.content_name);
                for (_, name) in data.begin_captures.iter().chain(data.end_captures.iter()) {
                    push(name);
                }
            }
        }
    }
    scopes
}
