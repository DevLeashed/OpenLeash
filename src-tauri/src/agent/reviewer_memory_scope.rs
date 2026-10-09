//! Proves where project instructions come from: the project the task runs in,
//! NOT the directory the harness binary lives in.
//!
//! The audit claim under test was "AGENTS.md ships into every agent's system
//! prompt in every project". That is only true if the loader walks up from the
//! harness, or from $HOME, or from a fixed path. It does not: `project_memory`
//! joins each name onto the *task's project root*. A project with no AGENTS.md
//! gets no project instructions at all.

use super::prompt::project_memory;

fn temp_project(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ol-mem-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_project_with_no_instructions_file_gets_none() {
    let d = temp_project("bare");
    assert!(project_memory(&d.to_string_lossy(), false).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn instructions_come_from_the_project_root_of_the_task() {
    let d = temp_project("mine");
    std::fs::write(d.join("AGENTS.md"), "PROJECT-SPECIFIC RULE").unwrap();
    let mem = project_memory(&d.to_string_lossy(), false);
    assert_eq!(mem.len(), 1, "exactly the one file: {mem:?}");
    assert_eq!(mem[0].0, "AGENTS.md");
    assert!(mem[0].1.contains("PROJECT-SPECIFIC RULE"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_sibling_project_is_not_read() {
    // Two unrelated projects open at once. One has instructions, one does not.
    let mine = temp_project("with");
    let other = temp_project("without");
    std::fs::write(mine.join("AGENTS.md"), "MINE ONLY").unwrap();

    assert_eq!(project_memory(&mine.to_string_lossy(), false).len(), 1);
    assert!(
        project_memory(&other.to_string_lossy(), false).is_empty(),
        "another project's instructions must not bleed in"
    );
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn an_instructions_file_deeper_in_the_tree_is_not_walked_up_to() {
    // The loader is a fixed list joined to the project root -- it does not
    // search parent directories. A repo nested under a folder that has an
    // AGENTS.md must not inherit it.
    let outer = temp_project("outer");
    let inner = outer.join("packages/app");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(outer.join("AGENTS.md"), "PARENT RULE").unwrap();

    let mem = project_memory(&inner.to_string_lossy(), false);
    assert!(
        mem.is_empty(),
        "no upward search from the project root: {mem:?}"
    );
    let _ = std::fs::remove_dir_all(&outer);
}
