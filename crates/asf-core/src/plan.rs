//! Plan import: markdown task lists → draft issues.
//!
//! Understands the shape produced by common "write a plan" skills (superpowers `writing-plans` and
//! similar): `### Task N: Title`, a `**Files:**` block (`- Create/Modify/Test: path`), checkbox
//! lines as acceptance, `[parallel]` to drop the implicit dependency on the previous task and
//! `**Depends on:** Task 1, Task 2` for explicit ones.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PlanParseError {
    #[error("no tasks found (expected headings like `### Task 1: Title`)")]
    NoTasks,
    #[error("task number {0} appears twice")]
    DuplicateTask(u32),
    #[error("task {task} depends on unknown task {on}")]
    UnknownDependency { task: u32, on: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftTask {
    pub number: u32,
    pub title: String,
    pub body: String,
    pub file_scope: Vec<String>,
    pub acceptance: Vec<String>,
    /// Task numbers this task depends on (explicit, or the implicit previous task).
    pub depends_on: Vec<u32>,
    pub parallel: bool,
    pub estimate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPlan {
    pub title: String,
    pub goal: Option<String>,
    pub tasks: Vec<DraftTask>,
}

fn strip_bold(s: &str) -> &str {
    s.trim().trim_start_matches("**").trim_end_matches("**").trim()
}

/// `### Task 12: Title [parallel]` → (12, "Title", parallel)
fn task_heading(line: &str) -> Option<(u32, String, bool)> {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return None;
    }
    let rest = t.trim_start_matches('#').trim();
    let lower = rest.to_ascii_lowercase();
    let after = lower.strip_prefix("task")?;
    let rest = &rest[rest.len() - after.len()..];
    let rest = rest.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let number: u32 = digits.parse().ok()?;
    let mut title = rest[digits.len()..]
        .trim_start_matches([':', '.', '-', ' ', '\t'])
        .trim()
        .to_string();
    let mut parallel = false;
    for tag in ["[parallel]", "[P]", "[p]"] {
        if title.to_ascii_lowercase().ends_with(&tag.to_ascii_lowercase()) {
            title = title[..title.len() - tag.len()].trim().to_string();
            parallel = true;
        }
    }
    Some((number, title, parallel))
}

fn clean_path(raw: &str) -> Option<String> {
    let p = raw.trim().trim_matches(|c| c == '`' || c == '*').trim();
    if p.is_empty() {
        return None;
    }
    // drop a trailing `:12-34` line range (but not a Windows drive like `C:/`)
    let p = match p.rfind(':') {
        Some(i)
            if i > 1
                && p[i + 1..].chars().all(|c| c.is_ascii_digit() || c == '-')
                && !p[i + 1..].is_empty() =>
        {
            &p[..i]
        }
        _ => p,
    };
    Some(p.replace('\\', "/"))
}

/// Paths from a files line: `- Create: a.rs`, `- Files: a, b`, `Files: a`.
fn paths_in(s: &str) -> Vec<String> {
    let after = match s.find(':') {
        Some(i) => s[i + 1..].trim_start().trim_start_matches("**").trim(),
        None => return Vec::new(),
    };
    after.split(',').filter_map(clean_path).collect()
}

fn numbers_in(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse() {
                out.push(n);
            }
            cur.clear();
        }
    }
    out
}

fn acceptance_text(line: &str) -> String {
    let t = line
        .trim()
        .trim_start_matches("- [ ]")
        .trim_start_matches("- [x]")
        .trim_start_matches("- [X]")
        .trim();
    let t = strip_bold(t);
    // "Step 3: do X" → "do X"
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("step ") {
        if let Some(i) = t.find(':') {
            return t[i + 1..].trim().to_string();
        }
    }
    t.to_string()
}

