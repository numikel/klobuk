use klobuk_core::method::*;

const GOOD: &str = r#"
[methodology]
name = "klobuk-default"
version = "0.1.0"
language = "en"
description = "Idea -> PRD -> issues via headless Claude."

[[stage]]
id = "idea-to-prd"
title = "Idea to PRD"
kind = "prompt"
prompt = "prompts/idea-to-prd.md"
schema = "schemas/prd.json"
inputs = ["idea.md"]
outputs = ["prd.json"]

[[stage]]
id = "prd-to-issues"
title = "PRD to issues"
kind = "prompt"
prompt = "prompts/prd-to-issues.md"
schema = "schemas/issues.json"
inputs = ["prd.json"]
outputs = ["issues.json"]
parser = "builtin:json-issues"

[issue_defaults]
labels = ["planned"]
priority = 2
"#;

fn all_exist(_: &str) -> bool {
    true
}

#[test]
fn parses_a_full_manifest() {
    let m = parse_manifest(GOOD).unwrap();
    assert_eq!(m.methodology.name, "klobuk-default");
    assert_eq!(m.methodology.language, "en");
    assert_eq!(m.stages.len(), 2);
    assert_eq!(m.stages[0].kind, StageKind::Prompt);
    assert_eq!(m.stages[0].runner, Runner::Headless);
    assert_eq!(m.stages[1].parser, Some(ArtifactParser::JsonIssues));
    assert_eq!(m.issue_defaults.labels, vec!["planned".to_string()]);
    assert_eq!(m.issue_defaults.priority, Some(2));
    assert!(validate(&m, &all_exist).is_empty());
}

#[test]
fn defaults_fill_optional_fields() {
    let m = parse_manifest(
        r#"
[methodology]
name = "x"
[[stage]]
id = "s"
title = "S"
kind = "external"
outputs = ["specs/*/tasks.md"]
parser = "builtin:spec-kit-tasks"
"#,
    )
    .unwrap();
    assert_eq!(m.methodology.language, "en");
    assert_eq!(m.methodology.version, "");
    assert_eq!(m.stages[0].runner, Runner::Interactive);
    assert!(m.stages[0].inputs.is_empty());
    assert!(m.issue_defaults.labels.is_empty());
    assert!(
        validate(&m, &|_| false).is_empty(),
        "external stages reference no files"
    );
}

#[test]
fn parse_errors_are_parse() {
    let e = parse_manifest("[[stage]]\nid = 1\n").unwrap_err();
    assert!(matches!(e, MethodError::Parse(_)), "{e}");
    let e = parse_manifest(
        "[methodology]\nname = \"a\"\n[[stage]]\nid = \"s\"\ntitle = \"S\"\nkind = \"magic\"\n",
    )
    .unwrap_err();
    assert!(matches!(e, MethodError::Parse(_)), "{e}");
}

#[test]
fn validate_reports_each_problem_separately() {
    let m = parse_manifest(
        r#"
[methodology]
name = "Bad Name"

[[stage]]
id = "a"
title = "A"
kind = "prompt"

[[stage]]
id = "a"
title = "A again"
kind = "prompt"
prompt = "../outside.md"
schema = "schemas/missing.json"

[[stage]]
id = "b"
title = "B"
kind = "external"

[[stage]]
id = "c"
title = "C"
kind = "skill"
"#,
    )
    .unwrap();
    let problems = validate(&m, &|p| p == "prompts/ok.md");
    let joined = problems.join("\n");
    assert!(joined.contains("name `Bad Name`"), "{joined}");
    assert!(
        joined.contains("stage `a`: kind `prompt` requires `prompt`"),
        "{joined}"
    );
    assert!(joined.contains("duplicate stage id `a`"), "{joined}");
    assert!(joined.contains("`../outside.md`"), "{joined}");
    assert!(joined.contains("schemas/missing.json"), "{joined}");
    assert!(
        joined.contains("stage `b`: kind `external` requires `outputs`"),
        "{joined}"
    );
    assert!(
        joined.contains("stage `b`: kind `external` requires `parser`"),
        "{joined}"
    );
    assert!(
        joined.contains("stage `c`: kind `skill` requires `skill`"),
        "{joined}"
    );
    assert!(problems.len() >= 8, "{joined}");
}

#[test]
fn validate_requires_at_least_one_stage() {
    let m = parse_manifest("[methodology]\nname = \"empty\"\n").unwrap();
    let problems = validate(&m, &all_exist);
    assert_eq!(problems, vec!["at least one [[stage]] is required".to_string()]);
}

#[test]
fn validate_checks_prompt_and_schema_exist() {
    let m = parse_manifest(GOOD).unwrap();
    let problems = validate(&m, &|p| p != "prompts/prd-to-issues.md");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("prompts/prd-to-issues.md"), "{problems:?}");
    assert!(problems[0].contains("not found"), "{problems:?}");
}