pub fn parse_md_tasks(md: &str) -> Result<ParsedPlan, PlanParseError> {
    let mut title: Option<String> = None;
    let mut goal: Option<String> = None;
    let mut tasks: Vec<DraftTask> = Vec::new();
    let mut explicit: Vec<Option<Vec<u32>>> = Vec::new();
    let mut files_mode = false;

    for raw in md.lines() {
        let line = raw.trim_end();
        if let Some((number, t, parallel)) = task_heading(line) {
            tasks.push(DraftTask {
                number,
                title: t,
                body: String::new(),
                file_scope: Vec::new(),
                acceptance: Vec::new(),
                depends_on: Vec::new(),
                parallel,
                estimate: None,
            });
            explicit.push(None);
            files_mode = false;
            continue;
        }
        let Some(task) = tasks.last_mut() else {
            // preamble
            if title.is_none() {
                if let Some(h) = line.trim_start().strip_prefix("# ") {
                    title = Some(h.trim().to_string());
                    continue;
                }
            }
            let lower = line.trim().to_ascii_lowercase();
            if lower.starts_with("**goal:**") || lower.starts_with("goal:") {
                let i = line.find(':').unwrap_or(0);
                goal = Some(
                    line[i + 1..]
                        .trim()
                        .trim_start_matches("**")
                        .trim_end_matches("**")
                        .trim()
                        .to_string(),
                );
            }
            continue;
        };
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() {
            files_mode = false;
            task.body.push('\n');
            continue;
        }
        if lower.starts_with("- [ ]") || lower.starts_with("- [x]") {
            files_mode = false;
            task.acceptance.push(acceptance_text(trimmed));
            continue;
        }
        let bare = strip_bold(trimmed.trim_start_matches("- ")).to_ascii_lowercase();
        if bare.starts_with("depends on") {
            let idx = explicit.len() - 1;
            explicit[idx] = Some(numbers_in(trimmed));
            continue;
        }
        if bare.starts_with("files") {
            let inline = paths_in(trimmed);
            if inline.is_empty() {
                files_mode = true;
            } else {
                task.file_scope.extend(inline);
            }
            continue;
        }
        if files_mode && trimmed.starts_with("- ") {
            let item = trimmed.trim_start_matches("- ");
            let lower_item = item.to_ascii_lowercase();
            let paths = if ["create", "modify", "test", "delete", "edit", "add"]
                .iter()
                .any(|k| lower_item.starts_with(k))
            {
                paths_in(item)
            } else {
                clean_path(item).into_iter().collect()
            };
            task.file_scope.extend(paths);
            continue;
        }
        files_mode = false;
        task.body.push_str(trimmed);
        task.body.push('\n');
    }

    if tasks.is_empty() {
        return Err(PlanParseError::NoTasks);
    }
    let mut seen = std::collections::BTreeSet::new();
    for t in &tasks {
        if !seen.insert(t.number) {
            return Err(PlanParseError::DuplicateTask(t.number));
        }
    }
    let numbers: Vec<u32> = tasks.iter().map(|t| t.number).collect();
    for (i, t) in tasks.iter_mut().enumerate() {
        t.depends_on = match &explicit[i] {
            Some(list) => list.clone(),
            None if i > 0 && !t.parallel => vec![numbers[i - 1]],
            None => Vec::new(),
        };
        for on in &t.depends_on {
            if !numbers.contains(on) {
                return Err(PlanParseError::UnknownDependency {
                    task: t.number,
                    on: *on,
                });
            }
        }
        t.body = t.body.trim().to_string();
        for s in &mut t.file_scope {
            *s = s.trim().to_string();
        }
    }
    let title = title.unwrap_or_else(|| tasks[0].title.clone());
    Ok(ParsedPlan { title, goal, tasks })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"# Greeting Feature Implementation Plan

> **For agentic workers:** use the usual skills.

**Goal:** Ship a greet() function with tests.

**Architecture:** one crate.

## Tasks

### Task 1: Scaffold module

**Files:**
- Create: `src/greet.rs`
- Modify: `src/lib.rs:1-10`
- Test: `tests/greet.rs`

- [ ] **Step 1: Write the failing test**
- [ ] **Step 2: Implement `greet(name) -> String`**

Some prose about the task.

### Task 2: Docs [parallel]

**Files:**
- Create: docs/greet.md

- [ ] write the doc page

### Task 3: Release notes

**Depends on:** Task 1, Task 2

- Files: `CHANGELOG.md`, `README.md`
- [ ] changelog entry
"#;

    #[test]
    fn parses_title_goal_and_tasks() {
        let p = parse_md_tasks(PLAN).unwrap();
        assert_eq!(p.title, "Greeting Feature Implementation Plan");
        assert_eq!(p.goal.as_deref(), Some("Ship a greet() function with tests."));
        assert_eq!(p.tasks.len(), 3);
        let t1 = &p.tasks[0];
        assert_eq!(t1.number, 1);
        assert_eq!(t1.title, "Scaffold module");
        assert_eq!(
            t1.file_scope,
            vec!["src/greet.rs", "src/lib.rs", "tests/greet.rs"]
        );
        assert_eq!(
            t1.acceptance,
            vec!["Write the failing test", "Implement `greet(name) -> String`"]
        );
        assert!(t1.body.contains("Some prose about the task."));
        assert!(!t1.parallel);
        assert!(t1.depends_on.is_empty(), "first task has nothing before it");
    }

    #[test]
    fn parallel_tag_and_explicit_deps() {
        let p = parse_md_tasks(PLAN).unwrap();
        let t2 = &p.tasks[1];
        assert_eq!(t2.title, "Docs");
        assert!(t2.parallel);
        assert_eq!(t2.file_scope, vec!["docs/greet.md"]);
        assert!(
            t2.depends_on.is_empty(),
            "[parallel] drops the implicit dependency on task 1"
        );
        let t3 = &p.tasks[2];
        assert_eq!(t3.depends_on, vec![1, 2]);
        assert_eq!(t3.file_scope, vec!["CHANGELOG.md", "README.md"]);
        assert_eq!(t3.acceptance, vec!["changelog entry"]);
    }

    #[test]
    fn implicit_chain_without_tags() {
        let md = "# P\n\n## Task 1: A\n- [ ] a\n\n## Task 2: B\n- [ ] b\n\n## Task 3: C\n- [ ] c\n";
        let p = parse_md_tasks(md).unwrap();
        assert_eq!(p.tasks[1].depends_on, vec![1]);
        assert_eq!(p.tasks[2].depends_on, vec![2]);
    }

    #[test]
    fn errors_without_tasks_or_with_duplicate_numbers_or_unknown_deps() {
        assert!(matches!(
            parse_md_tasks("# Just a title\n\ntext\n").unwrap_err(),
            PlanParseError::NoTasks
        ));
        let dup = "# P\n## Task 1: A\n## Task 1: B\n";
        assert!(matches!(
            parse_md_tasks(dup).unwrap_err(),
            PlanParseError::DuplicateTask(1)
        ));
        let bad = "# P\n## Task 1: A\n**Depends on:** Task 7\n";
        assert!(matches!(
            parse_md_tasks(bad).unwrap_err(),
            PlanParseError::UnknownDependency { task: 1, on: 7 }
        ));
    }

    #[test]
    fn title_falls_back_to_first_task_when_no_h1() {
        let p = parse_md_tasks("## Task 1: Only\n- [ ] x\n").unwrap();
        assert_eq!(p.title, "Only");
        assert!(p.goal.is_none());
    }
}
